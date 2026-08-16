use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::PathBuf;

use cranelift_codegen::ir::types;
use cranelift_codegen::ir::InstBuilder;
use cranelift_codegen::settings::Flags;
use cranelift_codegen::Context;
use cranelift_frontend::FunctionBuilderContext;
use cranelift_object::object::{Object, ObjectSection, ObjectSymbol, RelocationTarget};

use super::{
    box_thunk_value, builtin_opaque_member_return_type, cleanup_place_type,
    collect_type_params_from_type, direct_ffi_type_for_source, direct_field_type, direct_type,
    direct_type_to_type, emit_host_object, emit_host_object_with_metadata, ensure_direct_type,
    enum_variant_payload_types_for_target, infer_operand_type, infer_rvalue_type, infer_try_type,
    infer_variant_payload_type, is_numeric_type_name, main_signature, mangle_default_binder_symbol,
    mangle_symbol, mangle_thunk_symbol, native_codegen_flags, ordered_named_args,
    ordered_optional_named_args, release_direct_call_results, release_direct_values,
    render_direct_type, runtime_type_is_wildcard, signature_for, thunk_signature,
    thunk_string_constant, unbox_thunk_value, validate_function, validate_operand, validate_rvalue,
    validate_tuple_projection_operand, validate_tuple_take_place, DirectType, DirectViewPlace,
    NativeCodegen, PlainClassField, PlainClassType, ScalarKind,
};
use crate::ast::{BinaryOp, UnaryOp};
use crate::diag::Span;
use crate::mir::MirReceiverKind;
use crate::mir::{
    AssertionCapture, BasicBlock, CallTarget, Instruction, MirArg, MirExternCall, MirExternParam,
    MirFormatPart, MirFunction, MirLocalType, MirMapEntry, MirMatchArm, MirParam, Operand, Rvalue,
    Terminator,
};
use crate::sema::Type;
use crate::{lower_path_to_mir, lower_source_to_mir};

#[test]
fn adr0038_local_and_returned_views_compile_through_direct_backend() {
    let module = lower_source_to_mir(
        r#"
class Counter:
    value: int64

def value_mut(counter: mut Counter) -> view mut int64 from counter:
    return view mut counter.value

class Pair:
    left: int64
    right: int64

class Matrix:
    left: Pair
    right: Pair

class Labels:
    left: str
    right: str

def choose(pair: mut Pair, left: bool) -> view mut int64 from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def identity(pair: mut Pair) -> view mut Pair from pair:
    return view mut pair

def choose_pair(matrix: mut Matrix, left: bool) -> view mut Pair from matrix:
    if left:
        return view mut matrix.left
    return view mut matrix.right

def choose_label(labels: mut Labels, left: bool) -> view mut str from labels:
    if left:
        return view mut labels.left
    return view mut labels.right

def main():
    mut counter = Counter(value=1)
    view mut local = counter.value
    local = 2
    view mut returned = value_mut(counter)
    returned = 3
    print(counter.value)
    mut pair = (4, 5)
    view mut tuple_item = pair[1]
    tuple_item = 6
    print(pair)
    mut selected = Pair(left=7, right=8)
    view mut selected_right = choose(selected, false)
    selected_right = 9
    print(selected.right)
    view mut whole = identity(selected)
    whole.left = 10
    mut matrix = Matrix(left=Pair(left=11, right=12), right=Pair(left=13, right=14))
    view mut selected_pair = choose_pair(matrix, false)
    view mut nested_selection = choose(selected_pair, true)
    nested_selection = 15
    print(matrix.right.left)
    mut labels = Labels(left="Ada", right="Grace")
    view mut selected_label = choose_label(labels, false)
    print(selected_label)
    selected_label = "Lin"
    print(labels.right)
"#,
    )
    .expect("view source should lower for direct codegen");
    emit_host_object(&module).expect("direct codegen should preserve view place identity");
}

#[test]
fn adr0038_direct_codegen_rejects_malformed_loan_and_capture_metadata() {
    let closure_source = r#"
def main():
    mut values = [1]
    mut update: def(int64) -> None = lambda [mut values] item: values.append(item)
    update(2)
"#;
    let closure_module = lower_source_to_mir(closure_source)
        .expect("mutable closure capture source should lower for direct validation");
    emit_host_object(&closure_module)
        .expect("valid mutable capture metadata should compile through direct codegen");

    let mutate_capture =
        |module: &mut crate::mir::MirModule,
         update: &mut dyn FnMut(&mut crate::mir::MirClosureCapture)| {
            let capture = module
                .functions
                .iter_mut()
                .flat_map(|function| &mut function.blocks)
                .flat_map(|block| &mut block.instructions)
                .find_map(|instruction| match instruction {
                    Instruction::Assign {
                        value: Rvalue::Closure { captures, .. },
                        ..
                    } => captures
                        .iter_mut()
                        .find(|capture| capture.passing == MirReceiverKind::BorrowMut),
                    _ => None,
                })
                .expect("lowered mutable closure should retain its capture metadata");
            update(capture);
        };

    let mut missing_source = closure_module.clone();
    mutate_capture(&mut missing_source, &mut |capture| {
        capture.source_place = None
    });
    let error = emit_host_object(&missing_source)
        .expect_err("mutable direct captures require an exact source place");
    assert!(
        error.contains("has no source place"),
        "unexpected error: {error}"
    );

    let mut generic_type = closure_module;
    mutate_capture(&mut generic_type, &mut |capture| {
        capture.ty = Type::TypeParam("Unresolved".to_string())
    });
    emit_host_object(&generic_type)
        .expect("direct closure metadata retains unresolved generic captures as opaque values");

    let mut escaped = lower_source_to_mir(
        r#"
class Pair:
    left: int64
    right: int64

def invalid(pair: mut Pair, other: mut Pair) -> view mut int64 from pair:
    return view mut pair.left
"#,
    )
    .expect("valid returned-view source should lower before metadata corruption");
    let return_loan = escaped
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::ReturnLoan { origin, .. } => Some(origin),
            _ => None,
        })
        .expect("returned view should emit ReturnLoan metadata");
    *return_loan = "other".to_string();
    let error = emit_host_object(&escaped)
        .expect_err("direct returned loans must remain within their declared origin");
    assert!(
        error.contains("has no projection within origin `other`"),
        "unexpected error: {error}"
    );
}

#[test]
fn adr0038_direct_view_place_projection_handles_root_and_nested_paths() {
    let root = DirectViewPlace::static_place("origin".to_string());
    assert_eq!(root.clone().project("").alternatives[0].place, "origin");
    assert_eq!(
        root.project("left.value").alternatives[0].place,
        "origin.left.value"
    );
}

fn test_function_operand(name: &str, params: Vec<Type>, return_type: Type) -> Operand {
    Operand::Function {
        name: name.to_string(),
        signature: Box::new(Type::Function {
            params: params
                .into_iter()
                .map(|ty| crate::sema::FunctionParamContract {
                    name: String::new(),
                    ty,
                    passing: crate::ast::ReceiverKind::Value,
                    has_default: false,
                    default_erased: false,
                })
                .collect(),
            return_type: Box::new(return_type),
        }),
    }
}

fn scalar_kind_for_tests(ty: &Type) -> Option<ScalarKind> {
    direct_type(ty, &HashMap::new()).and_then(|ty| ty.scalar_kind())
}

#[test]
fn direct_array_surface_relocates_only_to_dedicated_native_array_kernels() {
    let source = r#"
def double(value: int32) -> float64:
    return value.to_float() * 2.0

def main():
    mut values = Array[int32].from_list(values=[1, 2, 3, 4], shape=[2, 2])
    copied = values.clone()
    values.set(index=[0, 1], value=9)
    values.fill(value=2)
    item = values[0, 1]
    values[0, 1] = item
    rows = values[:1]
    added = values + copied
    scaled = 3 - values
    wrapped = values.wrapping_add(rhs=copied)
    saturated = values.saturating_mul(rhs=2)
    mapped = values.map(double)
    shape = values.shape()
    count = values.len()
    total = values.sum()
    minimum = values.min()
    maximum = values.max()
    average = values.mean()
    print(rows)
    print(added)
    print(scaled)
    print(wrapped)
    print(saturated)
    print(mapped)
    print(shape)
    print(count)
    print(total)
    print(minimum)
    print(maximum)
    print(average)
"#;
    let mir = lower_source_to_mir(source).expect("Array surface should lower to MIR");
    let object = emit_host_object(&mir).expect("Array surface should emit direct native code");
    let referenced = object_referenced_symbols(&object);
    for required in [
        "aura_direct_array_from_vec",
        "aura_direct_array_clone",
        "aura_direct_array_shape",
        "aura_direct_array_len",
        "aura_direct_array_set_in_place",
        "aura_direct_array_fill_in_place",
        "aura_direct_array_index",
        "aura_direct_array_set_index_in_place",
        "aura_direct_array_slice",
        "aura_direct_array_binary",
        "aura_direct_array_map",
        "aura_direct_array_reduce",
    ] {
        assert!(
            referenced.iter().any(|symbol| symbol.contains(required)),
            "Array lowering should reference `{required}`: {referenced:?}"
        );
    }
    assert!(
        referenced
            .iter()
            .all(|symbol| !symbol.contains("aura_direct_binary_value_at")),
        "Array operators must not fall back to the generic boxed binary helper: {referenced:?}"
    );
}

#[test]
fn direct_fixed_width_integer_methods_relocate_to_the_width_arithmetic_kernel() {
    let source = r#"
def main():
    signed: int32 = 2147483647
    signed_rhs: int32 = 2
    unsigned: uint8 = 255
    unsigned_rhs: uint8 = 1
    print(signed.wrapping_add(signed_rhs))
    print(signed.wrapping_sub(signed_rhs))
    print(signed.wrapping_mul(signed_rhs))
    print(signed.saturating_add(signed_rhs))
    print(signed.saturating_sub(signed_rhs))
    print(signed.saturating_mul(signed_rhs))
    print(unsigned.wrapping_add(unsigned_rhs))
    print(unsigned.saturating_sub(unsigned_rhs))
    print(signed.wrapping_shl(signed_rhs))
    print(signed.wrapping_shr(signed_rhs))
    print(signed.saturating_shl(signed_rhs))
    print(signed.saturating_shr(signed_rhs))
"#;
    let mir = lower_source_to_mir(source).expect("fixed-width methods should lower to MIR");
    let object =
        emit_host_object(&mir).expect("fixed-width methods should emit direct native code");
    let referenced = object_referenced_symbols(&object);

    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_integer_width_binary")),
        "fixed-width methods should reference the typed width-arithmetic kernel: {referenced:?}"
    );
}

#[test]
fn direct_dynamic_binary_opcodes_match_the_runtime_abi() {
    for (operator, opcode) in [
        (BinaryOp::Add, 0),
        (BinaryOp::Sub, 1),
        (BinaryOp::Mul, 2),
        (BinaryOp::Div, 3),
        (BinaryOp::Mod, 4),
        (BinaryOp::Eq, 5),
        (BinaryOp::NotEq, 6),
        (BinaryOp::Less, 7),
        (BinaryOp::LessEq, 8),
        (BinaryOp::Greater, 9),
        (BinaryOp::GreaterEq, 10),
        (BinaryOp::And, 11),
        (BinaryOp::Or, 12),
        (BinaryOp::FloorDiv, 13),
        (BinaryOp::Pow, 14),
        (BinaryOp::BitAnd, 15),
        (BinaryOp::BitOr, 16),
        (BinaryOp::BitXor, 17),
        (BinaryOp::Shl, 18),
        (BinaryOp::Shr, 19),
    ] {
        assert_eq!(
            super::FunctionCompiler::binary_opcode(operator),
            opcode,
            "{operator:?} must retain the opcode consumed by aura_direct_binary_value_at",
        );
    }
}

#[test]
fn tuple_native_ownership_gates_separate_public_projection_from_private_destructuring() {
    assert!(validate_tuple_projection_operand(&Operand::Place("pair".to_string())).is_ok());
    let public_move = validate_tuple_projection_operand(&Operand::MovePlace("pair".to_string()))
        .expect_err("public tuple indexing must never consume a source projection");
    assert!(public_move.contains("only reads Copy elements"));

    assert!(validate_tuple_take_place("%t17").is_ok());
    let user_place = validate_tuple_take_place("pair")
        .expect_err("destructive extraction must be scoped to a private captured tuple");
    assert!(user_place.contains("whole-tuple destructuring"));

    let classes = HashMap::new();
    let tuple_type = Type::Tuple(vec![Type::named("int64"), Type::named("str")]);
    assert_eq!(
        direct_type(&tuple_type, &classes),
        Some(DirectType::Opaque(tuple_type.clone())),
        "structural tuple types use the opaque aggregate ABI"
    );
    assert!(validate_rvalue(
        &Rvalue::TupleLiteral {
            elements: vec![Operand::Int(1), Operand::String("one".to_string())],
            element_types: vec![Type::named("int64"), Type::named("str")],
        },
        &classes,
    )
    .is_ok());
    let arity_error = validate_rvalue(
        &Rvalue::TupleLiteral {
            elements: vec![Operand::Int(1)],
            element_types: vec![Type::named("int64"), Type::named("str")],
        },
        &classes,
    )
    .expect_err("tuple literal MIR metadata must preserve source arity");
    assert!(arity_error.contains("tuple literal arity 1 with 2 element types"));

    let projection_error = validate_rvalue(
        &Rvalue::TupleElement {
            tuple: Operand::MovePlace("pair".to_string()),
            index: 0,
            element_type: Type::named("int64"),
        },
        &classes,
    )
    .expect_err("public tuple projection cannot consume its source");
    assert!(projection_error.contains("only reads Copy elements"));
    assert!(validate_rvalue(
        &Rvalue::TupleElement {
            tuple: Operand::Place("pair".to_string()),
            index: 0,
            element_type: Type::named("int64"),
        },
        &classes,
    )
    .is_ok());

    let take_error = validate_rvalue(
        &Rvalue::TupleTakeElement {
            place: "pair".to_string(),
            index: 1,
            element_type: Type::named("str"),
        },
        &classes,
    )
    .expect_err("destructive extraction cannot name a user-visible place");
    assert!(take_error.contains("private captured temporary"));
    assert!(validate_rvalue(
        &Rvalue::TupleTakeElement {
            place: "%t4".to_string(),
            index: 1,
            element_type: Type::named("str"),
        },
        &classes,
    )
    .is_ok());

    assert!(!runtime_type_is_wildcard(&tuple_type));
    assert!(runtime_type_is_wildcard(&Type::Tuple(vec![
        Type::TypeParam("Element".to_string())
    ])));
    for ty in [Type::Tuple(vec![Type::named("int64")]), tuple_type.clone()] {
        let encoded = crate::native_runtime::canonical_runtime_type_name(&ty);
        let payload = encoded
            .strip_prefix("__aura_type_json_v1__:")
            .expect("runtime structural types use the canonical tagged encoding");
        assert_eq!(
            serde_json::from_str::<Type>(payload)
                .expect("canonical runtime type payload should decode"),
            ty
        );
    }
}

fn object_referenced_symbols(bytes: &[u8]) -> BTreeSet<String> {
    let object = cranelift_object::object::File::parse(bytes)
        .expect("direct backend output should be a readable host object");
    object
        .sections()
        .flat_map(|section| section.relocations())
        .filter_map(|(_, relocation)| match relocation.target() {
            RelocationTarget::Symbol(index) => object.symbol_by_index(index).ok(),
            _ => None,
        })
        .filter_map(|symbol| symbol.name().ok().map(str::to_string))
        .collect()
}

fn object_function_referenced_symbols(bytes: &[u8], function: &str) -> BTreeSet<String> {
    let object = cranelift_object::object::File::parse(bytes)
        .expect("direct backend output should be a readable host object");
    let symbol = object
        .symbols()
        .find(|symbol| {
            symbol
                .name()
                .is_ok_and(|name| name.trim_start_matches('_') == function)
        })
        .unwrap_or_else(|| panic!("direct object should define `{function}`"));
    let section_index = symbol
        .section_index()
        .unwrap_or_else(|| panic!("`{function}` should belong to an object section"));
    let start = symbol.address();
    let section = object
        .section_by_index(section_index)
        .expect("function section should exist");
    let start = start.saturating_sub(section.address());
    let end = if symbol.size() > 0 {
        start.saturating_add(symbol.size())
    } else {
        object
            .symbols()
            .filter(|candidate| candidate.section_index() == Some(section_index))
            .map(|candidate| candidate.address().saturating_sub(section.address()))
            .filter(|address| *address > start)
            .min()
            .unwrap_or(section.size())
    };
    section
        .relocations()
        .filter(|(offset, _)| *offset >= start && *offset < end)
        .filter_map(|(_, relocation)| match relocation.target() {
            RelocationTarget::Symbol(index) => object.symbol_by_index(index).ok(),
            _ => None,
        })
        .filter_map(|symbol| symbol.name().ok().map(str::to_string))
        .collect()
}

fn object_referenced_symbol_occurrences(bytes: &[u8], needle: &str) -> usize {
    let object = cranelift_object::object::File::parse(bytes)
        .expect("direct backend output should be a readable host object");
    object
        .sections()
        .flat_map(|section| section.relocations())
        .filter_map(|(_, relocation)| match relocation.target() {
            RelocationTarget::Symbol(index) => object.symbol_by_index(index).ok(),
            _ => None,
        })
        .filter_map(|symbol| symbol.name().ok())
        .filter(|name| name.contains(needle))
        .count()
}

#[test]
fn heterogeneous_match_arm_mutable_locals_keep_distinct_direct_slots() {
    let source =
        include_str!("../tests/fixtures/run-pass/match_arm_local_binding_slot_isolation.au");
    let module = lower_source_to_mir(source)
        .expect("heterogeneous sibling match-arm locals should lower to MIR");
    let mir_output =
        crate::run_mir(&module).expect("the MIR backend should execute both match arms");
    assert_eq!(mir_output.stdout, "Ada\n42\n");

    emit_host_object(&module)
        .expect("the direct backend should preserve each arm-local binding's own type");

    let describe = module
        .functions
        .iter()
        .find(|function| function.name == "describe")
        .expect("describe should lower");
    let arm_local_types = describe
        .local_types
        .iter()
        .filter(|local| local.name.starts_with("%t"))
        .filter_map(|local| match &local.ty {
            Type::Named(name, _) if matches!(name.as_str(), "Person" | "Score") => {
                Some((local.name.as_str(), name.as_str()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(arm_local_types.iter().any(|(_, ty)| *ty == "Person"));
    assert!(arm_local_types.iter().any(|(_, ty)| *ty == "Score"));
    assert!(
        arm_local_types.iter().all(|(slot, _)| *slot != "item"),
        "arm-local source names must be rewritten to typed MIR slots"
    );
}

#[test]
fn tuple_native_symbols_keep_public_projection_separate_from_private_take() {
    let tuple_type = Type::Tuple(vec![Type::named("int64"), Type::named("str")]);
    let module = |instructions, local_types| crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types,
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions,
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    let public_projection = module(
        vec![
            Instruction::Assign {
                target: "pair".to_string(),
                value: Rvalue::TupleLiteral {
                    elements: vec![Operand::Int(7), Operand::String("seven".to_string())],
                    element_types: vec![Type::named("int64"), Type::named("str")],
                },
            },
            Instruction::Assign {
                target: "first".to_string(),
                value: Rvalue::TupleElement {
                    tuple: Operand::Place("pair".to_string()),
                    index: 0,
                    element_type: Type::named("int64"),
                },
            },
        ],
        vec![
            MirLocalType {
                name: "pair".to_string(),
                ty: tuple_type.clone(),
            },
            MirLocalType {
                name: "first".to_string(),
                ty: Type::named("int64"),
            },
        ],
    );
    let public_object =
        emit_host_object(&public_projection).expect("Copy tuple projection should emit directly");
    let public_symbols = object_referenced_symbols(&public_object);
    for required in ["aura_direct_tuple_new", "aura_direct_tuple_element"] {
        assert!(
            public_symbols
                .iter()
                .any(|symbol| symbol.contains(required)),
            "public tuple projection should reference `{required}`: {public_symbols:?}"
        );
    }
    assert!(
        public_symbols
            .iter()
            .all(|symbol| !symbol.contains("aura_direct_tuple_take_element")),
        "public tuple projection must not reference destructive take: {public_symbols:?}"
    );

    let private_take = module(
        vec![
            Instruction::Assign {
                target: "pair".to_string(),
                value: Rvalue::TupleLiteral {
                    elements: vec![Operand::Int(7), Operand::String("seven".to_string())],
                    element_types: vec![Type::named("int64"), Type::named("str")],
                },
            },
            Instruction::Assign {
                target: "%t0".to_string(),
                value: Rvalue::Use(Operand::MovePlace("pair".to_string())),
            },
            Instruction::Assign {
                target: "label".to_string(),
                value: Rvalue::TupleTakeElement {
                    place: "%t0".to_string(),
                    index: 1,
                    element_type: Type::named("str"),
                },
            },
        ],
        vec![
            MirLocalType {
                name: "pair".to_string(),
                ty: tuple_type.clone(),
            },
            MirLocalType {
                name: "%t0".to_string(),
                ty: tuple_type,
            },
            MirLocalType {
                name: "label".to_string(),
                ty: Type::named("str"),
            },
        ],
    );
    let private_object =
        emit_host_object(&private_take).expect("private captured tuple take should emit directly");
    let private_symbols = object_referenced_symbols(&private_object);
    assert!(
        private_symbols
            .iter()
            .any(|symbol| symbol.contains("aura_direct_tuple_take_element")),
        "private tuple destruction should reference destructive take: {private_symbols:?}"
    );
}

#[test]
fn tuple_specialized_trait_dispatch_emits_structural_runtime_matchers() {
    let source = r#"
trait Label:
    def label(self) -> str

class Envelope[T]:
    payload: T

impl Label for Envelope[(int32, str)]:
    def label(self) -> str:
        return "integer then string"

impl Label for Envelope[(str, int32)]:
    def label(self) -> str:
        return "string then integer"

def describe[T: Label](value: T) -> str:
    return value.label()

def main():
    print(describe(Envelope[(int32, str)](payload=(7, "seven"))))
    print(describe(Envelope[(str, int32)](payload=("eight", 8))))
"#;

    let mir = lower_source_to_mir(source).expect("tuple-specialized dispatch should lower");
    let output = crate::run_mir(&mir).expect("tuple-specialized dispatch should run through MIR");
    assert_eq!(output.stdout, "integer then string\nstring then integer\n");

    let bytes =
        emit_host_object(&mir).expect("tuple-specialized dispatch should emit direct object code");
    let referenced = object_referenced_symbols(&bytes);
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_value_type_matches")),
        "dynamic tuple specialization must consult runtime type matching: {referenced:?}"
    );

    let object = cranelift_object::object::File::parse(bytes.as_slice())
        .expect("direct tuple-dispatch output should be a readable host object");
    let data_occurrences = |needle: &[u8]| {
        object
            .sections()
            .filter_map(|section| section.data().ok())
            .map(|data| {
                data.windows(needle.len())
                    .filter(|window| *window == needle)
                    .count()
            })
            .sum::<usize>()
    };
    let tuple_payload = |elements| {
        crate::native_runtime::canonical_runtime_type_name(&Type::Tuple(elements))
            .strip_prefix("__aura_type_json_v1__:")
            .expect("canonical structural runtime type tag")
            .to_string()
    };
    let int_then_string = tuple_payload(vec![Type::named("int32"), Type::named("str")]);
    let string_then_int = tuple_payload(vec![Type::named("str"), Type::named("int32")]);
    assert!(
        data_occurrences(int_then_string.as_bytes()) >= 2,
        "direct dispatch must encode a tuple matcher in addition to the enclosing class pattern"
    );
    assert!(
        data_occurrences(string_then_int.as_bytes()) >= 2,
        "direct dispatch must encode a second tuple matcher in addition to its enclosing class pattern"
    );
}

#[test]
fn d3_native_unhinted_integer_operands_use_the_unboxed_int64_path() {
    assert_eq!(
        infer_operand_type(&Operand::Int(7), &HashMap::new(), &HashMap::new()),
        Some(DirectType::Scalar(ScalarKind::Int64))
    );

    let source = r#"
def identity(value: int64) -> int64:
    return value

def main() -> int32:
    value = 2147483648
    print(identity(value))
    return 0
"#;
    let mir = lower_source_to_mir(source).expect("default int64 source should lower to MIR");
    let object = emit_host_object(&mir).expect("default int64 source should compile directly");
    let referenced = object_referenced_symbols(&object);

    for forbidden in [
        "aura_direct_box_uint_literal",
        "aura_direct_binary_value_at",
        "aura_direct_cast_value_at",
        "aura_direct_unbox_i64",
    ] {
        assert!(
            !referenced.iter().any(|symbol| symbol.contains(forbidden)),
            "default int64 values must not reference `{forbidden}`: {referenced:?}"
        );
    }
}

#[test]
fn direct_duration_api_uses_centralized_constructor_and_conversion_dispatch() {
    let source = r#"
def main() -> int32:
    base = Duration.ms(125)
    seconds = Duration.seconds(2)
    minutes = Duration.minutes(-1)
    combined = base + seconds
    scaled = 3 * combined
    reverse_scaled = combined * 3
    print(scaled // 2)
    print(reverse_scaled)
    print(minutes < 0ms)
    print(combined.to_ms())
    print(combined.to_seconds())
    return 0
"#;
    let mir = lower_source_to_mir(source).expect("Duration API source should lower to MIR");
    let object = emit_host_object(&mir).expect("Duration API should compile directly");
    let referenced = object_referenced_symbols(&object);

    for required in [
        "aura_direct_duration_from_i64",
        "aura_direct_duration_to_float",
        "aura_direct_binary_value_at",
    ] {
        assert!(
            referenced.iter().any(|symbol| symbol.contains(required)),
            "Duration direct code should reference `{required}`: {referenced:?}"
        );
    }
}

#[test]
fn direct_select_packages_variadic_sources_into_the_owned_tuple_abi() {
    let module = module_with_main_call_result_type(
        Rvalue::Call {
            callee: CallTarget::Name("select".to_string()),
            args: vec![MirArg {
                name: None,
                value: Operand::Duration(0),
                writeback_place: None,
            }],
        },
        Type::Named("SelectOutcome".to_string(), vec![Type::Unit, Type::Unit]),
    );
    let object = emit_host_object(&module)
        .expect("typed select should package direct sources into an owned tuple");
    let referenced = object_referenced_symbols(&object);
    for required in [
        "aura_direct_duration_literal",
        "aura_direct_arg_buffer_new",
        "aura_direct_arg_buffer_store_owned",
        "aura_direct_tuple_new",
        "aura_direct_select",
        "aura_direct_release_value",
    ] {
        assert!(
            referenced.iter().any(|symbol| symbol.contains(required)),
            "direct select code should reference `{required}`: {referenced:?}"
        );
    }
    assert!(
        referenced
            .iter()
            .all(|symbol| !symbol.contains("aura_direct_host_builtin")),
        "select must use its internal tuple ABI rather than generic host dispatch: {referenced:?}"
    );

    let nonrepeatable_task_source = r#"
def worker() -> str:
    return "value"

def main() -> int32:
    with TaskGroup() as group:
        task = group.start(worker)
        outcome = select(task, 0ms)
    return 0
"#;
    let mir = lower_source_to_mir(nonrepeatable_task_source)
        .expect("a nonrepeatable Task select source should lower");
    let object = emit_host_object(&mir)
        .expect("the direct select tuple must accept a moved nonrepeatable Task source");
    assert!(
        object_referenced_symbols(&object)
            .iter()
            .any(|symbol| symbol.contains("aura_direct_select")),
        "source-level nonrepeatable Task selection must use the direct select ABI"
    );
}

#[test]
fn direct_select_inference_preserves_queue_and_task_payload_types() {
    let variable_types = HashMap::from([
        (
            "messages".to_string(),
            DirectType::Opaque(Type::Named("Queue".to_string(), vec![Type::named("str")])),
        ),
        (
            "worker".to_string(),
            DirectType::Opaque(Type::Named("Task".to_string(), vec![Type::named("int32")])),
        ),
    ]);
    let select_call = Rvalue::Call {
        callee: CallTarget::Name("select".to_string()),
        args: vec![
            MirArg {
                name: None,
                value: Operand::Place("messages".to_string()),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Place("worker".to_string()),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Duration(0),
                writeback_place: None,
            },
        ],
    };
    assert_eq!(
        infer_rvalue_type(
            &select_call,
            &variable_types,
            &HashMap::new(),
            &HashMap::new(),
        ),
        Some(DirectType::Opaque(Type::Named(
            "SelectOutcome".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        ))),
        "native result inference must preserve the homogeneous payload type of each source family"
    );
}

#[test]
fn direct_random_api_uses_dedicated_borrowed_runtime_symbols() {
    let source = r#"
import random

class Item:
    label: str

class Holder:
    values: list[Item]

def main() -> int32:
    mut rng = random.Rng(seed=42)
    print(rng.next_int(lo=0, hi=10))
    print(rng.next_float())
    mut holder = Holder([Item("a"), Item("b"), Item("c")])
    rng.shuffle(values=holder.values)
    print(random.secure_int(5, 6))
    print(random.secure_bytes(0).len())
    return 0
"#;
    let mir = lower_source_to_mir(source).expect("Randomness API source should lower to MIR");
    let object = emit_host_object(&mir).expect("Randomness API should compile directly");
    let referenced = object_referenced_symbols(&object);

    for required in [
        "aura_direct_rng_new",
        "aura_direct_rng_next_int",
        "aura_direct_rng_next_float",
        "aura_direct_rng_shuffle",
        "aura_direct_random_secure_int",
        "aura_direct_random_secure_bytes",
    ] {
        assert!(
            referenced.iter().any(|symbol| symbol.contains(required)),
            "Randomness direct code should reference `{required}`: {referenced:?}"
        );
    }
}

#[test]
fn direct_opaque_user_clone_dispatches_to_the_declared_trait_method() {
    let source = include_str!("../tests/fixtures/run-pass/random_opaque_user_clone_dispatch.au");
    let mir = lower_source_to_mir(source).expect("opaque Holder clone source should lower to MIR");
    let object = emit_host_object(&mir).expect("opaque Holder clone should compile directly");
    let referenced = object_referenced_symbols(&object);

    assert!(
        referenced
            .iter()
            .all(|symbol| !symbol.contains("aura_direct_clone_value")),
        "the user-declared clone method must not fall through to opaque runtime cloning: {referenced:?}"
    );
}

#[test]
fn direct_rng_nonbuiltin_trait_dispatch_preserves_builtin_member_dispatch() {
    let source = include_str!("../tests/fixtures/run-pass/random_rng_trait_dispatch.au");
    let mir = lower_source_to_mir(source).expect("Rng trait source should lower to MIR");
    let object = emit_host_object(&mir).expect("Rng trait source should compile directly");
    let referenced = object_referenced_symbols(&object);

    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_rng_next_int")),
        "the builtin Rng.next_int call must retain its dedicated runtime dispatch: {referenced:?}"
    );
}

#[test]
fn direct_user_defined_rng_does_not_reference_random_runtime_symbols() {
    let source = r#"
class Rng:
    value: int64

    def next_int(self, lo: int64, hi: int64) -> int64:
        return self.value

    def next_float(self) -> str:
        return "local"

    def shuffle(self, value: int64) -> int64:
        return value

def main() -> int32:
    rng = Rng(5)
    print(rng.next_int(0, 10))
    print(rng.next_float())
    print(rng.shuffle(7))
    return 0
"#;
    let mir = lower_source_to_mir(source).expect("the local Rng class should lower to MIR");
    let object = emit_host_object(&mir).expect("the local Rng class should compile directly");
    let referenced = object_referenced_symbols(&object);

    assert!(
        !referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_rng_")),
        "a user class named Rng must not reference the random.Rng runtime: {referenced:?}"
    );
}

#[test]
fn direct_path_named_random_keeps_user_rng_constructors_out_of_builtin_runtime() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run-pass/random.au");
    let mir = crate::lower_path_to_mir(&path)
        .expect("path-level user Rng fixture should lower with source provenance");
    let object = emit_host_object(&mir)
        .expect("path-level local and imported user Rng classes should compile directly");
    let referenced = object_referenced_symbols(&object);

    assert!(
        !referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_rng_")),
        "user classes named Rng must not reference the random.Rng runtime: {referenced:?}"
    );
}

#[test]
fn direct_monotonic_time_uses_scalar_runtime_abi_without_generic_host_boxing() {
    let source = r#"
import sys

def main() -> int32:
    start: int64 = sys.monotonic_time_ms()
    finish: int64 = sys.monotonic_time_ms()
    if finish < start:
        return 1
    return 0
"#;
    let mir = lower_source_to_mir(source).expect("monotonic clock source should lower to MIR");
    let object = emit_host_object(&mir).expect("monotonic clock source should compile directly");
    let referenced = object_function_referenced_symbols(&object, "aura_fn_main");

    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_monotonic_time_ms")),
        "direct monotonic clock calls must use the scalar runtime ABI: {referenced:?}"
    );
    for generic_symbol in [
        "aura_direct_host_builtin",
        "aura_direct_arg_buffer_new",
        "aura_direct_box_i64",
        "aura_direct_unbox_int64",
    ] {
        assert!(
            !referenced
                .iter()
                .any(|symbol| symbol.contains(generic_symbol)),
            "direct monotonic clock calls must not use `{generic_symbol}`: {referenced:?}"
        );
    }
}

#[test]
fn direct_sleep_uses_void_runtime_abi_without_boxing_unit() {
    let source = r#"
def main() -> int32:
    sleep(0ms)
    return 0
"#;
    let mir = lower_source_to_mir(source).expect("sleep source should lower to MIR");
    let object = emit_host_object(&mir).expect("sleep source should compile directly");
    let referenced = object_referenced_symbols(&object);

    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_sleep_value_void")),
        "direct sleep calls must use the void runtime ABI: {referenced:?}"
    );
    assert!(
        !referenced
            .iter()
            .any(|symbol| symbol.ends_with("aura_direct_sleep_value")),
        "direct sleep calls must not allocate a boxed Unit result: {referenced:?}"
    );
}

#[test]
fn direct_yield_now_uses_void_runtime_abi_without_boxing_unit() {
    let source = r#"
def main() -> int32:
    yield_now()
    return 0
"#;
    let mir = lower_source_to_mir(source).expect("yield_now source should lower to MIR");
    let object = emit_host_object(&mir).expect("yield_now source should compile directly");
    let referenced = object_referenced_symbols(&object);

    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_yield_now")),
        "direct yield_now calls must use the void runtime ABI: {referenced:?}"
    );
    assert!(
        !referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_yield_now_value")),
        "direct yield_now calls must not use a boxed-value ABI: {referenced:?}"
    );
}

#[test]
fn native_loop_safepoint_uses_void_yield_runtime_abi_when_tasks_can_run() {
    let source = r#"
def worker(limit: int32) -> int32:
    mut value: int32 = 0
    while value < limit:
        value += 1
    return value

def main() -> int32:
    with TaskGroup() as group:
        task = group.start(worker, 8)
        mut value: int32 = 0
        while value < 8:
            value += 1
        match task.result():
            case TaskResult.Ready(result):
                return result
            case _:
                return 1
"#;
    let mir = lower_source_to_mir(source).expect("concurrent loop source should lower to MIR");
    assert!(
        mir.functions.iter().any(|function| {
            function.blocks.iter().any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| matches!(instruction, Instruction::Safepoint))
            })
        }),
        "loop lowering must retain the portable MIR safepoint marker"
    );
    assert!(
        mir.functions.iter().any(|function| {
            function.blocks.iter().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction,
                        Instruction::Assign {
                            value: Rvalue::StartTask { .. },
                            ..
                        }
                    )
                })
            })
        }),
        "the fixture must preserve a runnable sibling task"
    );

    let object = emit_host_object(&mir).expect("concurrent loop source should compile directly");
    let referenced = object_referenced_symbols(&object);
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_yield_now")),
        "a concurrent native loop must call the void scheduler ABI: {referenced:?}"
    );
    assert!(
        !referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_yield_now_value")),
        "an automatic safepoint must not request a boxed Unit result: {referenced:?}"
    );
}

#[test]
fn direct_task_lowering_preserves_specialized_repeatability_and_owned_observers() {
    let source = r#"
def relay[T](value: own T) -> T:
    return value

def int_worker() -> int64:
    return 1

def string_worker() -> str:
    return "value"

def main() -> int32:
    with TaskGroup() as group:
        int_task = group.start(relay[int64], 1)
        int_result = int_task.result_or(0)

        string_task = group.start_with_stack(262144, relay[str], "value")
        string_result = string_task.result_or("")

        int_tasks = [group.start(int_worker)]
        any_int = wait_any(int_tasks)
        string_tasks = [group.start(string_worker)]
        all_strings = wait_all(string_tasks)
    return 0
"#;
    let mir = lower_source_to_mir(source)
        .expect("specialized task ownership source should lower to direct MIR");
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let starts = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::StartTask {
                        result_is_copy,
                        stack_size,
                        ..
                    },
                ..
            } => Some((*result_is_copy, stack_size.is_some())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        starts,
        vec![(true, false), (false, true), (true, false), (false, false)]
    );
    assert!(main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| matches!(
            instruction,
            Instruction::Assign {
                value: Rvalue::Call {
                    callee: CallTarget::Member {
                        object: Operand::MovePlace(place),
                        field,
                        ..
                    },
                    ..
                },
                ..
            } if place == "string_task" && field == "result_or"
        )));
    assert!(main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| matches!(
            instruction,
            Instruction::Assign {
                value: Rvalue::Call {
                    callee: CallTarget::Name(name),
                    args,
                },
                ..
            } if name == "wait_all"
                && matches!(
                    args.first().map(|arg| &arg.value),
                    Some(Operand::MovePlace(place)) if place == "string_tasks"
                )
        )));

    let object = emit_host_object(&mir)
        .expect("specialized task starts and consuming observers should emit directly");
    assert!(!object.is_empty());
}

#[test]
fn native_loop_safepoint_elides_runtime_call_when_no_sibling_can_run() {
    let source = r#"
def main() -> int32:
    mut value: int32 = 0
    while value < 8:
        value += 1
    return value
"#;
    let mir = lower_source_to_mir(source).expect("sequential loop source should lower to MIR");
    assert!(
        mir.functions.iter().any(|function| {
            function.blocks.iter().any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| matches!(instruction, Instruction::Safepoint))
            })
        }),
        "static native elision must not remove the backend-independent MIR marker"
    );

    let object = emit_host_object(&mir).expect("sequential loop source should compile directly");
    let referenced = object_referenced_symbols(&object);
    assert!(
        !referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_yield_now")),
        "a module with no task start has no sibling to schedule and must elide the call: \
         {referenced:?}"
    );
}

#[test]
fn native_codegen_forwards_exact_call_and_task_frame_metadata_to_runtime_abis() {
    let source = r#"
def child() -> int32:
    return 7

def main() -> int32:
    with TaskGroup() as group:
        task = group.start(child)
        match task.result():
            case TaskResult.Ready(value):
                return value
            case _:
                return 0
"#;
    let mut mir = lower_source_to_mir(source).expect("task-frame source should lower");
    let child = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "child")
        .expect("child should lower");
    child.source_path = Some("/workspace/pkg/child.au".to_string());
    let main = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should lower");
    main.source_path = Some("/workspace/app/main.au".to_string());

    let object = emit_host_object_with_metadata(&mir, "/workspace/app/main.au", source)
        .expect("frame-aware task source should emit directly");
    let referenced = object_referenced_symbols(&object);
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_enter_call_with_frame")),
        "generated function entry must use the complete frame ABI: {referenced:?}"
    );
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_start_task_function_with_frames")),
        "generated task start must use the function-value ancestry-carrying ABI: {referenced:?}"
    );
    for legacy_abi in [
        "aura_direct_start_task_call_with_frames",
        "aura_direct_start_task_call",
    ] {
        assert!(
            !referenced.iter().any(|symbol| symbol.ends_with(legacy_abi)),
            "generated code must not silently fall back to legacy task ABI `{legacy_abi}`"
        );
    }

    let parsed = cranelift_object::object::File::parse(object.as_slice())
        .expect("frame-aware output should be a readable host object");
    let data_contains = |needle: &[u8]| {
        parsed.sections().any(|section| {
            section
                .data()
                .ok()
                .is_some_and(|data| data.windows(needle.len()).any(|window| window == needle))
        })
    };
    assert!(data_contains(b"/workspace/pkg/child.au"));
    assert!(data_contains(b"/workspace/app/main.au"));
    assert!(data_contains(b"child"));
    assert!(data_contains(b"main"));
}

#[test]
fn handbuilt_mir_safepoint_validates_and_emits_for_a_sequential_module() {
    let function = MirFunction {
        name: "main".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: Vec::new(),
        local_types: Vec::new(),
        return_type: Type::named("int32"),
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: vec![Instruction::Safepoint],
            terminator: Terminator::Return(Operand::Int(0)),
        }],
    };
    validate_function(&function, &HashMap::new())
        .expect("a hand-built MIR safepoint is a valid backend instruction");

    let module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![function],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    let object = emit_host_object(&module).expect("a hand-built MIR safepoint should emit");
    let referenced = object_referenced_symbols(&object);
    assert!(
        !referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_yield_now")),
        "hand-built sequential MIR must receive the same static elision: {referenced:?}"
    );
}

#[test]
fn handbuilt_mir_safepoint_does_not_mask_a_malformed_terminator() {
    let function = MirFunction {
        name: "main".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: Vec::new(),
        local_types: Vec::new(),
        return_type: Type::named("int32"),
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: vec![Instruction::Safepoint],
            terminator: Terminator::Unreachable,
        }],
    };

    let error = validate_function(&function, &HashMap::new())
        .expect_err("a valid safepoint must not make an unsupported terminator valid");
    assert!(
        error.contains("does not yet support MIR terminator"),
        "unexpected malformed-MIR diagnostic: {error}"
    );
}

#[test]
fn host_builtin_return_types_cover_the_control_plane_surface() {
    for name in [
        "sys::args",
        "sys::env",
        "sys::current_dir",
        "sys::unix_time_ms",
        "sys::monotonic_time_ms",
        "path::join",
        "path::parent",
        "path::file_name",
        "path::extension",
        "path::is_absolute",
        "json::parse",
        "json::dumps",
        "json::is_null",
        "json::as_bool",
        "json::as_int",
        "json::as_float",
        "json::into_string",
        "json::into_array",
        "json::into_object",
        "bytes::hex_encode",
        "bytes::hex_decode",
        "bytes::base64_encode",
        "bytes::base64_decode",
        "bytes::sha256",
        "bytes::sha256_string",
        "str.to_bytes",
        "str.from_bytes",
        "json::is_valid",
        "json::stringify_map",
        "json::parse_string_map",
        "toml::is_valid",
        "toml::stringify_map",
        "toml::parse_string_map",
        "metrics::increment",
        "metrics::get",
        "metrics::reset",
        "log::debug",
        "log::info",
        "log::warn",
        "log::error",
        "trace::event",
    ] {
        assert!(super::host_builtin_return_type(name).is_some(), "{name}");
    }
    assert!(super::host_builtin_return_type("missing::call").is_none());
    assert_eq!(
        super::builtin_opaque_member_return_type(&Type::named("str"), "to_bytes", &HashMap::new()),
        Some(DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![Type::named("uint8")],
        )))
    );
}

#[test]
fn direct_dynamic_json_surface_uses_the_host_builtin_abi() {
    let source = r#"
import json

def main() -> int32:
    match json.parse("{\"ready\":true,\"count\":2}"):
        case Result.Ok(value):
            print(json.dumps(value))
        case Result.Err(error):
            print(error)

    print(json.is_null(json.Value.Null))
    print(json.as_bool(json.Value.Bool(true)))
    print(json.as_int(json.Value.Int(7)))
    print(json.as_float(json.Value.Float(1.5)))
    print(json.into_string(json.Value.String("aura")))
    print(json.into_array(json.Value.Array([json.Value.Int(1)])))
    print(json.into_object(json.Value.Object({"value": json.Value.Int(1)})))
    return 0
"#;

    let mir = lower_source_to_mir(source).expect("dynamic JSON source should lower to MIR");
    let object = emit_host_object(&mir).expect("dynamic JSON source should compile directly");
    let referenced = object_referenced_symbols(&object);
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_host_builtin")),
        "dynamic JSON direct code must use the host builtin ABI: {referenced:?}"
    );
}

#[test]
fn direct_backend_emits_object_for_supported_scalar_program() {
    let source = "def helper(value: int32) -> int32:\n    return value + 2\n\ndef main() -> int32:\n    mut current: int32 = 1\n    if current < 5:\n        current = helper(value=current)\n    print(current)\n    return 0\n";

    let mir = lower_source_to_mir(source).expect("source should lower to MIR");
    let object = emit_host_object(&mir).expect("direct backend should emit an object");

    assert!(!object.is_empty());
}

#[test]
fn direct_backend_enables_stack_returns_for_flattened_mutable_writeback() {
    let flags: Flags = native_codegen_flags().expect("native flags should configure");
    assert!(
        flags.enable_multi_ret_implicit_sret(),
        "flattened mutable receiver writeback can exceed x86-64's return registers"
    );
}

#[cfg(target_arch = "x86_64")]
#[test]
fn direct_backend_x86_64_emits_three_integer_results_for_mutable_receiver_writeback() {
    let source = r#"
class Probe:
    condition_calls: int32
    message_calls: int32

    def condition(mut self) -> bool:
        self.condition_calls += 1
        return false

def main():
    mut probe = Probe(condition_calls=0, message_calls=0)
    assert probe.condition()
"#;
    let mir = lower_source_to_mir(source).expect("mutable receiver source should lower");
    let object = emit_host_object(&mir)
        .expect("x86-64 should spill flattened writeback results through a return area");
    assert!(!object.is_empty());
}

#[test]
fn ticket9_direct_backend_emits_unboxed_int64_ten_million_loop() {
    let source = r#"
def count_to(limit: int64) -> int64:
    mut current: int64 = 0
    while current < limit:
        current += 1
    return current

def main() -> int32:
    print(count_to(10000000))
    return 0
"#;

    let mir = lower_source_to_mir(source).expect("int64 loop should lower to MIR");
    let object = emit_host_object(&mir)
        .expect("the direct backend should promote loop literals into unboxed int64 values");
    let referenced = object_referenced_symbols(&object);

    assert!(!object.is_empty());
    assert!(
        !referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_binary_value_at")),
        "int64 loop arithmetic and comparison must not use boxed runtime dispatch: {referenced:?}"
    );
    assert!(
        !referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_cast_value_at")),
        "int64 contextual literals must be promoted without a boxed runtime cast: {referenced:?}"
    );
}

#[test]
fn ticket9_wide_scalar_casts_avoid_boxed_runtime_dispatch() {
    let source = r#"
def main() -> int32:
    signed: int64 = 42
    unsigned: uint64 = signed as uint64
    signed_again: int64 = unsigned as int64
    floating: float64 = signed_again as float64
    from_float: uint64 = floating as uint64
    print(from_float)
    return 0
"#;

    let mir = lower_source_to_mir(source).expect("wide scalar casts should lower to MIR");
    let object = emit_host_object(&mir).expect("wide scalar casts should emit directly");
    let referenced = object_referenced_symbols(&object);

    for forbidden in ["aura_direct_cast_value_at"] {
        assert!(
            !referenced.iter().any(|symbol| symbol.contains(forbidden)),
            "wide scalar casts must not reference `{forbidden}`: {referenced:?}"
        );
    }
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_cast_integer_to_integer")),
        "wide integer casts should use the unboxed checked helper"
    );
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_cast_integer_to_float")),
        "wide integer-to-float casts should use the unboxed exactness helper"
    );
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_cast_float_to_integer")),
        "float-to-wide-integer casts should use the unboxed checked helper"
    );
}

#[test]
fn ticket9_int64_opaque_results_use_the_checked_int64_unbox_helper() {
    let source = r#"
def main() -> int32:
    minimum: int64 = -9223372036854775808
    value: int64 = abs(minimum)
    print(value)
    return 0
"#;

    let mir = lower_source_to_mir(source).expect("int64 abs overflow source should lower");
    let object = emit_host_object(&mir).expect("int64 abs overflow source should emit directly");
    let referenced = object_referenced_symbols(&object);

    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_unbox_int64")),
        "opaque int64 results must use the target-aware checked helper: {referenced:?}"
    );
}

#[test]
fn ticket9_int64_task_thunks_use_the_checked_int64_unbox_helper() {
    let source = r#"
def echo(value: int64) -> int64:
    return value

def main() -> int32:
    with TaskGroup() as group:
        task = group.start(echo, -9223372036854775808)
        match task.result():
            case TaskResult.Ready(value):
                print(value)
            case _:
                return 1
    return 0
"#;

    let mir = lower_source_to_mir(source).expect("int64 task thunk source should lower");
    let object = emit_host_object(&mir).expect("int64 task thunk source should emit directly");
    let referenced = object_referenced_symbols(&object);

    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_unbox_int64")),
        "int64 task thunks must use the target-aware checked helper: {referenced:?}"
    );
}

#[test]
fn ticket9_wide_boundary_literals_stay_unboxed() {
    let source = r#"
def main() -> int32:
    minimum: int64 = -9223372036854775808
    maximum: uint64 = 18446744073709551615
    print(minimum)
    print(maximum)
    return 0
"#;

    let mir = lower_source_to_mir(source).expect("wide boundaries should lower to MIR");
    let object = emit_host_object(&mir).expect("wide boundaries should emit directly");
    let referenced = object_referenced_symbols(&object);

    for forbidden in [
        "aura_direct_unary_value_at",
        "aura_direct_box_uint_literal",
        "aura_direct_unbox_i64",
        "aura_direct_unbox_u64",
    ] {
        assert!(
            !referenced.iter().any(|symbol| symbol.contains(forbidden)),
            "wide boundary literals must not reference `{forbidden}`: {referenced:?}"
        );
    }
}

#[test]
fn ticket9_expected_uint64_operands_avoid_generic_literal_boxing() {
    let source = r#"
class Holder:
    value: uint64

    def echo(self, value: uint64) -> uint64:
        return value

def take(value: uint64) -> uint64:
    return value

def maximum() -> uint64:
    return 18446744073709551615

def main() -> int32:
    holder = Holder(value=18446744073709551615)
    direct = take(18446744073709551615)
    method = holder.echo(18446744073709551615)
    casted: uint64 = 18446744073709551615 as uint64
    print(maximum())
    print(direct)
    print(method)
    print(casted)
    return 0
"#;

    let mir = lower_source_to_mir(source).expect("expected uint64 operands should lower");
    let object = emit_host_object(&mir).expect("expected uint64 operands should emit directly");
    let referenced = [
        "aura_fn_Holder_echo",
        "aura_fn_take",
        "aura_fn_maximum",
        "aura_fn_main",
    ]
    .into_iter()
    .flat_map(|function| object_function_referenced_symbols(&object, function))
    .collect::<BTreeSet<_>>();

    for forbidden in [
        "aura_direct_box_uint_literal",
        "aura_direct_unbox_u64",
        "aura_direct_cast_value_at",
    ] {
        assert!(
            !referenced.iter().any(|symbol| symbol.contains(forbidden)),
            "expected uint64 operands must not reference `{forbidden}`: {referenced:?}"
        );
    }
}

#[test]
fn ticket9_uint64_opaque_boundaries_use_typed_boxing_without_generic_detours() {
    let source = r#"
def main() -> int32:
    values: list[uint64] = [18446744073709551615]
    maybe: Option[uint64] = Option.Some(18446744073709551615)
    print(values.len())
    print(maybe != Option.None)
    return 0
"#;

    let mir = lower_source_to_mir(source).expect("typed uint64 containers should lower");
    let object = emit_host_object(&mir).expect("typed uint64 containers should emit directly");
    let referenced = object_referenced_symbols(&object);

    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_box_u64")),
        "opaque uint64 boundaries should use the typed boxing helper"
    );
    for forbidden in ["aura_direct_box_uint_literal", "aura_direct_unbox_u64"] {
        assert!(
            !referenced.iter().any(|symbol| symbol.contains(forbidden)),
            "typed opaque uint64 boundaries must not reference `{forbidden}`: {referenced:?}"
        );
    }
}

#[test]
fn direct_backend_emits_contextual_none_and_unit_equality() {
    let source = include_str!("../tests/fixtures/run-pass/contextual_none_equality.au");
    let mir = lower_source_to_mir(source).expect("contextual None source should lower to MIR");
    let object = emit_host_object(&mir)
        .expect("direct backend should support contextual None and scalar unit equality");

    assert!(!object.is_empty());
}

#[test]
fn direct_backend_preserves_integer_call_types_through_equality_contexts() {
    let source = include_str!("../tests/fixtures/run-pass/integer_call_equality.au");
    let mir = lower_source_to_mir(source).expect("integer equality source should lower to MIR");

    emit_host_object(&mir)
        .expect("direct equality must preserve int32 and uint64 function-call result types");
}

#[test]
fn direct_backend_emits_retain_and_release_hooks_for_opaque_call_and_local_flow() {
    let source = r#"
def echo(value: str):
    print(value)

def main() -> int32:
    mut text = "hello"
    echo(text)
    text = "goodbye"
    print(text)
    return 0
"#;

    let mir = lower_source_to_mir(source).expect("source should lower to MIR");
    let object = emit_host_object(&mir).expect("direct backend should emit an object");
    let rendered = String::from_utf8_lossy(&object);

    assert!(rendered.contains("aura_direct_retain_value"));
    assert!(rendered.contains("aura_direct_release_value"));
}

#[test]
fn direct_backend_emits_object_for_plain_class_programs() {
    let source = include_str!("../../../examples/point.au");
    let mir = lower_source_to_mir(source).expect("point example should lower to MIR");
    let object = emit_host_object(&mir).expect("plain classes should now be supported directly");

    assert!(!object.is_empty());
}

#[test]
fn direct_backend_emits_object_for_trait_impl_dispatch() {
    let source = include_str!("../../../examples/traits/greeter.au");
    let mir = lower_source_to_mir(source).expect("trait example should lower to MIR");
    let object = emit_host_object(&mir).expect("trait impl dispatch should now compile directly");

    assert!(!object.is_empty());
}

#[test]
fn direct_backend_emits_object_for_extended_feature_examples() {
    let examples = [
        (
            "collections/list_polish",
            include_str!("../../../examples/collections/list_polish.au"),
        ),
        (
            "collections/dict_basics",
            include_str!("../../../examples/collections/dict_basics.au"),
        ),
        (
            "collections/set_basics",
            include_str!("../../../examples/collections/set_basics.au"),
        ),
        (
            "control_flow/match_literals",
            include_str!("../../../examples/control_flow/match_literals.au"),
        ),
        (
            "concurrency/task_group_start",
            include_str!("../../../examples/concurrency/task_group_start.au"),
        ),
        (
            "concurrency/queue_timeout",
            include_str!("../../../examples/concurrency/queue_timeout.au"),
        ),
        (
            "concurrency/queue_get_timeout_named",
            include_str!("../../../examples/concurrency/queue_get_timeout_named.au"),
        ),
        (
            "error_handling/try_result",
            include_str!("../../../examples/error_handling/try_result.au"),
        ),
        (
            "error_handling/try_from_trait",
            include_str!("../tests/fixtures/run-pass/try_from_trait.au"),
        ),
        (
            "numbers/numeric_builtins",
            include_str!("../../../examples/numbers/numeric_builtins.au"),
        ),
        (
            "strings/string_methods",
            include_str!("../../../examples/strings/string_methods.au"),
        ),
        (
            "strings/string_parsing_and_formatting",
            include_str!("../../../examples/strings/string_parsing_and_formatting.au"),
        ),
        (
            "traits/operator_traits",
            include_str!("../../../examples/traits/operator_traits.au"),
        ),
        (
            "traits/ordering_traits",
            include_str!("../../../examples/traits/ordering_traits.au"),
        ),
        (
            "basics/borrowed_lifetime_labels",
            include_str!("../../../examples/basics/copy_return_selection.au"),
        ),
        (
            "traits/generic_trait_bounds",
            include_str!("../../../examples/traits/generic_trait_bounds.au"),
        ),
        (
            "traits/specialized_trait_dispatch",
            include_str!("../../../examples/traits/specialized_trait_dispatch.au"),
        ),
    ];

    for (name, source) in examples {
        let mir = lower_source_to_mir(source).expect("example should lower to MIR");
        let object = emit_host_object(&mir).expect("example should emit direct object");
        assert!(!object.is_empty(), "{name}");
    }
}

#[test]
fn direct_backend_emits_object_for_every_runnable_fixture() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run-pass");
    let mut paths = std::fs::read_dir(&fixtures)
        .expect("run-pass fixture directory should be readable")
        .map(|entry| {
            entry
                .expect("run-pass fixture entry should be readable")
                .path()
        })
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("au"))
        .collect::<Vec<_>>();
    paths.sort();

    for path in paths {
        let mir = lower_path_to_mir(&path)
            .unwrap_or_else(|error| panic!("{} should lower to MIR: {error}", path.display()));
        let object = emit_host_object(&mir).unwrap_or_else(|error| {
            panic!("{} should emit a direct object: {error}", path.display())
        });
        assert!(!object.is_empty(), "{}", path.display());
    }
}

#[test]
fn direct_backend_emits_object_for_runtime_member_surface_matrix() {
    let source = r#"
def worker(value: int32) -> int32:
    return value + 1

def main() -> int32:
    text = "  Aura Repo  "
    trimmed = text.trim()
    truth = (trimmed.contains("Repo") and true) or false
    print(truth)
    print(trimmed.len())
    print(trimmed.contains("Repo"))
    print(trimmed.starts_with("Aura"))
    print(trimmed.ends_with("Repo"))
    print(trimmed.replace("Repo", "Lang"))
    print(trimmed.to_lower())
    print(trimmed.to_upper())
    words = trimmed.split(" ")
    print("/".join(words))
    match trimmed.strip_prefix("Aura "):
        case Some(rest):
            print(rest)
        case None:
            print("missing")
    match trimmed.strip_suffix(" Repo"):
        case Some(rest):
            print(rest)
        case None:
            print("missing")

    mut numbers = [1, 2]
    print(numbers.len())
    print(numbers.is_empty())
    mut clone_numbers = numbers.copy()
    clone_numbers.append(3)
    print(clone_numbers.pop())
    print(clone_numbers.get(0))
    print(clone_numbers[1])
    print(clone_numbers.set(0, 9))
    clone_numbers[1] = 8
    print(clone_numbers.remove(0))
    print(clone_numbers.swap(0, 0))
    print(clone_numbers.contains(8))
    print(clone_numbers.insert(1, 7))
    clone_numbers.reverse()
    clone_numbers.extend([5, 6])
    clone_numbers.clear()
    print(clone_numbers.is_empty())

    mut counts = {"a": 1}
    print(counts.len())
    print(counts.is_empty())
    copy_counts = counts.copy()
    print(copy_counts.get("a"))
    print(copy_counts["a"])
    print(counts["a"])
    counts["a"] = 2
    counts["b"] = 3
    print(counts.remove("a"))
    print("b" in counts)
    print(counts.keys().len())
    print(counts.values().len())
    print(counts.items().len())
    counts.update({"c": 4})
    counts.clear()
    print(counts.is_empty())

    mut seen = {"x"}
    print(seen.len())
    print(seen.is_empty())
    copy_seen = seen.copy()
    print("x" in copy_seen)
    print(seen.add("y"))
    print(seen.remove("x"))
    print("y" in seen)

    jobs = Queue[int32]()
    jobs_copy = jobs
    print(jobs_copy.put(1))
    print(jobs.get())
    jobs.close()

    with TaskGroup() as group:
        task = group.start(worker, 4)
        task_copy = task
        print(task_copy.result())
        group.cancel()

    return 0
"#;

    let mir = lower_source_to_mir(source).expect("runtime member matrix should lower to MIR");
    let object = emit_host_object(&mir).expect("runtime member matrix should compile directly");

    assert!(!object.is_empty());
}

#[test]
fn direct_backend_emits_object_for_call_writeback_and_cleanup_surface() {
    let source = r#"
class Counter:
    value: int32

class Resource:
    closed: bool = false
    def close(mut self):
        self.closed = true

def bump(counter: mut Counter, amount: int32 = 2) -> int32:
    counter.value += amount
    return counter.value

def copy_into(source: Counter, target: mut Counter):
    target.value = source.value

def worker(value: int32) -> int32:
    return value + 1

def main() -> int32:
    mut first = Counter(value=1)
    mut second = Counter(value=0)
    print(bump(counter=first))
    copy_into(source=first, target=second)
    print(second.value)

    mut total: int64 = 0
    for i in range(stop=3):
        total += i
    print(total)

    print(abs(-3))
    print(min(8, 2))
    print(max(8, 2))
    print(parse_int32("12"))
    print(cancelled())

    jobs = Queue[int32]()
    print(jobs.put(7))
    match jobs.get(timeout=1ms):
        case QueueReceive.Item(value):
            print(value)
        case QueueReceive.TimedOut:
            print(99)
        case QueueReceive.Closed:
            print(98)
        case QueueReceive.Cancelled:
            print(97)
    jobs.close()

    sleep(0ms)

    with Resource() as resource:
        print(resource.closed)

    with TaskGroup() as group:
        task = group.start(worker, 4)
        print(task.result())
        group.cancel()

    return second.value
"#;

    let mir = lower_source_to_mir(source).expect("writeback/cleanup matrix should lower to MIR");
    let object = emit_host_object(&mir).expect("writeback/cleanup matrix should compile directly");
    assert!(!object.is_empty());
}

#[test]
fn direct_backend_emits_object_for_supported_cleanup_and_explicit_task_group_close() {
    let source = r#"
class Resource:
    closed: bool = false
    def close(mut self):
        self.closed = true

def main() -> int32:
    with Resource() as resource:
        print(resource.closed)

    with TaskGroup() as group:
        group.cancel()

    return 0
"#;

    let mir = lower_source_to_mir(source).expect("cleanup surface should lower to MIR");
    let object = emit_host_object(&mir).expect("cleanup surface should compile directly");

    assert!(!object.is_empty());
}

fn module_with_main_call(call: Rvalue) -> crate::mir::MirModule {
    module_with_main_call_result_type(call, Type::named("int32"))
}

fn module_with_main_call_result_type(call: Rvalue, result_ty: Type) -> crate::mir::MirModule {
    crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![MirLocalType {
                name: "%t0".to_string(),
                ty: result_ty,
            }],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![Instruction::Assign {
                    target: "%t0".to_string(),
                    value: call,
                }],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    }
}

#[test]
fn direct_ffi_codegen_embeds_call_metadata_and_uses_only_the_runtime_adapter() {
    let call = Rvalue::Call {
        callee: CallTarget::Extern(MirExternCall {
            symbol: "getpid".to_string(),
            abi: "C".to_string(),
            params: Vec::new(),
            return_type: Type::named("int32"),
        }),
        args: Vec::new(),
    };
    let module = module_with_main_call_result_type(call, Type::named("int32"));
    let object = emit_host_object(&module).expect("direct getpid FFI should emit");
    let referenced = object_referenced_symbols(&object);
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_ffi_call")),
        "direct FFI must route through the shared runtime adapter: {referenced:?}"
    );
    assert!(
        referenced.iter().all(|symbol| !symbol.contains("getpid")),
        "generated objects must resolve process symbols at runtime: {referenced:?}"
    );
    assert!(
        object.windows(4).any(|bytes| bytes == b"AUFI")
            && object.windows(6).any(|bytes| bytes == b"getpid"),
        "the validated serialized call spec must be embedded in the object"
    );
}

#[test]
fn direct_ffi_codegen_handles_narrow_scalars_mutable_bytes_and_opaque_ownership() {
    let handle_ty = Type::named("ProcessHandle");
    let bytes_ty = Type::Named("list".to_string(), vec![Type::named("uint8")]);
    let module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![
                MirLocalType {
                    name: "bytes".to_string(),
                    ty: bytes_ty.clone(),
                },
                MirLocalType {
                    name: "read_count".to_string(),
                    ty: Type::named("int64"),
                },
                MirLocalType {
                    name: "handle".to_string(),
                    ty: handle_ty.clone(),
                },
                MirLocalType {
                    name: "released".to_string(),
                    ty: Type::Unit,
                },
                MirLocalType {
                    name: "narrow".to_string(),
                    ty: Type::named("uint16"),
                },
            ],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::Assign {
                        target: "bytes".to_string(),
                        value: Rvalue::VecLiteral {
                            elements: vec![Operand::Int(0), Operand::Int(0)],
                            element_type: Type::named("uint8"),
                        },
                    },
                    Instruction::Assign {
                        target: "read_count".to_string(),
                        value: Rvalue::Call {
                            callee: CallTarget::Extern(MirExternCall {
                                symbol: "read".to_string(),
                                abi: "C".to_string(),
                                params: vec![
                                    MirExternParam {
                                        name: "fd".to_string(),
                                        passing: MirReceiverKind::Borrow,
                                        ty: Type::named("int32"),
                                    },
                                    MirExternParam {
                                        name: "bytes".to_string(),
                                        passing: MirReceiverKind::BorrowMut,
                                        ty: bytes_ty.clone(),
                                    },
                                ],
                                return_type: Type::named("int64"),
                            }),
                            args: vec![
                                MirArg {
                                    name: None,
                                    value: Operand::Int(0),
                                    writeback_place: None,
                                },
                                MirArg {
                                    name: None,
                                    value: Operand::MovePlace("bytes".to_string()),
                                    writeback_place: Some("bytes".to_string()),
                                },
                            ],
                        },
                    },
                    Instruction::Assign {
                        target: "handle".to_string(),
                        value: Rvalue::Call {
                            callee: CallTarget::Extern(MirExternCall {
                                symbol: "malloc".to_string(),
                                abi: "C".to_string(),
                                params: vec![MirExternParam {
                                    name: "size".to_string(),
                                    passing: MirReceiverKind::Borrow,
                                    ty: Type::named("uint64"),
                                }],
                                return_type: handle_ty.clone(),
                            }),
                            args: vec![MirArg {
                                name: None,
                                value: Operand::Int(1),
                                writeback_place: None,
                            }],
                        },
                    },
                    Instruction::Assign {
                        target: "released".to_string(),
                        value: Rvalue::Call {
                            callee: CallTarget::Extern(MirExternCall {
                                symbol: "free".to_string(),
                                abi: "C".to_string(),
                                params: vec![MirExternParam {
                                    name: "handle".to_string(),
                                    passing: MirReceiverKind::Value,
                                    ty: handle_ty,
                                }],
                                return_type: Type::Unit,
                            }),
                            args: vec![MirArg {
                                name: None,
                                value: Operand::MovePlace("handle".to_string()),
                                writeback_place: None,
                            }],
                        },
                    },
                    Instruction::Assign {
                        target: "narrow".to_string(),
                        value: Rvalue::Call {
                            callee: CallTarget::Extern(MirExternCall {
                                symbol: "htons".to_string(),
                                abi: "C".to_string(),
                                params: vec![MirExternParam {
                                    name: "value".to_string(),
                                    passing: MirReceiverKind::Borrow,
                                    ty: Type::named("uint16"),
                                }],
                                return_type: Type::named("uint16"),
                            }),
                            args: vec![MirArg {
                                name: None,
                                value: Operand::Int(7),
                                writeback_place: None,
                            }],
                        },
                    },
                ],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    let object = emit_host_object(&module)
        .expect("mutable byte views, opaque ownership, and narrow scalars should emit");
    assert!(
        object_referenced_symbol_occurrences(&object, "aura_direct_ffi_call") >= 4,
        "each extern call must reference the shared runtime adapter"
    );
}

#[test]
fn direct_ffi_validation_rejects_unvalidated_metadata() {
    let invalid_abi = module_with_main_call_result_type(
        Rvalue::Call {
            callee: CallTarget::Extern(MirExternCall {
                symbol: "foreign".to_string(),
                abi: "Rust".to_string(),
                params: Vec::new(),
                return_type: Type::named("int32"),
            }),
            args: Vec::new(),
        },
        Type::named("int32"),
    );
    let error = emit_host_object(&invalid_abi).expect_err("non-C FFI must never reach codegen");
    assert!(error.contains("unsupported FFI ABI `Rust`"));

    let wrong_arity = module_with_main_call_result_type(
        Rvalue::Call {
            callee: CallTarget::Extern(MirExternCall {
                symbol: "foreign".to_string(),
                abi: "C".to_string(),
                params: vec![MirExternParam {
                    name: "value".to_string(),
                    passing: MirReceiverKind::Borrow,
                    ty: Type::named("int32"),
                }],
                return_type: Type::named("int32"),
            }),
            args: Vec::new(),
        },
        Type::named("int32"),
    );
    assert_eq!(
        emit_host_object(&wrong_arity).expect_err("extern arity drift must be rejected"),
        "direct backend expected 1 argument(s) for extern `foreign`, found 0"
    );

    let unexpected_writeback = module_with_main_call_result_type(
        Rvalue::Call {
            callee: CallTarget::Extern(MirExternCall {
                symbol: "foreign".to_string(),
                abi: "C".to_string(),
                params: vec![MirExternParam {
                    name: "value".to_string(),
                    passing: MirReceiverKind::Borrow,
                    ty: Type::named("int32"),
                }],
                return_type: Type::named("int32"),
            }),
            args: vec![MirArg {
                name: None,
                value: Operand::Int(1),
                writeback_place: Some("value".to_string()),
            }],
        },
        Type::named("int32"),
    );
    assert_eq!(
        emit_host_object(&unexpected_writeback)
            .expect_err("shared extern arguments must not request writeback"),
        "direct backend extern argument 1 unexpectedly requests writeback"
    );

    let bytes_ty = Type::Named("list".to_string(), vec![Type::named("uint8")]);
    let missing_mut_writeback = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![
                MirLocalType {
                    name: "bytes".to_string(),
                    ty: bytes_ty.clone(),
                },
                MirLocalType {
                    name: "%t0".to_string(),
                    ty: Type::named("int64"),
                },
            ],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::Assign {
                        target: "bytes".to_string(),
                        value: Rvalue::VecLiteral {
                            elements: vec![Operand::Int(0)],
                            element_type: Type::named("uint8"),
                        },
                    },
                    Instruction::Assign {
                        target: "%t0".to_string(),
                        value: Rvalue::Call {
                            callee: CallTarget::Extern(MirExternCall {
                                symbol: "read".to_string(),
                                abi: "C".to_string(),
                                params: vec![MirExternParam {
                                    name: "bytes".to_string(),
                                    passing: MirReceiverKind::BorrowMut,
                                    ty: bytes_ty,
                                }],
                                return_type: Type::named("int64"),
                            }),
                            args: vec![MirArg {
                                name: None,
                                value: Operand::MovePlace("bytes".to_string()),
                                writeback_place: None,
                            }],
                        },
                    },
                ],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    assert_eq!(
        emit_host_object(&missing_mut_writeback)
            .expect_err("mutable byte views require an explicit writeback place"),
        "direct backend extern mutable argument 1 has no writeback place"
    );
}

#[test]
fn direct_ffi_source_types_pin_the_v0_abi_and_capability_contract() {
    use crate::ffi::FfiType;

    for (name, expected) in [
        ("bool", FfiType::Bool),
        ("int8", FfiType::I8),
        ("int16", FfiType::I16),
        ("int32", FfiType::I32),
        ("int", FfiType::I64),
        ("int64", FfiType::I64),
        ("uint8", FfiType::U8),
        ("uint16", FfiType::U16),
        ("uint32", FfiType::U32),
        ("uint64", FfiType::U64),
        ("float32", FfiType::F32),
        ("float64", FfiType::F64),
    ] {
        let direct = direct_ffi_type_for_source(&Type::named(name), Some(MirReceiverKind::Borrow))
            .unwrap_or_else(|error| panic!("{name} should be a valid shared FFI scalar: {error}"));
        assert_eq!(direct.ffi_type, expected, "{name} ABI kind");
        assert_eq!(direct.opaque_name, None, "{name} is not nominal");
    }

    assert_eq!(
        direct_ffi_type_for_source(&Type::Unit, None)
            .expect("None is the FFI unit return")
            .ffi_type,
        FfiType::Unit
    );
    let string = direct_ffi_type_for_source(&Type::named("str"), Some(MirReceiverKind::Borrow))
        .expect("shared str is a UTF-8 view");
    assert_eq!(string.ffi_type, FfiType::StringView);
    let bytes_ty = Type::Named("list".to_string(), vec![Type::named("uint8")]);
    assert_eq!(
        direct_ffi_type_for_source(&bytes_ty, Some(MirReceiverKind::Borrow))
            .expect("shared bytes are a const view")
            .ffi_type,
        FfiType::BytesView
    );
    assert_eq!(
        direct_ffi_type_for_source(&bytes_ty, Some(MirReceiverKind::BorrowMut))
            .expect("mutable bytes are a scratch view")
            .ffi_type,
        FfiType::BytesViewMut
    );
    let handle =
        direct_ffi_type_for_source(&Type::named("ProcessHandle"), Some(MirReceiverKind::Value))
            .expect("owned opaque handles are consumed");
    assert_eq!(handle.ffi_type, FfiType::OpaqueHandle);
    assert_eq!(handle.opaque_name.as_deref(), Some("ProcessHandle"));

    for passing in [MirReceiverKind::BorrowMut, MirReceiverKind::Value] {
        assert_eq!(
            direct_ffi_type_for_source(&Type::named("int32"), Some(passing)),
            Err(format!(
                "direct backend cannot pass `int32` with ownership mode `{passing:?}` through FFI v0"
            ))
        );
    }
    assert_eq!(
        direct_ffi_type_for_source(&Type::named("str"), Some(MirReceiverKind::BorrowMut)),
        Err("direct backend can pass `str` through FFI v0 only as a shared view".to_string())
    );
    assert_eq!(
        direct_ffi_type_for_source(
            &Type::named("ProcessHandle"),
            Some(MirReceiverKind::BorrowMut)
        ),
        Err(
            "direct backend cannot mutably borrow opaque handle `ProcessHandle` through FFI v0"
                .to_string()
        )
    );
    assert_eq!(
        direct_ffi_type_for_source(&bytes_ty, Some(MirReceiverKind::Value)),
        Err("direct backend cannot pass `own list[uint8]` through FFI v0".to_string())
    );
    assert_eq!(
        direct_ffi_type_for_source(&bytes_ty, None),
        Err("direct backend cannot return `list[uint8]` through FFI v0".to_string())
    );
    assert_eq!(
        direct_ffi_type_for_source(
            &Type::Named("list".to_string(), vec![Type::named("int32")]),
            Some(MirReceiverKind::Borrow),
        ),
        Err("direct backend cannot lower `list[int32]` through FFI v0".to_string())
    );
    assert_eq!(
        direct_ffi_type_for_source(
            &Type::Tuple(vec![Type::named("int32")]),
            Some(MirReceiverKind::Borrow),
        ),
        Err("direct backend cannot lower `(int32,)` through FFI v0".to_string())
    );
}

fn module_with_main_member_call(
    object_name: &str,
    object_ty: Type,
    object_value: Rvalue,
    field: &str,
    args: Vec<MirArg>,
) -> crate::mir::MirModule {
    module_with_main_member_call_result_type(
        object_name,
        object_ty,
        object_value,
        Type::named("int32"),
        field,
        args,
    )
}

fn module_with_main_member_call_result_type(
    object_name: &str,
    object_ty: Type,
    object_value: Rvalue,
    result_ty: Type,
    field: &str,
    args: Vec<MirArg>,
) -> crate::mir::MirModule {
    crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![
                MirLocalType {
                    name: object_name.to_string(),
                    ty: object_ty,
                },
                MirLocalType {
                    name: "%t0".to_string(),
                    ty: result_ty,
                },
            ],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::Assign {
                        target: object_name.to_string(),
                        value: object_value,
                    },
                    Instruction::Assign {
                        target: "%t0".to_string(),
                        value: Rvalue::Call {
                            callee: CallTarget::Member {
                                object: Operand::Place(object_name.to_string()),
                                field: field.to_string(),
                                receiver_place: Some(object_name.to_string()),
                            },
                            args,
                        },
                    },
                ],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    }
}

#[test]
fn direct_backend_internal_collection_member_surface_compiles() {
    let string_byte_len = module_with_main_member_call_result_type(
        "text",
        Type::named("str"),
        Rvalue::Use(Operand::String("é🎉e\u{301}".to_string())),
        Type::named("int64"),
        "byte_len",
        Vec::new(),
    );
    assert!(!emit_host_object(&string_byte_len)
        .expect("str.byte_len() should compile directly")
        .is_empty());

    let slice_args = || {
        vec![
            MirArg {
                name: None,
                value: Operand::Int(1),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Bool(true),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Int(3),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Bool(true),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Int(1),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Int(5),
                writeback_place: None,
            },
        ]
    };
    let string_slice = module_with_main_member_call_result_type(
        "text",
        Type::named("str"),
        Rvalue::Use(Operand::String("Aé🙂Z".to_string())),
        Type::named("str"),
        "__slice",
        slice_args(),
    );
    assert!(!emit_host_object(&string_slice)
        .expect("internal owned str slicing should compile directly")
        .is_empty());

    let vec_slice = module_with_main_member_call_result_type(
        "values",
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        Rvalue::VecLiteral {
            element_type: Type::named("int32"),
            elements: vec![
                Operand::Int(1),
                Operand::Int(2),
                Operand::Int(3),
                Operand::Int(4),
            ],
        },
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        "__slice",
        slice_args(),
    );
    assert!(!emit_host_object(&vec_slice)
        .expect("internal owned Vec slicing should compile directly")
        .is_empty());

    let vec_index_option = module_with_main_member_call_result_type(
        "values",
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        Rvalue::VecLiteral {
            element_type: Type::named("int32"),
            elements: vec![Operand::Int(1), Operand::Int(2)],
        },
        Type::Named("Option".to_string(), vec![Type::named("int32")]),
        "__index_option",
        vec![MirArg {
            name: None,
            value: Operand::Int(1),
            writeback_place: None,
        }],
    );
    assert!(!emit_host_object(&vec_index_option)
        .expect("internal vec optional indexing should compile directly")
        .is_empty());

    let vec_set_index = module_with_main_member_call_result_type(
        "values",
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        Rvalue::VecLiteral {
            element_type: Type::named("int32"),
            elements: vec![Operand::Int(1), Operand::Int(2)],
        },
        Type::Unit,
        "__set_index",
        vec![
            MirArg {
                name: None,
                value: Operand::Int(0),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Int(9),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Int(1),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Int(1),
                writeback_place: None,
            },
        ],
    );
    assert!(!emit_host_object(&vec_set_index)
        .expect("internal vec indexed assignment should compile directly")
        .is_empty());

    let map_index = module_with_main_member_call_result_type(
        "counts",
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        ),
        Rvalue::MapLiteral {
            key_type: Type::named("str"),
            value_type: Type::named("int32"),
            entries: vec![MirMapEntry {
                key: Operand::String("a".to_string()),
                value: Operand::Int(1),
            }],
        },
        Type::named("int32"),
        "__index",
        vec![
            MirArg {
                name: None,
                value: Operand::String("a".to_string()),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Int(1),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Int(1),
                writeback_place: None,
            },
        ],
    );
    assert!(!emit_host_object(&map_index)
        .expect("internal map indexing should compile directly")
        .is_empty());

    let map_set_index = module_with_main_member_call_result_type(
        "counts",
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        ),
        Rvalue::MapLiteral {
            key_type: Type::named("str"),
            value_type: Type::named("int32"),
            entries: vec![MirMapEntry {
                key: Operand::String("a".to_string()),
                value: Operand::Int(1),
            }],
        },
        Type::Unit,
        "__set_index",
        vec![
            MirArg {
                name: None,
                value: Operand::String("b".to_string()),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Int(2),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Int(1),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Int(1),
                writeback_place: None,
            },
        ],
    );
    assert!(!emit_host_object(&map_set_index)
        .expect("internal map indexed assignment should compile directly")
        .is_empty());

    let set_index_option = module_with_main_member_call_result_type(
        "seen",
        Type::Named("set".to_string(), vec![Type::named("str")]),
        Rvalue::SetLiteral {
            element_type: Type::named("str"),
            elements: vec![Operand::String("x".to_string())],
        },
        Type::Named("Option".to_string(), vec![Type::named("str")]),
        "__index_option",
        vec![MirArg {
            name: None,
            value: Operand::Int(0),
            writeback_place: None,
        }],
    );
    assert!(!emit_host_object(&set_index_option)
        .expect("internal set optional indexing should compile directly")
        .is_empty());
}

#[test]
fn direct_backend_scalar_bool_range_and_coercion_paths_compile() {
    let module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![
                MirLocalType {
                    name: "%left".to_string(),
                    ty: Type::named("bool"),
                },
                MirLocalType {
                    name: "%right".to_string(),
                    ty: Type::named("bool"),
                },
                MirLocalType {
                    name: "%and".to_string(),
                    ty: Type::named("bool"),
                },
                MirLocalType {
                    name: "%or".to_string(),
                    ty: Type::named("bool"),
                },
                MirLocalType {
                    name: "%range".to_string(),
                    ty: Type::named("Range"),
                },
                MirLocalType {
                    name: "%int_as_bool".to_string(),
                    ty: Type::named("bool"),
                },
                MirLocalType {
                    name: "%int32_value".to_string(),
                    ty: Type::named("int32"),
                },
                MirLocalType {
                    name: "%unit_as_int".to_string(),
                    ty: Type::named("int32"),
                },
            ],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::Assign {
                        target: "%left".to_string(),
                        value: Rvalue::Use(Operand::Bool(true)),
                    },
                    Instruction::Assign {
                        target: "%right".to_string(),
                        value: Rvalue::Use(Operand::Bool(false)),
                    },
                    Instruction::Assign {
                        target: "%and".to_string(),
                        value: Rvalue::Binary {
                            op: BinaryOp::And,
                            left: Operand::Place("%left".to_string()),
                            right: Operand::Place("%right".to_string()),
                            span: Span::new(1, 1),
                        },
                    },
                    Instruction::Assign {
                        target: "%or".to_string(),
                        value: Rvalue::Binary {
                            op: BinaryOp::Or,
                            left: Operand::Place("%left".to_string()),
                            right: Operand::Place("%right".to_string()),
                            span: Span::new(1, 1),
                        },
                    },
                    Instruction::Assign {
                        target: "%range".to_string(),
                        value: Rvalue::Call {
                            callee: CallTarget::Name("range".to_string()),
                            args: vec![
                                MirArg {
                                    name: None,
                                    value: Operand::Int(0),
                                    writeback_place: None,
                                },
                                MirArg {
                                    name: None,
                                    value: Operand::Int(4),
                                    writeback_place: None,
                                },
                                MirArg {
                                    name: Some("start".to_string()),
                                    value: Operand::Int(1),
                                    writeback_place: None,
                                },
                            ],
                        },
                    },
                    Instruction::Assign {
                        target: "%int32_value".to_string(),
                        value: Rvalue::Use(Operand::Int(1)),
                    },
                    Instruction::Assign {
                        target: "%int_as_bool".to_string(),
                        value: Rvalue::Use(Operand::Place("%int32_value".to_string())),
                    },
                    Instruction::Assign {
                        target: "%unit_as_int".to_string(),
                        value: Rvalue::Use(Operand::Unit),
                    },
                ],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    let object = emit_host_object(&module)
        .expect("manual scalar bool/range/coercion MIR should emit direct object");
    assert!(!object.is_empty());
}

#[test]
fn direct_backend_internal_collection_member_errors_are_reported() {
    let cases = [
        (
            module_with_main_member_call_result_type(
                "values",
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                Rvalue::VecLiteral {
                    element_type: Type::named("int32"),
                    elements: vec![Operand::Int(1)],
                },
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "__index_option",
                Vec::new(),
            ),
            "internal optional vector indexing",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                Rvalue::VecLiteral {
                    element_type: Type::named("int32"),
                    elements: vec![Operand::Int(1)],
                },
                Type::named("int32"),
                "__index",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(0),
                    writeback_place: None,
                }],
            ),
            "internal vector indexing",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                Type::Named(
                    "dict".to_string(),
                    vec![Type::named("str"), Type::named("int32")],
                ),
                Rvalue::MapLiteral {
                    key_type: Type::named("str"),
                    value_type: Type::named("int32"),
                    entries: vec![MirMapEntry {
                        key: Operand::String("a".to_string()),
                        value: Operand::Int(1),
                    }],
                },
                Type::Unit,
                "__set_index",
                vec![MirArg {
                    name: None,
                    value: Operand::String("a".to_string()),
                    writeback_place: None,
                }],
            ),
            "internal map indexed assignment",
        ),
        (
            module_with_main_member_call_result_type(
                "seen",
                Type::Named("set".to_string(), vec![Type::named("str")]),
                Rvalue::SetLiteral {
                    element_type: Type::named("str"),
                    elements: vec![Operand::String("x".to_string())],
                },
                Type::Named("Option".to_string(), vec![Type::named("str")]),
                "__index_option",
                Vec::new(),
            ),
            "internal optional set indexing",
        ),
    ];

    for (module, expected) in cases {
        let error = emit_host_object(&module).expect_err("invalid internal collection member call");
        assert!(
            error.contains(expected),
            "expected `{expected}` in `{error}`"
        );
    }
}

#[test]
fn direct_backend_runtime_member_matrix_covers_remaining_string_collection_and_runtime_paths() {
    let string_ty = Type::named("str");
    let string_value = Rvalue::Use(Operand::String("Aura".to_string()));
    let vec_ty = Type::Named("list".to_string(), vec![Type::named("int32")]);
    let vec_value = Rvalue::VecLiteral {
        element_type: Type::named("int32"),
        elements: vec![Operand::Int(1), Operand::Int(2)],
    };
    let map_ty = Type::Named(
        "dict".to_string(),
        vec![Type::named("str"), Type::named("int32")],
    );
    let map_value = Rvalue::MapLiteral {
        key_type: Type::named("str"),
        value_type: Type::named("int32"),
        entries: vec![MirMapEntry {
            key: Operand::String("count".to_string()),
            value: Operand::Int(1),
        }],
    };
    let set_ty = Type::Named("set".to_string(), vec![Type::named("str")]);
    let set_value = Rvalue::SetLiteral {
        element_type: Type::named("str"),
        elements: vec![Operand::String("ready".to_string())],
    };
    let channel_ty = Type::Named("Queue".to_string(), vec![Type::named("int32")]);
    let channel_value = Rvalue::Call {
        callee: CallTarget::Name("Queue".to_string()),
        args: Vec::new(),
    };
    let task_group_ty = Type::named("TaskGroup");
    let task_group_value = Rvalue::Call {
        callee: CallTarget::Name("TaskGroup".to_string()),
        args: Vec::new(),
    };
    let cases = vec![
        (
            "str.len",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("int64"),
                "len",
                Vec::new(),
            ),
        ),
        (
            "str.byte_len",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("int64"),
                "byte_len",
                Vec::new(),
            ),
        ),
        (
            "str.contains",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("bool"),
                "contains",
                vec![MirArg {
                    name: None,
                    value: Operand::String("ror".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "str.starts_with",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("bool"),
                "starts_with",
                vec![MirArg {
                    name: None,
                    value: Operand::String("Aur".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "str.ends_with",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("bool"),
                "ends_with",
                vec![MirArg {
                    name: None,
                    value: Operand::String("ora".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "str.split",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::Named("list".to_string(), vec![Type::named("str")]),
                "split",
                vec![MirArg {
                    name: None,
                    value: Operand::String("r".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "str.replace",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("str"),
                "replace",
                vec![
                    MirArg {
                        name: None,
                        value: Operand::String("Aur".to_string()),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::String("Our".to_string()),
                        writeback_place: None,
                    },
                ],
            ),
        ),
        (
            "str.add",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("str"),
                "add",
                vec![MirArg {
                    name: None,
                    value: Operand::String(" language".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "str.to_lower",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("str"),
                "to_lower",
                Vec::new(),
            ),
        ),
        (
            "str.to_upper",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("str"),
                "to_upper",
                Vec::new(),
            ),
        ),
        (
            "str.strip_prefix",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("str")]),
                "strip_prefix",
                vec![MirArg {
                    name: None,
                    value: Operand::String("Au".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "str.strip_suffix",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("str")]),
                "strip_suffix",
                vec![MirArg {
                    name: None,
                    value: Operand::String("ra".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "str.trim",
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                Rvalue::Use(Operand::String("  Aura  ".to_string())),
                Type::named("str"),
                "trim",
                Vec::new(),
            ),
        ),
        (
            "list.len",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::named("int64"),
                "len",
                Vec::new(),
            ),
        ),
        (
            "list.is_empty",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::named("bool"),
                "is_empty",
                Vec::new(),
            ),
        ),
        (
            "list.append",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Unit,
                "append",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(3),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "list.pop",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::named("int32"),
                "pop",
                Vec::new(),
            ),
        ),
        (
            "list.get",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "get",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(0),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "list.set",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "set",
                vec![
                    MirArg {
                        name: None,
                        value: Operand::Int(0),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(9),
                        writeback_place: None,
                    },
                ],
            ),
        ),
        (
            "list.remove",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Unit,
                "remove",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "list.swap",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Unit,
                "swap",
                vec![
                    MirArg {
                        name: None,
                        value: Operand::Int(0),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(1),
                        writeback_place: None,
                    },
                ],
            ),
        ),
        (
            "list.contains",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::named("bool"),
                "contains",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(2),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "list.insert",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Unit,
                "insert",
                vec![
                    MirArg {
                        name: None,
                        value: Operand::Int(1),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(5),
                        writeback_place: None,
                    },
                ],
            ),
        ),
        (
            "list.clear",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Unit,
                "clear",
                Vec::new(),
            ),
        ),
        (
            "list.reverse",
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Unit,
                "reverse",
                Vec::new(),
            ),
        ),
        (
            "dict.len",
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::named("int64"),
                "len",
                Vec::new(),
            ),
        ),
        (
            "dict.is_empty",
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::named("bool"),
                "is_empty",
                Vec::new(),
            ),
        ),
        (
            "dict.get",
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "get",
                vec![MirArg {
                    name: None,
                    value: Operand::String("count".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "dict.set",
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "set",
                vec![
                    MirArg {
                        name: None,
                        value: Operand::String("count".to_string()),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(2),
                        writeback_place: None,
                    },
                ],
            ),
        ),
        (
            "dict.remove",
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "remove",
                vec![MirArg {
                    name: None,
                    value: Operand::String("count".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "dict membership",
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::named("bool"),
                "contains_key",
                vec![MirArg {
                    name: None,
                    value: Operand::String("count".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "dict.keys",
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Named("list".to_string(), vec![Type::named("str")]),
                "keys",
                Vec::new(),
            ),
        ),
        (
            "dict.values",
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                "values",
                Vec::new(),
            ),
        ),
        (
            "dict.items",
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Named(
                    "list".to_string(),
                    vec![Type::Tuple(vec![Type::named("str"), Type::named("int32")])],
                ),
                "items",
                Vec::new(),
            ),
        ),
        (
            "dict.clear",
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Unit,
                "clear",
                Vec::new(),
            ),
        ),
        (
            "set.len",
            module_with_main_member_call_result_type(
                "seen",
                set_ty.clone(),
                set_value.clone(),
                Type::named("int64"),
                "len",
                Vec::new(),
            ),
        ),
        (
            "set.is_empty",
            module_with_main_member_call_result_type(
                "seen",
                set_ty.clone(),
                set_value.clone(),
                Type::named("bool"),
                "is_empty",
                Vec::new(),
            ),
        ),
        (
            "set.contains",
            module_with_main_member_call_result_type(
                "seen",
                set_ty.clone(),
                set_value.clone(),
                Type::named("bool"),
                "contains",
                vec![MirArg {
                    name: None,
                    value: Operand::String("ready".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "set.add",
            module_with_main_member_call_result_type(
                "seen",
                set_ty.clone(),
                set_value.clone(),
                Type::Unit,
                "add",
                vec![MirArg {
                    name: None,
                    value: Operand::String("go".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "set.remove",
            module_with_main_member_call_result_type(
                "seen",
                set_ty.clone(),
                set_value.clone(),
                Type::Unit,
                "remove",
                vec![MirArg {
                    name: None,
                    value: Operand::String("ready".to_string()),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "Queue.put",
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Unit,
                        Type::Named("SendError".to_string(), vec![Type::named("int32")]),
                    ],
                ),
                "put",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
        ),
        (
            "Queue.get",
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "get",
                Vec::new(),
            ),
        ),
        (
            "Queue.close",
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value,
                Type::Unit,
                "close",
                Vec::new(),
            ),
        ),
        (
            "TaskGroup.cancel",
            module_with_main_member_call_result_type(
                "group",
                task_group_ty.clone(),
                task_group_value.clone(),
                Type::Unit,
                "cancel",
                Vec::new(),
            ),
        ),
        (
            "TaskGroup.close",
            module_with_main_member_call_result_type(
                "group",
                task_group_ty.clone(),
                task_group_value,
                Type::Unit,
                "close",
                Vec::new(),
            ),
        ),
    ];

    for (name, module) in cases {
        let object = emit_host_object(&module).expect("runtime member surface should compile");
        assert!(!object.is_empty(), "{name}");
    }
}

#[test]
fn direct_backend_resource_member_argument_errors_cover_network_and_process_paths() {
    let arg = |name: Option<&str>, value: Operand| MirArg {
        name: name.map(str::to_string),
        value,
        writeback_place: None,
    };
    let opaque_value = || Rvalue::Use(Operand::String("opaque-resource".to_string()));
    let named = |name: &str| Type::Named(name.to_string(), Vec::new());

    let cases = vec![
        (
            named("fs.File"),
            "read_all",
            vec![arg(None, Operand::Int(1))],
            "expected `read_all()` to take no arguments",
        ),
        (
            named("fs.File"),
            "read_bytes",
            vec![arg(None, Operand::Int(1))],
            "expected `read_bytes()` to take no arguments",
        ),
        (
            named("fs.File"),
            "write_all",
            Vec::new(),
            "expected `write_all()` to receive one argument",
        ),
        (
            named("fs.File"),
            "write_bytes",
            Vec::new(),
            "expected `write_bytes()` to receive one argument",
        ),
        (
            named("fs.File"),
            "flush",
            vec![arg(None, Operand::Int(1))],
            "expected `flush()` to take no arguments",
        ),
        (
            named("fs.File"),
            "close",
            vec![arg(None, Operand::Int(1))],
            "expected `close()` to take no arguments",
        ),
        (
            named("fs.File"),
            "unknown",
            Vec::new(),
            "does not know runtime member `fs.File.unknown`",
        ),
        (
            named("process.Child"),
            "stdin",
            vec![arg(None, Operand::Int(1))],
            "expected `stdin()` to take no arguments",
        ),
        (
            named("process.Child"),
            "stdout",
            vec![arg(None, Operand::Int(1))],
            "expected `stdout()` to take no arguments",
        ),
        (
            named("process.Child"),
            "stderr",
            vec![arg(None, Operand::Int(1))],
            "expected `stderr()` to take no arguments",
        ),
        (
            named("process.Child"),
            "kill",
            vec![arg(None, Operand::Int(1))],
            "expected `kill()` to take no arguments",
        ),
        (
            named("process.Child"),
            "terminate",
            vec![arg(None, Operand::Int(1))],
            "expected `terminate()` to take no arguments",
        ),
        (
            named("process.Child"),
            "close",
            vec![arg(None, Operand::Int(1))],
            "expected `close()` to take no arguments",
        ),
        (
            named("process.Child"),
            "unknown",
            Vec::new(),
            "does not know runtime member `process.Child.unknown`",
        ),
        (
            named("process.Pipe"),
            "read_all",
            vec![arg(None, Operand::Int(1))],
            "expected `read_all()` to take no arguments",
        ),
        (
            named("process.Pipe"),
            "read_bytes",
            Vec::new(),
            "expected `read_bytes()` to receive `max_bytes`",
        ),
        (
            named("process.Pipe"),
            "write_all",
            Vec::new(),
            "expected `write_all()` to receive `text`",
        ),
        (
            named("process.Pipe"),
            "write_bytes",
            Vec::new(),
            "expected `write_bytes()` to receive `bytes`",
        ),
        (
            named("process.Pipe"),
            "flush",
            vec![arg(None, Operand::Int(1))],
            "expected `flush()` to take no arguments",
        ),
        (
            named("process.Pipe"),
            "close",
            vec![arg(None, Operand::Int(1))],
            "expected `close()` to take no arguments",
        ),
        (
            named("process.Pipe"),
            "unknown",
            Vec::new(),
            "does not know runtime member `process.Pipe.unknown`",
        ),
        (
            named("process.Completed"),
            "status",
            vec![arg(None, Operand::Int(1))],
            "expected `status()` to take no arguments",
        ),
        (
            named("process.Completed"),
            "success",
            vec![arg(None, Operand::Int(1))],
            "expected `success()` to take no arguments",
        ),
        (
            named("process.Completed"),
            "stdout",
            vec![arg(None, Operand::Int(1))],
            "expected `stdout()` to take no arguments",
        ),
        (
            named("process.Completed"),
            "stderr",
            vec![arg(None, Operand::Int(1))],
            "expected `stderr()` to take no arguments",
        ),
        (
            named("process.Completed"),
            "stdout_bytes",
            vec![arg(None, Operand::Int(1))],
            "expected `stdout_bytes()` to take no arguments",
        ),
        (
            named("process.Completed"),
            "stderr_bytes",
            vec![arg(None, Operand::Int(1))],
            "expected `stderr_bytes()` to take no arguments",
        ),
        (
            named("process.Completed"),
            "check",
            vec![arg(None, Operand::Int(1))],
            "expected `check()` to take no arguments",
        ),
        (
            named("process.Completed"),
            "unknown",
            Vec::new(),
            "does not know runtime member `process.Completed.unknown`",
        ),
        (
            named("process.Supervisor"),
            "start",
            Vec::new(),
            "expected `start()` to receive `name`",
        ),
        (
            named("process.Supervisor"),
            "start",
            vec![arg(Some("name"), Operand::String("worker".to_string()))],
            "expected `start()` to receive `command`",
        ),
        (
            named("process.Supervisor"),
            "stop",
            vec![arg(None, Operand::Int(1))],
            "expected `stop()` to take no arguments",
        ),
        (
            named("process.Supervisor"),
            "is_empty",
            vec![arg(None, Operand::Int(1))],
            "expected `is_empty()` to take no arguments",
        ),
        (
            named("process.Supervisor"),
            "close",
            vec![arg(None, Operand::Int(1))],
            "expected `close()` to take no arguments",
        ),
        (
            named("process.Supervisor"),
            "unknown",
            Vec::new(),
            "does not know runtime member `process.Supervisor.unknown`",
        ),
        (
            named("net.TcpListener"),
            "local_addr",
            vec![arg(None, Operand::Int(1))],
            "expected `local_addr()` to take no arguments",
        ),
        (
            named("net.TcpListener"),
            "close",
            vec![arg(None, Operand::Int(1))],
            "expected `close()` to take no arguments",
        ),
        (
            named("net.TcpListener"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.TcpListener.unknown`",
        ),
        (
            named("net.TcpStream"),
            "read_bytes",
            Vec::new(),
            "expected `read_bytes()` to receive `max_bytes`",
        ),
        (
            named("net.TcpStream"),
            "read_exact",
            Vec::new(),
            "expected `read_exact()` to receive `count`",
        ),
        (
            named("net.TcpStream"),
            "write_all",
            Vec::new(),
            "expected `write_all()` to receive `text`",
        ),
        (
            named("net.TcpStream"),
            "write_bytes",
            Vec::new(),
            "expected `write_bytes()` to receive `bytes`",
        ),
        (
            named("net.TcpStream"),
            "flush",
            vec![arg(None, Operand::Int(1))],
            "expected `flush()` to take no arguments",
        ),
        (
            named("net.TcpStream"),
            "local_addr",
            vec![arg(None, Operand::Int(1))],
            "expected `local_addr()` to take no arguments",
        ),
        (
            named("net.TcpStream"),
            "peer_addr",
            vec![arg(None, Operand::Int(1))],
            "expected `peer_addr()` to take no arguments",
        ),
        (
            named("net.TcpStream"),
            "shutdown_read",
            vec![arg(None, Operand::Int(1))],
            "expected `shutdown_read()` to take no arguments",
        ),
        (
            named("net.TcpStream"),
            "shutdown_write",
            vec![arg(None, Operand::Int(1))],
            "expected `shutdown_write()` to take no arguments",
        ),
        (
            named("net.TcpStream"),
            "shutdown_both",
            vec![arg(None, Operand::Int(1))],
            "expected `shutdown_both()` to take no arguments",
        ),
        (
            named("net.TcpStream"),
            "close",
            vec![arg(None, Operand::Int(1))],
            "expected `close()` to take no arguments",
        ),
        (
            named("net.TcpStream"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.TcpStream.unknown`",
        ),
        (
            named("net.UdpSocket"),
            "send_text",
            Vec::new(),
            "expected `send_text()` to receive `address`",
        ),
        (
            named("net.UdpSocket"),
            "send_text",
            vec![arg(
                Some("address"),
                Operand::String("127.0.0.1:9".to_string()),
            )],
            "expected `send_text()` to receive `text`",
        ),
        (
            named("net.UdpSocket"),
            "send_bytes",
            Vec::new(),
            "expected `send_bytes()` to receive `address`",
        ),
        (
            named("net.UdpSocket"),
            "send_bytes",
            vec![arg(
                Some("address"),
                Operand::String("127.0.0.1:9".to_string()),
            )],
            "expected `send_bytes()` to receive `bytes`",
        ),
        (
            named("net.UdpSocket"),
            "recv",
            Vec::new(),
            "expected `recv()` to receive `max_bytes`",
        ),
        (
            named("net.UdpSocket"),
            "recv_from",
            Vec::new(),
            "expected `recv_from()` to receive `max_bytes`",
        ),
        (
            named("net.UdpSocket"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.UdpSocket.unknown`",
        ),
        (
            named("net.UdpDatagram"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.UdpDatagram.unknown`",
        ),
        (
            named("net.HttpListener"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.HttpListener.unknown`",
        ),
        (
            named("net.HttpExchange"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.HttpExchange.unknown`",
        ),
        (
            named("net.HttpResponse"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.HttpResponse.unknown`",
        ),
        (
            named("net.WebSocketListener"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.WebSocketListener.unknown`",
        ),
        (
            named("net.WebSocket"),
            "send_text",
            Vec::new(),
            "expected `send_text()` to receive `text`",
        ),
        (
            named("net.WebSocket"),
            "send_bytes",
            Vec::new(),
            "expected `send_bytes()` to receive `bytes`",
        ),
        (
            named("net.WebSocket"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.WebSocket.unknown`",
        ),
        (
            named("net.UnixListener"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.UnixListener.unknown`",
        ),
        (
            named("net.UnixStream"),
            "read_exact",
            Vec::new(),
            "expected `read_exact()` to receive `count`",
        ),
        (
            named("net.UnixStream"),
            "write_all",
            Vec::new(),
            "expected `write_all()` to receive `text`",
        ),
        (
            named("net.UnixStream"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.UnixStream.unknown`",
        ),
        (
            named("net.TlsListener"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.TlsListener.unknown`",
        ),
        (
            named("net.TlsStream"),
            "read_exact",
            Vec::new(),
            "expected `read_exact()` to receive `count`",
        ),
        (
            named("net.TlsStream"),
            "write_all",
            Vec::new(),
            "expected `write_all()` to receive `text`",
        ),
        (
            named("net.TlsStream"),
            "unknown",
            Vec::new(),
            "does not know runtime member `net.TlsStream.unknown`",
        ),
    ];

    for (object_ty, field, args, expected) in cases {
        let module = module_with_main_member_call_result_type(
            "resource",
            object_ty,
            opaque_value(),
            Type::named("int32"),
            field,
            args,
        );
        let error = emit_host_object(&module).expect_err("invalid resource member call");
        assert!(
            error.contains(expected),
            "expected `{expected}` in `{error}`"
        );
    }
}

#[test]
fn direct_backend_collection_member_argument_errors_cover_core_runtime_paths() {
    let arg = |name: Option<&str>, value: Operand| MirArg {
        name: name.map(str::to_string),
        value,
        writeback_place: None,
    };
    let opaque_value = || Rvalue::Use(Operand::String("opaque-resource".to_string()));
    let vec_int = || Type::Named("list".to_string(), vec![Type::named("int32")]);
    let map_string_int = || {
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        )
    };
    let set_string = || Type::Named("set".to_string(), vec![Type::named("str")]);
    let queue_int = || Type::Named("Queue".to_string(), vec![Type::named("int32")]);
    let task_int = || Type::Named("Task".to_string(), vec![Type::named("int32")]);

    let cases = vec![
        (
            Type::named("str"),
            "to_string",
            vec![arg(None, Operand::Int(1))],
            "expected `to_string()` to take no arguments",
        ),
        (
            Type::named("str"),
            "clone",
            vec![arg(None, Operand::Int(1))],
            "expected `clone()` to take no arguments",
        ),
        (
            Type::named("str"),
            "len",
            vec![arg(None, Operand::Int(1))],
            "expected `len()` to take no arguments",
        ),
        (
            Type::named("str"),
            "contains",
            Vec::new(),
            "expected `contains`() to receive one string argument",
        ),
        (
            Type::named("str"),
            "split",
            Vec::new(),
            "expected `split()` to receive one string argument",
        ),
        (
            Type::named("str"),
            "replace",
            vec![arg(None, Operand::String("from".to_string()))],
            "expected `replace()` to receive `from` and `to` string arguments",
        ),
        (
            Type::named("str"),
            "add",
            Vec::new(),
            "expected `add()` to receive one string argument",
        ),
        (
            Type::named("str"),
            "to_lower",
            vec![arg(None, Operand::Int(1))],
            "expected `to_lower()` to take no arguments",
        ),
        (
            Type::named("str"),
            "to_upper",
            vec![arg(None, Operand::Int(1))],
            "expected `to_upper()` to take no arguments",
        ),
        (
            Type::named("str"),
            "join",
            Vec::new(),
            "expected `join()` to receive one list argument",
        ),
        (
            Type::named("str"),
            "strip_prefix",
            Vec::new(),
            "expected `strip_prefix`() to receive one string argument",
        ),
        (
            Type::named("str"),
            "trim",
            vec![arg(None, Operand::Int(1))],
            "expected `trim()` to take no arguments",
        ),
        (
            Type::named("str"),
            "unknown",
            Vec::new(),
            "does not know runtime member `str.unknown`",
        ),
        (
            vec_int(),
            "len",
            vec![arg(None, Operand::Int(1))],
            "expected `len()` to take no arguments",
        ),
        (
            vec_int(),
            "is_empty",
            vec![arg(None, Operand::Int(1))],
            "expected `is_empty()` to take no arguments",
        ),
        (
            vec_int(),
            "append",
            Vec::new(),
            "expected `append()` to receive one argument",
        ),
        (
            vec_int(),
            "pop",
            vec![arg(None, Operand::Int(1)), arg(None, Operand::Int(2))],
            "expected `pop()` to receive at most one index",
        ),
        (
            vec_int(),
            "get",
            Vec::new(),
            "expected `get()` to receive one index argument",
        ),
        (
            vec_int(),
            "__index_option",
            Vec::new(),
            "expected internal optional vector indexing to receive one argument",
        ),
        (
            vec_int(),
            "__index",
            vec![arg(None, Operand::Int(0))],
            "expected internal vector indexing to receive index, line, and column",
        ),
        (
            vec_int(),
            "set",
            vec![arg(None, Operand::Int(0))],
            "expected `set()` to receive index and value",
        ),
        (
            vec_int(),
            "__set_index",
            vec![arg(None, Operand::Int(0))],
            "expected internal indexed assignment to receive index, value, line, and column",
        ),
        (
            vec_int(),
            "remove",
            Vec::new(),
            "expected `remove()` to receive one value argument",
        ),
        (
            vec_int(),
            "swap",
            vec![arg(None, Operand::Int(0))],
            "expected `swap()` to receive two index arguments",
        ),
        (
            vec_int(),
            "contains",
            Vec::new(),
            "expected `contains()` to receive one value argument",
        ),
        (
            vec_int(),
            "insert",
            vec![arg(None, Operand::Int(0))],
            "expected `insert()` to receive index and value",
        ),
        (
            vec_int(),
            "clear",
            vec![arg(None, Operand::Int(1))],
            "expected `clear()` to take no arguments",
        ),
        (
            vec_int(),
            "reverse",
            vec![arg(None, Operand::Int(1))],
            "expected `reverse()` to take no arguments",
        ),
        (
            vec_int(),
            "extend",
            Vec::new(),
            "expected `extend()` to receive one list argument",
        ),
        (
            vec_int(),
            "unknown",
            Vec::new(),
            "does not know runtime member `list.unknown`",
        ),
        (
            map_string_int(),
            "len",
            vec![arg(None, Operand::Int(1))],
            "expected `len()` to take no arguments",
        ),
        (
            map_string_int(),
            "is_empty",
            vec![arg(None, Operand::Int(1))],
            "expected `is_empty()` to take no arguments",
        ),
        (
            map_string_int(),
            "get",
            Vec::new(),
            "expected `get()` to receive one key argument",
        ),
        (
            map_string_int(),
            "__index",
            vec![arg(None, Operand::String("a".to_string()))],
            "expected internal map indexing to receive key, line, and column",
        ),
        (
            map_string_int(),
            "set",
            vec![arg(None, Operand::String("a".to_string()))],
            "expected `set()` to receive key and value",
        ),
        (
            map_string_int(),
            "__set_index",
            vec![arg(None, Operand::String("a".to_string()))],
            "expected internal map indexed assignment to receive key, value, line, and column",
        ),
        (
            map_string_int(),
            "remove",
            Vec::new(),
            "expected `remove()` to receive one key argument",
        ),
        (
            map_string_int(),
            "contains_key",
            Vec::new(),
            "expected `contains_key()` to receive one key argument",
        ),
        (
            map_string_int(),
            "keys",
            vec![arg(None, Operand::Int(1))],
            "expected `keys()` to take no arguments",
        ),
        (
            map_string_int(),
            "values",
            vec![arg(None, Operand::Int(1))],
            "expected `values()` to take no arguments",
        ),
        (
            map_string_int(),
            "items",
            vec![arg(None, Operand::Int(1))],
            "expected `items()` to take no arguments",
        ),
        (
            map_string_int(),
            "clear",
            vec![arg(None, Operand::Int(1))],
            "expected `clear()` to take no arguments",
        ),
        (
            map_string_int(),
            "update",
            Vec::new(),
            "expected `update()` to receive one dict argument",
        ),
        (
            map_string_int(),
            "unknown",
            Vec::new(),
            "does not know runtime member `dict.unknown`",
        ),
        (
            set_string(),
            "len",
            vec![arg(None, Operand::Int(1))],
            "expected `len()` to take no arguments",
        ),
        (
            set_string(),
            "is_empty",
            vec![arg(None, Operand::Int(1))],
            "expected `is_empty()` to take no arguments",
        ),
        (
            set_string(),
            "contains",
            Vec::new(),
            "expected `contains()` to receive one value argument",
        ),
        (
            set_string(),
            "add",
            Vec::new(),
            "expected `add()` to receive one value argument",
        ),
        (
            set_string(),
            "remove",
            Vec::new(),
            "expected `remove()` to receive one value argument",
        ),
        (
            set_string(),
            "__index_option",
            Vec::new(),
            "expected internal optional set indexing to receive one argument",
        ),
        (
            set_string(),
            "unknown",
            Vec::new(),
            "does not know runtime member `set.unknown`",
        ),
        (
            queue_int(),
            "put",
            Vec::new(),
            "expected `put()` to receive a value argument",
        ),
        (
            queue_int(),
            "try_put",
            Vec::new(),
            "expected `try_put()` to receive one argument",
        ),
        (
            queue_int(),
            "close",
            vec![arg(None, Operand::Int(1))],
            "expected `close()` to take no arguments",
        ),
        (
            queue_int(),
            "unknown",
            Vec::new(),
            "does not know runtime member `Queue.unknown`",
        ),
        (
            Type::named("TaskGroup"),
            "cancel",
            vec![arg(None, Operand::Int(1))],
            "expected `cancel()` to take no arguments",
        ),
        (
            Type::named("TaskGroup"),
            "close",
            vec![arg(None, Operand::Int(1))],
            "expected `close()` to take no arguments",
        ),
        (
            task_int(),
            "unknown",
            Vec::new(),
            "does not know runtime member `Task.unknown`",
        ),
    ];

    for (object_ty, field, args, expected) in cases {
        let module = module_with_main_member_call_result_type(
            "resource",
            object_ty,
            opaque_value(),
            Type::named("int32"),
            field,
            args,
        );
        let error = emit_host_object(&module).expect_err("invalid collection member call");
        assert!(
            error.contains(expected),
            "expected `{expected}` in `{error}`"
        );
    }
}

#[test]
fn direct_backend_resource_member_success_paths_cover_remaining_network_surfaces() {
    let arg = |name: Option<&str>, value: Operand| MirArg {
        name: name.map(str::to_string),
        value,
        writeback_place: None,
    };
    let opaque_value = || Rvalue::Use(Operand::String("opaque-resource".to_string()));
    let named = |name: &str| Type::Named(name.to_string(), Vec::new());
    let vec_uint8 = || Type::Named("list".to_string(), vec![Type::named("uint8")]);
    let option = |ty: Type| Type::Named("Option".to_string(), vec![ty]);
    let result = |ok: Type| {
        Type::Named(
            "Result".to_string(),
            vec![ok, Type::Named("io.Error".to_string(), Vec::new())],
        )
    };
    let process_result = |ok: Type| {
        Type::Named(
            "Result".to_string(),
            vec![ok, Type::Named("process.Error".to_string(), Vec::new())],
        )
    };
    let string_arg = |name: &str, value: &str| arg(Some(name), Operand::String(value.to_string()));

    let cases = vec![
        (
            named("fs.File"),
            "read_all",
            Vec::new(),
            result(Type::named("str")),
        ),
        (
            named("fs.File"),
            "read_bytes",
            Vec::new(),
            result(vec_uint8()),
        ),
        (
            named("fs.File"),
            "write_all",
            vec![string_arg("text", "payload")],
            result(Type::Unit),
        ),
        (
            named("fs.File"),
            "write_bytes",
            vec![string_arg("bytes", "payload")],
            result(Type::Unit),
        ),
        (named("fs.File"), "flush", Vec::new(), result(Type::Unit)),
        (named("fs.File"), "close", Vec::new(), Type::Unit),
        (
            named("process.Child"),
            "stdin",
            Vec::new(),
            option(named("process.Pipe")),
        ),
        (
            named("process.Child"),
            "stdout",
            Vec::new(),
            option(named("process.Pipe")),
        ),
        (
            named("process.Child"),
            "stderr",
            Vec::new(),
            option(named("process.Pipe")),
        ),
        (
            named("process.Child"),
            "wait",
            Vec::new(),
            named("process.Wait"),
        ),
        (
            named("process.Child"),
            "wait_or_none",
            Vec::new(),
            process_result(option(named("process.ExitStatus"))),
        ),
        (
            named("process.Child"),
            "wait_ok",
            Vec::new(),
            process_result(named("process.ExitStatus")),
        ),
        (
            named("process.Child"),
            "kill",
            Vec::new(),
            process_result(Type::Unit),
        ),
        (
            named("process.Child"),
            "terminate",
            Vec::new(),
            process_result(Type::Unit),
        ),
        (named("process.Child"), "close", Vec::new(), Type::Unit),
        (
            named("process.Pipe"),
            "read_all",
            Vec::new(),
            process_result(Type::named("str")),
        ),
        (
            named("process.Pipe"),
            "read_line",
            Vec::new(),
            process_result(option(Type::named("str"))),
        ),
        (
            named("process.Pipe"),
            "read_bytes",
            vec![arg(Some("max_bytes"), Operand::Int(4))],
            process_result(option(vec_uint8())),
        ),
        (
            named("process.Pipe"),
            "write_all",
            vec![string_arg("text", "payload")],
            process_result(Type::Unit),
        ),
        (
            named("process.Pipe"),
            "write_bytes",
            vec![string_arg("bytes", "payload")],
            process_result(Type::Unit),
        ),
        (
            named("process.Pipe"),
            "flush",
            Vec::new(),
            process_result(Type::Unit),
        ),
        (named("process.Pipe"), "close", Vec::new(), Type::Unit),
        (
            named("process.Completed"),
            "status",
            Vec::new(),
            named("process.ExitStatus"),
        ),
        (
            named("process.Completed"),
            "success",
            Vec::new(),
            Type::named("bool"),
        ),
        (
            named("process.Completed"),
            "stdout",
            Vec::new(),
            Type::named("str"),
        ),
        (
            named("process.Completed"),
            "stderr",
            Vec::new(),
            Type::named("str"),
        ),
        (
            named("process.Completed"),
            "stdout_bytes",
            Vec::new(),
            vec_uint8(),
        ),
        (
            named("process.Completed"),
            "stderr_bytes",
            Vec::new(),
            vec_uint8(),
        ),
        (
            named("process.Completed"),
            "check",
            Vec::new(),
            process_result(Type::Unit),
        ),
        (
            named("process.Supervisor"),
            "start",
            vec![
                string_arg("name", "worker"),
                arg(Some("command"), Operand::String("aura-worker".to_string())),
            ],
            process_result(Type::Unit),
        ),
        (
            named("process.Supervisor"),
            "start",
            vec![
                string_arg("name", "configured"),
                arg(Some("command"), Operand::String("aura-worker".to_string())),
                arg(Some("cwd"), Operand::Unit),
                arg(Some("env"), Operand::String("env".to_string())),
                arg(Some("stdin"), Operand::String("stdin".to_string())),
                arg(Some("stdout"), Operand::String("stdout".to_string())),
                arg(Some("stderr"), Operand::String("stderr".to_string())),
                arg(Some("restart"), Operand::String("restart".to_string())),
                arg(Some("backoff"), Operand::Duration(10)),
                arg(Some("max_restarts"), Operand::Int(1)),
                arg(Some("group"), Operand::Bool(false)),
            ],
            process_result(Type::Unit),
        ),
        (
            named("process.Supervisor"),
            "wait",
            Vec::new(),
            named("process.SupervisorWait"),
        ),
        (
            named("process.Supervisor"),
            "wait_or_none",
            Vec::new(),
            process_result(option(named("process.SupervisorEvent"))),
        ),
        (
            named("process.Supervisor"),
            "stop",
            Vec::new(),
            process_result(Type::Unit),
        ),
        (
            named("process.Supervisor"),
            "is_empty",
            Vec::new(),
            Type::named("bool"),
        ),
        (named("process.Supervisor"), "close", Vec::new(), Type::Unit),
        (
            named("net.TcpStream"),
            "local_addr",
            Vec::new(),
            result(Type::named("str")),
        ),
        (
            named("net.TcpStream"),
            "read_all",
            Vec::new(),
            result(Type::named("str")),
        ),
        (
            named("net.TcpStream"),
            "peer_addr",
            Vec::new(),
            result(Type::named("str")),
        ),
        (
            named("net.TcpStream"),
            "shutdown_read",
            Vec::new(),
            result(Type::Unit),
        ),
        (
            named("net.TcpStream"),
            "shutdown_write",
            Vec::new(),
            result(Type::Unit),
        ),
        (
            named("net.TcpStream"),
            "shutdown_both",
            Vec::new(),
            result(Type::Unit),
        ),
        (
            named("net.UdpSocket"),
            "send_bytes",
            vec![
                string_arg("address", "127.0.0.1:9"),
                string_arg("bytes", "payload"),
            ],
            result(Type::Unit),
        ),
        (
            named("net.UdpSocket"),
            "recv",
            vec![arg(Some("max_bytes"), Operand::Int(32))],
            result(option(vec_uint8())),
        ),
        (
            named("net.UdpSocket"),
            "peer_addr",
            Vec::new(),
            result(Type::named("str")),
        ),
        (named("net.UdpSocket"), "close", Vec::new(), Type::Unit),
        (named("net.UdpDatagram"), "bytes", Vec::new(), vec_uint8()),
        (
            named("net.UdpDatagram"),
            "text",
            Vec::new(),
            result(Type::named("str")),
        ),
        (
            named("net.HttpExchange"),
            "body_bytes",
            Vec::new(),
            vec_uint8(),
        ),
        (
            named("net.HttpExchange"),
            "respond_bytes",
            vec![
                arg(Some("status"), Operand::Int(200)),
                string_arg("bytes", "payload"),
                string_arg("headers", "headers"),
            ],
            result(Type::Unit),
        ),
        (
            named("net.HttpResponse"),
            "reason",
            Vec::new(),
            Type::named("str"),
        ),
        (
            named("net.HttpResponse"),
            "headers",
            Vec::new(),
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("str")],
            ),
        ),
        (named("net.HttpResponse"), "bytes", Vec::new(), vec_uint8()),
        (
            named("net.WebSocket"),
            "send_bytes",
            vec![string_arg("bytes", "payload")],
            result(Type::Unit),
        ),
        (
            named("net.WebSocket"),
            "recv_bytes",
            Vec::new(),
            result(option(vec_uint8())),
        ),
        (named("net.WebSocket"), "close", Vec::new(), Type::Unit),
        (
            named("net.UnixStream"),
            "read_exact",
            vec![arg(Some("count"), Operand::Int(4))],
            result(vec_uint8()),
        ),
        (
            named("net.UnixStream"),
            "write_all",
            vec![string_arg("text", "payload")],
            result(Type::Unit),
        ),
        (named("net.UnixStream"), "close", Vec::new(), Type::Unit),
        (
            named("net.TlsStream"),
            "read_line",
            Vec::new(),
            result(option(Type::named("str"))),
        ),
        (
            named("net.TlsStream"),
            "read_exact",
            vec![arg(Some("count"), Operand::Int(4))],
            result(vec_uint8()),
        ),
        (
            named("net.TlsStream"),
            "write_all",
            vec![string_arg("text", "payload")],
            result(Type::Unit),
        ),
        (named("net.TlsStream"), "close", Vec::new(), Type::Unit),
    ];

    for (object_ty, field, args, result_ty) in cases {
        let module = module_with_main_member_call_result_type(
            "resource",
            object_ty,
            opaque_value(),
            result_ty,
            field,
            args,
        );
        let object = emit_host_object(&module).expect("resource member call should compile");
        assert!(!object.is_empty(), "{field}");
    }
}

#[test]
fn direct_backend_runtime_member_arity_errors_cover_string_collection_and_runtime_paths() {
    let string_ty = Type::named("str");
    let string_value = Rvalue::Use(Operand::String("Aura".to_string()));
    let vec_ty = Type::Named("list".to_string(), vec![Type::named("int32")]);
    let vec_value = Rvalue::VecLiteral {
        element_type: Type::named("int32"),
        elements: vec![Operand::Int(1), Operand::Int(2)],
    };
    let map_ty = Type::Named(
        "dict".to_string(),
        vec![Type::named("str"), Type::named("int32")],
    );
    let map_value = Rvalue::MapLiteral {
        key_type: Type::named("str"),
        value_type: Type::named("int32"),
        entries: vec![MirMapEntry {
            key: Operand::String("count".to_string()),
            value: Operand::Int(1),
        }],
    };
    let set_ty = Type::Named("set".to_string(), vec![Type::named("str")]);
    let set_value = Rvalue::SetLiteral {
        element_type: Type::named("str"),
        elements: vec![Operand::String("ready".to_string())],
    };
    let channel_ty = Type::Named("Queue".to_string(), vec![Type::named("int32")]);
    let channel_value = Rvalue::Call {
        callee: CallTarget::Name("Queue".to_string()),
        args: Vec::new(),
    };
    let task_ty = Type::Named("Task".to_string(), vec![Type::named("int32")]);
    let task_value = Rvalue::Use(Operand::String("opaque-task".to_string()));
    let task_group_ty = Type::named("TaskGroup");
    let task_group_value = Rvalue::Call {
        callee: CallTarget::Name("TaskGroup".to_string()),
        args: Vec::new(),
    };
    let arg = |value| MirArg {
        name: None,
        value,
        writeback_place: None,
    };

    let cases = vec![
        (
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("int64"),
                "len",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `len()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("bool"),
                "contains",
                Vec::new(),
            ),
            "expected `contains`() to receive one string argument",
        ),
        (
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("str"),
                "replace",
                vec![MirArg {
                    name: None,
                    value: Operand::String("Aur".to_string()),
                    writeback_place: None,
                }],
            ),
            "expected `replace()` to receive `from` and `to` string arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("str"),
                "trim",
                vec![MirArg {
                    name: None,
                    value: Operand::String("x".to_string()),
                    writeback_place: None,
                }],
            ),
            "expected `trim()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("str"),
                "to_string",
                vec![arg(Operand::Int(1))],
            ),
            "expected `to_string()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::Named("list".to_string(), vec![Type::named("str")]),
                "split",
                Vec::new(),
            ),
            "expected `split()` to receive one string argument",
        ),
        (
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("str"),
                "add",
                Vec::new(),
            ),
            "expected `add()` to receive one string argument",
        ),
        (
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("str"),
                "to_lower",
                vec![arg(Operand::String("x".to_string()))],
            ),
            "expected `to_lower()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::named("str"),
                "to_upper",
                vec![arg(Operand::String("x".to_string()))],
            ),
            "expected `to_upper()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("str")]),
                "strip_prefix",
                Vec::new(),
            ),
            "expected `strip_prefix`() to receive one string argument",
        ),
        (
            module_with_main_member_call_result_type(
                "text",
                string_ty.clone(),
                string_value,
                Type::named("str"),
                "unknown",
                Vec::new(),
            ),
            "does not know runtime member `str.unknown`",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::named("bool"),
                "append",
                Vec::new(),
            ),
            "expected `append()` to receive one argument",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::named("bool"),
                "is_empty",
                vec![arg(Operand::Int(1))],
            ),
            "expected `is_empty()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::named("int32"),
                "pop",
                vec![arg(Operand::Int(1)), arg(Operand::Int(2))],
            ),
            "expected `pop()` to receive at most one index",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "get",
                Vec::new(),
            ),
            "expected `get()` to receive one index argument",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::named("int32"),
                "__index",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(0),
                    writeback_place: None,
                }],
            ),
            "expected internal vector indexing",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Unit,
                "__set_index",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(0),
                    writeback_place: None,
                }],
            ),
            "expected internal indexed assignment",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::named("bool"),
                "swap",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(0),
                    writeback_place: None,
                }],
            ),
            "expected `swap()` to receive two index arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "set",
                vec![arg(Operand::Int(0))],
            ),
            "expected `set()` to receive index and value",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "remove",
                Vec::new(),
            ),
            "expected `remove()` to receive one value argument",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::named("bool"),
                "contains",
                Vec::new(),
            ),
            "expected `contains()` to receive one value argument",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Unit,
                "reverse",
                vec![arg(Operand::Int(1))],
            ),
            "expected `reverse()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::Unit,
                "extend",
                Vec::new(),
            ),
            "expected `extend()` to receive one list argument",
        ),
        (
            module_with_main_member_call_result_type(
                "values",
                vec_ty.clone(),
                vec_value.clone(),
                Type::named("bool"),
                "unknown",
                Vec::new(),
            ),
            "does not know runtime member `list.unknown`",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::named("int32"),
                "__index",
                vec![MirArg {
                    name: None,
                    value: Operand::String("count".to_string()),
                    writeback_place: None,
                }],
            ),
            "expected internal map indexing",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Unit,
                "__set_index",
                vec![MirArg {
                    name: None,
                    value: Operand::String("count".to_string()),
                    writeback_place: None,
                }],
            ),
            "expected internal map indexed assignment",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::named("bool"),
                "contains_key",
                Vec::new(),
            ),
            "expected `contains_key()` to receive one key argument",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::named("bool"),
                "is_empty",
                vec![arg(Operand::Int(1))],
            ),
            "expected `is_empty()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "get",
                Vec::new(),
            ),
            "expected `get()` to receive one key argument",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "remove",
                Vec::new(),
            ),
            "expected `remove()` to receive one key argument",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Named("list".to_string(), vec![Type::named("str")]),
                "keys",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `keys()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                "items",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `items()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                "values",
                vec![arg(Operand::Int(1))],
            ),
            "expected `values()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::Unit,
                "clear",
                vec![arg(Operand::Int(1))],
            ),
            "expected `clear()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "counts",
                map_ty.clone(),
                map_value.clone(),
                Type::named("bool"),
                "unknown",
                Vec::new(),
            ),
            "does not know runtime member `dict.unknown`",
        ),
        (
            module_with_main_member_call_result_type(
                "seen",
                set_ty.clone(),
                set_value.clone(),
                Type::named("bool"),
                "contains",
                Vec::new(),
            ),
            "expected `contains()` to receive one value argument",
        ),
        (
            module_with_main_member_call_result_type(
                "seen",
                set_ty.clone(),
                set_value.clone(),
                Type::named("bool"),
                "is_empty",
                vec![arg(Operand::Int(1))],
            ),
            "expected `is_empty()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "seen",
                set_ty.clone(),
                set_value.clone(),
                Type::named("bool"),
                "add",
                Vec::new(),
            ),
            "expected `add()` to receive one value argument",
        ),
        (
            module_with_main_member_call_result_type(
                "seen",
                set_ty.clone(),
                set_value.clone(),
                Type::named("bool"),
                "remove",
                Vec::new(),
            ),
            "expected `remove()` to receive one value argument",
        ),
        (
            module_with_main_member_call_result_type(
                "seen",
                set_ty.clone(),
                set_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("str")]),
                "__index_option",
                Vec::new(),
            ),
            "expected internal optional set indexing",
        ),
        (
            module_with_main_member_call_result_type(
                "seen",
                set_ty.clone(),
                set_value.clone(),
                Type::named("bool"),
                "unknown",
                Vec::new(),
            ),
            "does not know runtime member `set.unknown`",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Unit,
                "put",
                Vec::new(),
            ),
            "expected `put()` to receive a value argument",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Unit,
                "put",
                vec![MirArg {
                    name: Some("delay".to_string()),
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `put()` arguments to use `value` and optional `timeout`, found `delay`",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Unit,
                "put",
                vec![
                    arg(Operand::Int(1)),
                    arg(Operand::Int(2)),
                    arg(Operand::Int(3)),
                ],
            ),
            "expected `put(value, timeout=...)`",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Unit,
                "try_put",
                Vec::new(),
            ),
            "expected `try_put()` to receive one argument",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Unit,
                "try_put",
                vec![MirArg {
                    name: Some("item".to_string()),
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `try_put()` to receive only `value=`",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "get",
                vec![MirArg {
                    name: Some("delay".to_string()),
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `get()` or `get(timeout=...)`",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "get",
                vec![
                    MirArg {
                        name: None,
                        value: Operand::Int(1),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(2),
                        writeback_place: None,
                    },
                ],
            ),
            "expected `get()` or `get(timeout=...)`",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "__get_in_task_group",
                Vec::new(),
            ),
            "expected internal `__get_in_task_group()` to receive one task-group argument",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "__get_with_registered_producers",
                vec![arg(Operand::Int(1))],
            ),
            "expected internal `__get_with_registered_producers()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "get_or_none",
                vec![MirArg {
                    name: Some("delay".to_string()),
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `get_or_none()` or `get_or_none(timeout=...)`",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "get_or_none",
                vec![arg(Operand::Int(1)), arg(Operand::Int(2))],
            ),
            "expected `get_or_none()` or `get_or_none(timeout=...)`",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::named("int32"),
                "get_or",
                Vec::new(),
            ),
            "expected `get_or()` to receive `default`",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::Unit,
                "close",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `close()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "jobs",
                channel_ty.clone(),
                channel_value.clone(),
                Type::named("bool"),
                "unknown",
                Vec::new(),
            ),
            "does not know runtime member `Queue.unknown`",
        ),
        (
            module_with_main_member_call_result_type(
                "task",
                task_ty.clone(),
                task_value.clone(),
                Type::named("int32"),
                "result",
                vec![MirArg {
                    name: Some("delay".to_string()),
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `result()` or `result(timeout=...)`",
        ),
        (
            module_with_main_member_call_result_type(
                "task",
                task_ty.clone(),
                task_value.clone(),
                Type::named("int32"),
                "result",
                vec![arg(Operand::Int(1)), arg(Operand::Int(2))],
            ),
            "expected `result()` or `result(timeout=...)`",
        ),
        (
            module_with_main_member_call_result_type(
                "task",
                task_ty.clone(),
                task_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "result_or_none",
                vec![MirArg {
                    name: Some("delay".to_string()),
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `result_or_none()` or `result_or_none(timeout=...)`",
        ),
        (
            module_with_main_member_call_result_type(
                "task",
                task_ty.clone(),
                task_value.clone(),
                Type::Named("Option".to_string(), vec![Type::named("int32")]),
                "result_or_none",
                vec![arg(Operand::Int(1)), arg(Operand::Int(2))],
            ),
            "expected `result_or_none()` or `result_or_none(timeout=...)`",
        ),
        (
            module_with_main_member_call_result_type(
                "task",
                task_ty.clone(),
                task_value.clone(),
                Type::named("int32"),
                "result_or",
                Vec::new(),
            ),
            "expected `result_or()` to receive `default`",
        ),
        (
            module_with_main_member_call_result_type(
                "task",
                task_ty,
                task_value,
                Type::named("bool"),
                "unknown",
                Vec::new(),
            ),
            "does not know runtime member `Task.unknown`",
        ),
        (
            module_with_main_member_call_result_type(
                "group",
                task_group_ty.clone(),
                task_group_value.clone(),
                Type::Unit,
                "cancel",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `cancel()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "group",
                task_group_ty.clone(),
                task_group_value.clone(),
                Type::Unit,
                "close",
                vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            ),
            "expected `close()` to take no arguments",
        ),
        (
            module_with_main_member_call_result_type(
                "group",
                task_group_ty,
                task_group_value,
                Type::named("bool"),
                "unknown",
                Vec::new(),
            ),
            "does not know runtime member `TaskGroup.unknown`",
        ),
    ];

    for (module, expected) in cases {
        let error = emit_host_object(&module).expect_err("invalid runtime member call");
        assert!(
            error.contains(expected),
            "expected `{expected}` in `{error}`"
        );
    }
}

#[test]
fn direct_backend_manual_wait_surface_compiles() {
    let source = r#"
def worker(queue: Queue[int32], value: int32) -> int32:
    match queue.put(value, timeout=2ms):
        case Result.Ok(_):
            pass
        case Result.Err(_):
            pass
    return value + 1

def main() -> int32:
    jobs = Queue[int32](capacity=1)
    with TaskGroup() as group:
        task = group.start(worker, jobs, 7)
        receive = jobs.get(timeout=5ms)
        one = wait_any([task], timeout=5ms)
        one_positional = wait_any([task], 5ms)
        all_now = wait_all([task])
        all = wait_all([task], timeout=5ms)
        all_positional = wait_all([task], 5ms)
        match receive:
            case QueueReceive.Item(value):
                print(value)
            case QueueReceive.Closed:
                print("closed")
            case QueueReceive.TimedOut:
                print("timedout")
            case QueueReceive.Cancelled:
                print("cancelled")
        match one:
            case WaitAny.Ready(index, result):
                print(index)
                print(result)
            case WaitAny.Error(index, message):
                print(index)
                print(message)
            case WaitAny.TimedOut:
                print("timedout")
            case WaitAny.Cancelled:
                print("cancelled")
        match one_positional:
            case WaitAny.Ready(index, result):
                print(index)
                print(result)
            case WaitAny.Error(index, message):
                print(index)
                print(message)
            case WaitAny.TimedOut:
                print("timedout")
            case WaitAny.Cancelled:
                print("cancelled")
        match all:
            case WaitAll.Ready(_):
                print("ready")
            case WaitAll.Error(index, message):
                print(index)
                print(message)
            case WaitAll.TimedOut:
                print("timedout")
            case WaitAll.Cancelled:
                print("cancelled")
        match all_positional:
            case WaitAll.Ready(_):
                print("ready")
            case WaitAll.Error(index, message):
                print(index)
                print(message)
            case WaitAll.TimedOut:
                print("timedout")
            case WaitAll.Cancelled:
                print("cancelled")
        match all_now:
            case WaitAll.Ready(_):
                print("ready")
            case WaitAll.Error(index, message):
                print(index)
                print(message)
            case WaitAll.TimedOut:
                print("timedout")
            case WaitAll.Cancelled:
                print("cancelled")
    return 0
"#;

    let module = lower_source_to_mir(source).expect("manual wait source should lower");
    assert!(!emit_host_object(&module)
        .expect("manual queue/task wait surface should compile directly")
        .is_empty());
}

#[test]
fn direct_backend_wait_helpers_cover_unknown_task_payload_fallback() {
    let string_vec = Type::Named("list".to_string(), vec![Type::named("str")]);
    let wait_all_unknown = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![
                MirLocalType {
                    name: "tasks".to_string(),
                    ty: string_vec.clone(),
                },
                MirLocalType {
                    name: "%wait".to_string(),
                    ty: Type::Named("WaitAll".to_string(), vec![Type::named("Unknown")]),
                },
            ],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::Assign {
                        target: "tasks".to_string(),
                        value: Rvalue::VecLiteral {
                            elements: vec![Operand::String("not-a-task".to_string())],
                            element_type: Type::named("str"),
                        },
                    },
                    Instruction::Assign {
                        target: "%wait".to_string(),
                        value: Rvalue::Call {
                            callee: CallTarget::Name("wait_all".to_string()),
                            args: vec![MirArg {
                                name: None,
                                value: Operand::Place("tasks".to_string()),
                                writeback_place: None,
                            }],
                        },
                    },
                ],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    assert!(!emit_host_object(&wait_all_unknown)
        .expect("direct wait helpers should preserve unknown task payload fallback")
        .is_empty());
}

#[test]
fn direct_backend_entry_thunk_handles_unit_parameters() {
    let unit_param_main = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: vec![MirParam {
                name: "marker".to_string(),
                passing: MirReceiverKind::Value,
                ty: Type::Unit,
                default_function: None,
            }],
            local_types: vec![MirLocalType {
                name: "marker".to_string(),
                ty: Type::Unit,
            }],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    assert!(!emit_host_object(&unit_param_main)
        .expect("direct entry thunk should lower Unit parameters")
        .is_empty());
}

#[test]
fn direct_backend_emits_object_for_process_member_surface_matrix() {
    let source = r#"
import process

def process_members() -> Result[None, process.Error]:
    completed = try process.run(["/bin/echo", "done"], stdout=process.pipe(), stderr=process.pipe(), timeout=2s, group=true)
    print(completed.status())
    print(completed.success())
    print(completed.stdout())
    print(completed.stdout_bytes())
    print(completed.stderr())
    print(completed.stderr_bytes())
    try completed.check()

    with child = try process.start(["/bin/cat"], stdin=process.pipe(), stdout=process.pipe(), stderr=process.pipe(), group=true):
        match child.stdin():
            case Option.Some(found_stdin):
                stdin_pipe: process.Pipe = found_stdin
                try stdin_pipe.write_all(text="ping\n", timeout=500ms)
                try stdin_pipe.write_bytes(bytes=[33 as uint8, 10 as uint8], timeout=500ms)
                try stdin_pipe.flush()
                stdin_pipe.close()
            case Option.None:
                pass
        match child.stdout():
            case Option.Some(found_stdout):
                stdout_pipe: process.Pipe = found_stdout
                print(try stdout_pipe.read_line(timeout=500ms))
                print(try stdout_pipe.read_bytes(max_bytes=4, timeout=500ms))
                print(try stdout_pipe.read_all())
                stdout_pipe.close()
            case Option.None:
                pass
        match child.stderr():
            case Option.Some(found_stderr):
                stderr_pipe: process.Pipe = found_stderr
                print(try stderr_pipe.read_all())
                stderr_pipe.close()
            case Option.None:
                pass
        print(child.wait(timeout=2s))
        print(try child.wait_or_none(timeout=1ms))
        child.close()

    with success = try process.start(["/bin/true"], stdout=process.null(), stderr=process.null(), group=true):
        print(try success.wait_ok(timeout=2s))

    with terminable = try process.start(["/bin/sleep", "10"], stdout=process.null(), stderr=process.null(), group=true):
        try terminable.terminate()
        print(terminable.wait(timeout=2s))

    with killable = try process.start(["/bin/sleep", "10"], stdout=process.null(), stderr=process.null(), group=true):
        try killable.kill()
        print(killable.wait(timeout=2s))

    with supervisor = process.supervisor():
        try supervisor.start(name="defaulted", command=["/usr/bin/false"])
        print(supervisor.wait(timeout=2s))
        env: dict[str, str] = {}
        try supervisor.start(name="explicit", command=["/usr/bin/false"], cwd=Option.None, env=env, stdin=process.null(), stdout=process.null(), stderr=process.null(), restart=process.RestartPolicy.Never, backoff=100ms, max_restarts=0, group=true)
        print(try supervisor.wait_or_none(timeout=2s))
        print(supervisor.is_empty())
        try supervisor.stop()
    return Result.Ok(None)

def main() -> int32:
    match process_members():
        case Result.Ok(_):
            return 0
        case Result.Err(error):
            print(error)
            return 1
"#;

    let module = lower_source_to_mir(source).expect("process member source should lower");
    assert!(!emit_host_object(&module)
        .expect("process member source should compile directly")
        .is_empty());
}

#[test]
fn direct_backend_task_result_payloads_support_plain_class_values() {
    let source = r#"
class Box:
    value: int32

def make_box() -> Box:
    return Box(value=7)

def main() -> int32:
    with TaskGroup() as group:
        task = group.start(make_box)
        match task.result():
            case TaskResult.Ready(box):
                print(box.value)
            case TaskResult.Error(_message):
                print(0)
            case TaskResult.TimedOut:
                print(0)
            case TaskResult.Cancelled:
                print(0)
    return 0
"#;

    let module = lower_source_to_mir(source).expect("plain-class task result source should lower");
    assert!(!emit_host_object(&module)
        .expect("plain-class task result source should compile directly")
        .is_empty());
}

#[test]
fn native_codegen_direct_error_paths_cover_missing_entry_wrapper_and_return_type_cases() {
    let source = r#"
class Counter:
    value: int32

    def current(self) -> int32:
        return self.value

def helper(value: int32) -> int32:
    return value + 1

def main() -> int32:
    return helper(1)
"#;
    let mir = lower_source_to_mir(source).expect("source should lower to MIR");

    let method = mir
        .functions
        .iter()
        .find(|function| function.receiver.is_some())
        .cloned()
        .expect("method should be lowered as a function");
    let helper = mir
        .functions
        .iter()
        .find(|function| function.name == "helper")
        .cloned()
        .expect("helper function should exist");
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .cloned()
        .expect("main function should exist");

    let mut method_codegen = NativeCodegen::new(&mir, "/tmp/native_codegen_errors.au", source)
        .expect("codegen should initialize");
    let thunk_error = method_codegen
        .define_function_thunk(&method)
        .expect_err("methods should still reject direct task-start thunks");
    assert!(thunk_error.contains("does not yet support task-start thunks for methods"));

    let mut broken_main = main.clone();
    broken_main.entry = "missing_block".to_string();
    let mut broken_entry_codegen =
        NativeCodegen::new(&mir, "/tmp/native_codegen_errors.au", source)
            .expect("codegen should initialize");
    let entry_error = broken_entry_codegen
        .define_function(&broken_main)
        .expect_err("missing entry blocks should be reported");
    assert!(entry_error.contains("could not find entry block `missing_block`"));

    let mut no_main_codegen = NativeCodegen::new(&mir, "/tmp/native_codegen_errors.au", source)
        .expect("codegen should initialize");
    no_main_codegen.functions.clear();
    let wrapper_error = no_main_codegen
        .define_main_wrapper()
        .expect_err("missing entrypoints should fail main wrapper generation");
    assert!(wrapper_error.contains("requires a `main` function or top-level script"));

    let mut missing_return_codegen =
        NativeCodegen::new(&mir, "/tmp/native_codegen_errors.au", source)
            .expect("codegen should initialize");
    missing_return_codegen
        .function_return_types
        .remove(&helper.name);
    let return_error = missing_return_codegen
        .define_function_thunk(&helper)
        .expect_err("missing thunk return types should fail");
    assert!(return_error.contains("does not know return type for `helper`"));
}

#[test]
fn direct_callable_objects_pin_defaults_capability_writebacks_and_task_handoff_abis() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run-pass");

    let defaults_path = fixtures.join("function_values_storage_and_calls.au");
    let defaults_mir =
        lower_path_to_mir(&defaults_path).expect("function-value defaults should lower");
    let offset = defaults_mir
        .functions
        .iter()
        .find(|function| function.name == "offset")
        .expect("offset function should exist");
    let default_function = offset.params[0]
        .default_function
        .as_deref()
        .expect("offset should retain its default-expression thunk");
    let defaults_object =
        emit_host_object(&defaults_mir).expect("function-value defaults should emit directly");
    let binder_references = object_function_referenced_symbols(
        &defaults_object,
        &mangle_default_binder_symbol("offset"),
    );
    assert!(
        binder_references
            .iter()
            .any(|symbol| symbol.contains(&mangle_symbol(default_function))),
        "the public function-value binder must call the selected function's concrete default thunk: {binder_references:?}"
    );
    assert!(
        binder_references
            .iter()
            .any(|symbol| symbol.contains("aura_direct_arg_buffer_store_owned")),
        "task-bound defaults must transfer their opaque handles into the scheduler-owned buffer: {binder_references:?}"
    );
    let default_main_references =
        object_function_referenced_symbols(&defaults_object, "aura_fn_main");
    for runtime_symbol in [
        "aura_direct_function_value",
        "aura_direct_function_bind_defaults",
        "aura_direct_function_call",
    ] {
        assert!(
            default_main_references
                .iter()
                .any(|symbol| symbol.contains(runtime_symbol)),
            "indirect calls must use `{runtime_symbol}`: {default_main_references:?}"
        );
    }
    for selected in ["first_default", "second_default"] {
        let function = defaults_mir
            .functions
            .iter()
            .find(|function| function.name == selected)
            .unwrap_or_else(|| panic!("selected default function `{selected}` should lower"));
        let default_function = function.params[0]
            .default_function
            .as_deref()
            .unwrap_or_else(|| panic!("`{selected}` should retain its concrete default thunk"));
        let binder_references = object_function_referenced_symbols(
            &defaults_object,
            &mangle_default_binder_symbol(selected),
        );
        assert!(
            binder_references
                .iter()
                .any(|symbol| symbol.contains(&mangle_symbol(default_function))),
            "the `{selected}` binder must call its own default thunk: {binder_references:?}"
        );
        for callable_symbol in [
            mangle_thunk_symbol(selected),
            mangle_default_binder_symbol(selected),
        ] {
            assert!(
                default_main_references
                    .iter()
                    .any(|symbol| symbol.contains(&callable_symbol)),
                "runtime selection must retain `{callable_symbol}` in main: {default_main_references:?}"
            );
        }
    }

    let capabilities_path = fixtures.join("function_value_inferred_capabilities.au");
    let capabilities_mir =
        lower_path_to_mir(&capabilities_path).expect("capability function values should lower");
    let capabilities_object = emit_host_object(&capabilities_mir)
        .expect("shared, mutable, and owned function values should emit directly");
    let mutable_thunk_references =
        object_function_referenced_symbols(&capabilities_object, &mangle_thunk_symbol("increment"));
    assert!(
        mutable_thunk_references
            .iter()
            .any(|symbol| symbol.contains(&mangle_symbol("increment"))),
        "the mutable-capability thunk must call its concrete function: {mutable_thunk_references:?}"
    );
    for runtime_symbol in [
        "aura_direct_instance_get_field",
        "aura_direct_instance_empty",
        "aura_direct_instance_set_field_owned",
    ] {
        assert!(
            mutable_thunk_references
                .iter()
                .any(|symbol| symbol.contains(runtime_symbol)),
            "the mutable plain-class parameter must round-trip through `{runtime_symbol}` for writeback: {mutable_thunk_references:?}"
        );
    }
    let owned_thunk_references =
        object_function_referenced_symbols(&capabilities_object, &mangle_thunk_symbol("take"));
    assert!(
        owned_thunk_references
            .iter()
            .any(|symbol| symbol.contains(&mangle_symbol("take"))),
        "the owned-capability thunk must call its concrete consuming function: {owned_thunk_references:?}"
    );

    let task_path = fixtures.join("function_values_task_targets.au");
    let task_mir = lower_path_to_mir(&task_path).expect("function-value task targets should lower");
    let task_object =
        emit_host_object(&task_mir).expect("function-value task targets should emit directly");
    let task_main_references = object_function_referenced_symbols(&task_object, "aura_fn_main");
    for runtime_symbol in [
        "aura_direct_task_arg_buffer_guard",
        "aura_direct_function_bind_defaults",
        "aura_direct_task_arg_buffer_disarm",
        "aura_direct_start_task_function_with_frames",
    ] {
        assert!(
            task_main_references
                .iter()
                .any(|symbol| symbol.contains(runtime_symbol)),
            "callable Task lowering must use `{runtime_symbol}`: {task_main_references:?}"
        );
    }
    for selected in ["first_default", "second_default"] {
        let function = task_mir
            .functions
            .iter()
            .find(|function| function.name == selected)
            .unwrap_or_else(|| panic!("selected task function `{selected}` should lower"));
        let default_function = function.params[0]
            .default_function
            .as_deref()
            .unwrap_or_else(|| panic!("`{selected}` should retain its task default thunk"));
        let binder_references = object_function_referenced_symbols(
            &task_object,
            &mangle_default_binder_symbol(selected),
        );
        assert!(
            binder_references
                .iter()
                .any(|symbol| symbol.contains(&mangle_symbol(default_function))),
            "task selection must keep `{selected}` bound to its concrete default: {binder_references:?}"
        );
        for callable_symbol in [
            mangle_thunk_symbol(selected),
            mangle_default_binder_symbol(selected),
        ] {
            assert!(
                task_main_references
                    .iter()
                    .any(|symbol| symbol.contains(&callable_symbol)),
                "the selected task callable must carry `{callable_symbol}`: {task_main_references:?}"
            );
        }
    }
    for legacy_symbol in [
        "aura_direct_start_task_call_with_frames",
        "aura_direct_start_task_call",
    ] {
        assert!(
            !task_main_references
                .iter()
                .any(|symbol| symbol.ends_with(legacy_symbol)),
            "function-value task lowering must not fall back to `{legacy_symbol}`"
        );
    }
}

#[test]
fn direct_closure_objects_pin_environment_call_default_binding_and_task_abis() {
    let source = r#"
def main() -> int32:
    offset: int64 = 5
    factor: int64 = 2
    worker: def(int64) -> int64 = lambda value: value * factor + offset
    direct: int64 = worker(3)
    values: list[int64] = [1, 2]
    mapped: list[int64] = values.map(worker)
    with TaskGroup() as group:
        task: Task[int64] = group.start(worker, 7)
    return 0
"#;
    let module = lower_source_to_mir(source)
        .expect("a capturing closure should lower across call, callback, and task contexts");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let (lifted_name, captures) = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Closure {
                        function, captures, ..
                    },
                ..
            } => Some((function, captures)),
            _ => None,
        })
        .expect("worker should construct a closure environment");
    assert_eq!(
        captures
            .iter()
            .map(|capture| capture.name.as_str())
            .collect::<Vec<_>>(),
        vec!["factor", "offset"],
        "native environment slots must retain MIR lexical-first-use order"
    );
    let lifted = module
        .functions
        .iter()
        .find(|function| function.name == *lifted_name)
        .expect("the closure body should be emitted as a direct-callable MIR function");
    assert_eq!(
        lifted
            .params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>(),
        vec!["factor", "offset", "value"],
        "the native thunk ABI must receive hidden captures before public parameters"
    );

    let object =
        emit_host_object(&module).expect("closure calls, callbacks, and task handoff should emit");
    let main_references = object_function_referenced_symbols(&object, "aura_fn_main");
    for runtime_symbol in [
        "aura_direct_function_value",
        "aura_direct_arg_buffer_new",
        "aura_direct_arg_buffer_store_owned",
        "aura_direct_closure_value",
        "aura_direct_function_bind_defaults",
        "aura_direct_function_call",
        "aura_direct_task_arg_buffer_guard",
        "aura_direct_task_arg_buffer_disarm",
        "aura_direct_start_task_function_with_frames",
    ] {
        assert!(
            main_references
                .iter()
                .any(|symbol| symbol.contains(runtime_symbol)),
            "closure lowering must use `{runtime_symbol}`: {main_references:?}"
        );
    }
    for callable_symbol in [
        mangle_thunk_symbol(lifted_name),
        mangle_default_binder_symbol(lifted_name),
    ] {
        assert!(
            main_references
                .iter()
                .any(|symbol| symbol.contains(&callable_symbol)),
            "closure construction must retain `{callable_symbol}`: {main_references:?}"
        );
    }
    let thunk_references =
        object_function_referenced_symbols(&object, &mangle_thunk_symbol(lifted_name));
    assert!(
        thunk_references
            .iter()
            .any(|symbol| symbol.contains(&mangle_symbol(lifted_name))),
        "the closure thunk must call its lifted implementation: {thunk_references:?}"
    );
    for legacy_symbol in [
        "aura_direct_function_thunk",
        "aura_direct_function_default_binder",
        "aura_direct_start_task_call_with_frames",
    ] {
        assert!(
            !main_references
                .iter()
                .any(|symbol| symbol.contains(legacy_symbol)),
            "closure objects must not regress to legacy ABI `{legacy_symbol}`"
        );
    }
}

#[test]
fn direct_closure_invalid_mir_reports_thunk_binder_call_and_task_contracts() {
    let source = r#"
def main() -> int32:
    offset: int32 = 5
    worker: def(int32) -> int32 = lambda value: value + offset
    direct: int32 = worker(1)
    with TaskGroup() as group:
        task: Task[int32] = group.start(worker, 2)
    return direct
"#;
    let module =
        lower_source_to_mir(source).expect("closure call and task source should lower to MIR");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .cloned()
        .expect("main should lower");
    let lifted_name = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Closure { function, .. },
                ..
            } => Some(function.clone()),
            _ => None,
        })
        .expect("worker should retain its lifted implementation");

    let mut missing_thunk =
        NativeCodegen::new(&module, "/tmp/direct_closure_missing_thunk.au", source)
            .expect("codegen should initialize");
    missing_thunk.function_thunks.remove(&lifted_name);
    let thunk_error = missing_thunk
        .define_function(&main)
        .expect_err("closure construction must require the lifted thunk");
    assert!(
        thunk_error.contains(&format!("does not know function thunk for `{lifted_name}`")),
        "{thunk_error}"
    );

    let mut missing_binder =
        NativeCodegen::new(&module, "/tmp/direct_closure_missing_binder.au", source)
            .expect("codegen should initialize");
    missing_binder.function_default_binders.remove(&lifted_name);
    let binder_error = missing_binder
        .define_function(&main)
        .expect_err("closure construction must require the lifted default binder");
    assert!(
        binder_error.contains(&format!(
            "does not know function default binder for `{lifted_name}`"
        )),
        "{binder_error}"
    );

    let mut invalid_call = module.clone();
    let invalid_call_main = invalid_call
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let call_args = invalid_call_main
        .blocks
        .iter_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Value(Operand::Place(place)),
                        args,
                    },
                ..
            } if place == "worker" => Some(args),
            _ => None,
        })
        .expect("worker invocation should use value-call MIR");
    call_args[0].name = Some("missing".to_string());
    let call_error =
        emit_host_object(&invalid_call).expect_err("unknown closure call parameters must fail");
    assert!(
        call_error.contains("direct backend function value has no parameter named `missing`"),
        "{call_error}"
    );

    let mut invalid_task = module.clone();
    let invalid_task_main = invalid_task
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let task_args = invalid_task_main
        .blocks
        .iter_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::StartTask { function, args, .. },
                ..
            } if matches!(&*function, Operand::Place(place) if place == "worker") => Some(args),
            _ => None,
        })
        .expect("worker task start should retain function-value arguments");
    task_args[0].name = Some("missing".to_string());
    let task_error =
        emit_host_object(&invalid_task).expect_err("unknown closure task parameters must fail");
    assert!(
        task_error.contains("task function value has no parameter named `missing`"),
        "{task_error}"
    );

    let mut consuming_target = module.clone();
    let consuming_main = consuming_target
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let call_target = consuming_main
        .blocks
        .iter_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Call { callee, .. },
                ..
            } if matches!(&*callee, CallTarget::Value(Operand::Place(place)) if place == "worker") => {
                Some(callee)
            }
            _ => None,
        })
        .expect("worker invocation should use a non-consuming call target");
    *call_target = CallTarget::Value(Operand::MovePlace("worker".to_string()));
    let consuming_error = emit_host_object(&consuming_target)
        .expect_err("indirect call targets must not consume their callable place");
    assert!(
        consuming_error.contains(
            "only permits `MovePlace` in consuming contexts, not in an indirect-call target"
        ),
        "{consuming_error}"
    );
}

#[test]
fn native_codegen_function_value_signature_errors_are_precise() {
    fn contract(
        name: &str,
        passing: crate::ast::ReceiverKind,
    ) -> crate::sema::FunctionParamContract {
        crate::sema::FunctionParamContract {
            name: name.to_string(),
            ty: Type::named("int32"),
            passing,
            has_default: false,
            default_erased: false,
        }
    }

    fn signature(param: crate::sema::FunctionParamContract) -> Type {
        Type::Function {
            params: vec![param],
            return_type: Box::new(Type::named("int32")),
        }
    }

    fn module(signature: Type, args: Vec<MirArg>) -> crate::mir::MirModule {
        crate::mir::MirModule {
            constants: Vec::new(),
            functions: vec![
                MirFunction {
                    name: "worker".to_string(),
                    module_name: "<test>".to_string(),
                    source_path: None,
                    span: Span::new(1, 1),
                    receiver: None,
                    params: vec![MirParam {
                        name: "value".to_string(),
                        passing: MirReceiverKind::Value,
                        ty: Type::named("int32"),
                        default_function: None,
                    }],
                    local_types: vec![MirLocalType {
                        name: "value".to_string(),
                        ty: Type::named("int32"),
                    }],
                    return_type: Type::named("int32"),
                    entry: "entry".to_string(),
                    blocks: vec![BasicBlock {
                        label: "entry".to_string(),
                        instructions: Vec::new(),
                        terminator: Terminator::Return(Operand::Place("value".to_string())),
                    }],
                },
                MirFunction {
                    name: "main".to_string(),
                    module_name: "<test>".to_string(),
                    source_path: None,
                    span: Span::new(4, 1),
                    receiver: None,
                    params: Vec::new(),
                    local_types: vec![MirLocalType {
                        name: "%result".to_string(),
                        ty: Type::named("int32"),
                    }],
                    return_type: Type::named("int32"),
                    entry: "entry".to_string(),
                    blocks: vec![BasicBlock {
                        label: "entry".to_string(),
                        instructions: vec![Instruction::Assign {
                            target: "%result".to_string(),
                            value: Rvalue::Call {
                                callee: CallTarget::Value(Operand::Function {
                                    name: "worker".to_string(),
                                    signature: Box::new(signature),
                                }),
                                args,
                            },
                        }],
                        terminator: Terminator::Return(Operand::Int(0)),
                    }],
                },
            ],
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        }
    }

    let argument = |name: Option<&str>, writeback_place: Option<&str>| MirArg {
        name: name.map(str::to_string),
        value: Operand::Int(1),
        writeback_place: writeback_place.map(str::to_string),
    };
    let value_signature = || signature(contract("value", crate::ast::ReceiverKind::Value));
    let mutable_signature = || signature(contract("value", crate::ast::ReceiverKind::BorrowMut));
    let cases = vec![
        (
            "non-function signature",
            Type::named("int32"),
            vec![argument(None, None)],
            "direct backend expected an indirect function value",
        ),
        (
            "too many arguments",
            value_signature(),
            vec![argument(None, None), argument(None, None)],
            "direct backend expected at most 1 indirect-call arguments, found 2",
        ),
        (
            "unknown named argument",
            value_signature(),
            vec![argument(Some("missing"), None)],
            "direct backend function value has no parameter named `missing`",
        ),
        (
            "duplicate named argument",
            Type::Function {
                params: vec![
                    contract("value", crate::ast::ReceiverKind::Value),
                    contract("other", crate::ast::ReceiverKind::Value),
                ],
                return_type: Box::new(Type::named("int32")),
            },
            vec![argument(Some("value"), None), argument(Some("value"), None)],
            "direct backend received duplicate indirect-call arguments",
        ),
        (
            "mutable argument without writeback",
            mutable_signature(),
            vec![argument(None, None)],
            "direct backend indirect mutable argument 1 has no writeback place",
        ),
        (
            "writeback on a value argument",
            value_signature(),
            vec![argument(None, Some("value"))],
            "direct backend indirect argument 1 unexpectedly requests writeback",
        ),
    ];

    for (label, signature, args, expected) in cases {
        let error = match emit_host_object(&module(signature, args)) {
            Ok(_) => panic!("{label} should be rejected"),
            Err(error) => error,
        };
        assert!(
            error.contains(expected),
            "{label} should report `{expected}`, found `{error}`"
        );
    }
}

#[test]
fn native_codegen_function_value_binding_skips_named_slots_for_later_positionals() {
    let source = r#"
def combine(first: int32, second: int32) -> int32:
    return first + second

def main() -> int32:
    callback = combine
    return callback(1, 2)
"#;
    let mut mir = lower_source_to_mir(source).expect("mixed binding source should lower");
    let main = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let indirect_args = main
        .blocks
        .iter_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Value(_),
                        args,
                    },
                ..
            } => Some(args),
            _ => None,
        })
        .expect("main should contain the indirect callback call");
    assert_eq!(indirect_args.len(), 2);
    indirect_args[0].name = Some("first".to_string());

    let object = emit_host_object(&mir)
        .expect("a positional argument after a named slot should bind the next free parameter");
    let main_references = object_function_referenced_symbols(&object, "aura_fn_main");
    for runtime_symbol in [
        "aura_direct_function_bind_defaults",
        "aura_direct_function_call",
    ] {
        assert!(
            main_references
                .iter()
                .any(|symbol| symbol.contains(runtime_symbol)),
            "mixed binding must retain the indirect callable ABI through `{runtime_symbol}`: {main_references:?}"
        );
    }
}

#[test]
fn native_codegen_default_binder_reports_method_and_metadata_failures() {
    let source = r#"
class Counter:
    value: int32

    def current(self) -> int32:
        return self.value

def target(value: int32 = 7) -> int32:
    return value

def main() -> int32:
    callback = target
    return callback()
"#;
    let mir = lower_source_to_mir(source).expect("defaulted function value should lower");
    let method = mir
        .functions
        .iter()
        .find(|function| function.receiver.is_some())
        .cloned()
        .expect("class method should lower");
    let target = mir
        .functions
        .iter()
        .find(|function| function.name == "target")
        .cloned()
        .expect("defaulted target should lower");
    let default_function = target.params[0]
        .default_function
        .as_deref()
        .expect("target should have a generated default thunk")
        .to_string();

    let mut method_codegen = NativeCodegen::new(&mir, "/tmp/function_default_binder.au", source)
        .expect("codegen should initialize");
    let method_error = method_codegen
        .define_function_default_binder(&method)
        .expect_err("methods are not first-class callable values");
    assert_eq!(
        method_error,
        "direct backend cannot build a function-value default binder for method `Counter.current`"
    );

    let mut missing_default_codegen =
        NativeCodegen::new(&mir, "/tmp/function_default_binder.au", source)
            .expect("codegen should initialize");
    missing_default_codegen.functions.remove(&default_function);
    let missing_default_error = missing_default_codegen
        .define_function_default_binder(&target)
        .expect_err("missing generated defaults should be diagnosed");
    assert_eq!(
        missing_default_error,
        format!("direct backend is missing default function `{default_function}` for `target`")
    );

    let mut missing_param_codegen =
        NativeCodegen::new(&mir, "/tmp/function_default_binder.au", source)
            .expect("codegen should initialize");
    missing_param_codegen.function_param_types.remove("target");
    let missing_param_error = missing_param_codegen
        .define_function_default_binder(&target)
        .expect_err("missing parameter ABI metadata should be diagnosed");
    assert_eq!(
        missing_param_error,
        "direct backend is missing parameter 1 metadata for `target`"
    );
}

#[test]
fn direct_backend_builtin_call_surface_compiles_across_success_and_error_matrix() {
    let success_source = r#"
import fs
import io
import net

def main() -> int32:
    print(7)
    print(3.5)
    print(true)
    print(None)
    text = "  Aura repo  "
    print(text)
    print("Aura " + "repo")
    print(f"value={text}")
    write_status = io.write("status")
    flushed = io.flush()
    line = io.read_line()
    entries = fs.read_dir(".")
    jobs = Queue[int32]()
    group = TaskGroup()
    ready = cancelled()
    sleep(0ms)
    value = abs(-7)
    floor = min(1, 2)
    ceil = max(1, 2)
    root = sqrt(9.0)
    rounded = round(2.5)
    pair = divmod(-7, 3)
    print(pair)
    parsed32 = parse_int32("7")
    parsed64 = parse_int64("7")
    parsedf = parse_float64("7.0")
    headers: dict[str, str] = {"X-Test": "ok"}
    body: list[uint8] = [1 as uint8, 2 as uint8]
    http_bytes = net.http_request_bytes_timeout("POST", "http://127.0.0.1/", body, headers, 5ms)
    values: list[int32] = list[int32]()
    names: set[str] = set[str]()
    counts: dict[str, int32] = dict[str, int32]()
    short = range(3)
    long = range(start=1, stop=4)
    print(ready)
    return (value + floor + ceil) as int32 + root as int32 + rounded as int32
"#;
    let success_mir =
        lower_source_to_mir(success_source).expect("builtin matrix source should lower");
    let object =
        emit_host_object(&success_mir).expect("builtin matrix source should emit direct code");
    assert!(!object.is_empty());

    let error_cases = [
        (
            "print missing arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("print".to_string()),
                args: vec![],
            }),
            "expected `print` to receive one argument",
        ),
        (
            "channel extra arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("Queue".to_string()),
                args: vec![
                    MirArg {
                        name: None,
                        value: Operand::Int(1),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(2),
                        writeback_place: None,
                    },
                ],
            }),
            "expected `Queue()` to take at most one capacity argument",
        ),
        (
            "channel bad named arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("Queue".to_string()),
                args: vec![MirArg {
                    name: Some("size".to_string()),
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            }),
            "expected `Queue()` to receive only `capacity=`",
        ),
        (
            "tasks extra arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("TaskGroup".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            }),
            "expected `TaskGroup()` to take no arguments",
        ),
        (
            "cancelled extra arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("cancelled".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            }),
            "expected `cancelled()` to take no arguments",
        ),
        (
            "sleep missing arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("sleep".to_string()),
                args: vec![],
            }),
            "expected `sleep()` to receive one duration argument",
        ),
        (
            "wait_any duplicate tasks",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("wait_any".to_string()),
                args: vec![
                    MirArg {
                        name: Some("tasks".to_string()),
                        value: Operand::Unit,
                        writeback_place: None,
                    },
                    MirArg {
                        name: Some("tasks".to_string()),
                        value: Operand::Unit,
                        writeback_place: None,
                    },
                ],
            }),
            "expected `wait_any(tasks, timeout=...)`",
        ),
        (
            "wait_all duplicate timeout",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("wait_all".to_string()),
                args: vec![
                    MirArg {
                        name: Some("tasks".to_string()),
                        value: Operand::Unit,
                        writeback_place: None,
                    },
                    MirArg {
                        name: Some("timeout".to_string()),
                        value: Operand::Unit,
                        writeback_place: None,
                    },
                    MirArg {
                        name: Some("timeout".to_string()),
                        value: Operand::Unit,
                        writeback_place: None,
                    },
                ],
            }),
            "expected `wait_all(tasks, timeout=...)`",
        ),
        (
            "wait_any unknown arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("wait_any".to_string()),
                args: vec![MirArg {
                    name: Some("jobs".to_string()),
                    value: Operand::Unit,
                    writeback_place: None,
                }],
            }),
            "expected `wait_any(tasks, timeout=...)`",
        ),
        (
            "abs missing arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("abs".to_string()),
                args: vec![],
            }),
            "expected `abs()` to receive one argument",
        ),
        (
            "parse_int32 missing arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("parse_int32".to_string()),
                args: vec![],
            }),
            "expected `parse_int32`() to receive one string argument",
        ),
        (
            "min missing arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("min".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            }),
            "expected `min`() to receive two arguments",
        ),
        (
            "sqrt missing arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("sqrt".to_string()),
                args: vec![],
            }),
            "expected `sqrt()` to receive one argument",
        ),
        (
            "round missing arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("round".to_string()),
                args: vec![],
            }),
            "direct backend is missing a builtin argument",
        ),
        (
            "divmod missing arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("divmod".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            }),
            "direct backend is missing a builtin argument",
        ),
        (
            "list extra arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("list".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            }),
            "expected `list`() to take no arguments",
        ),
        (
            "range too many args",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("range".to_string()),
                args: vec![
                    MirArg {
                        name: None,
                        value: Operand::Int(1),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(2),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(3),
                        writeback_place: None,
                    },
                ],
            }),
            "expected `range()` to receive one or two arguments",
        ),
        (
            "range too many mixed args",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("range".to_string()),
                args: vec![
                    MirArg {
                        name: Some("start".to_string()),
                        value: Operand::Int(0),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(1),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(2),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(3),
                        writeback_place: None,
                    },
                ],
            }),
            "expected `range()` to receive one or two arguments",
        ),
        (
            "range bad named arg",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("range".to_string()),
                args: vec![
                    MirArg {
                        name: Some("middle".to_string()),
                        value: Operand::Int(1),
                        writeback_place: None,
                    },
                    MirArg {
                        name: Some("stop".to_string()),
                        value: Operand::Int(3),
                        writeback_place: None,
                    },
                ],
            }),
            "does not recognize `range()` argument `middle`",
        ),
        (
            "range missing stop",
            module_with_main_call(Rvalue::Call {
                callee: CallTarget::Name("range".to_string()),
                args: vec![MirArg {
                    name: Some("start".to_string()),
                    value: Operand::Int(1),
                    writeback_place: None,
                }],
            }),
            "expected `range()` to receive a `stop` argument",
        ),
    ];

    for (label, module, expected) in error_cases {
        let error = emit_host_object(&module)
            .expect_err(&format!("{label} should be rejected by direct codegen"));
        assert!(
            error.contains(expected),
            "{label} reported `{error}` instead of containing `{expected}`"
        );
    }

    for (label, module) in [
        (
            "untyped dict constructor",
            module_with_main_call_result_type(
                Rvalue::Call {
                    callee: CallTarget::Name("dict".to_string()),
                    args: Vec::new(),
                },
                Type::Named(
                    "dict".to_string(),
                    vec![Type::named("str"), Type::named("int32")],
                ),
            ),
        ),
        (
            "opaque unary not",
            module_with_main_call_result_type(
                Rvalue::Unary {
                    op: UnaryOp::Not,
                    value: Operand::String("truthy".to_string()),
                    span: Span::new(1, 1),
                },
                Type::named("Unknown"),
            ),
        ),
        (
            "identity int32 cast",
            module_with_main_call(Rvalue::Cast {
                value: Operand::Int(7),
                ty: Type::named("int32"),
                span: Span::new(1, 1),
            }),
        ),
        (
            "wait_all without timeout",
            module_with_main_call_result_type(
                Rvalue::Call {
                    callee: CallTarget::Name("wait_all".to_string()),
                    args: vec![MirArg {
                        name: None,
                        value: Operand::Unit,
                        writeback_place: None,
                    }],
                },
                Type::Named("WaitAll".to_string(), vec![Type::named("Unknown")]),
            ),
        ),
        (
            "wait_any positional timeout",
            module_with_main_call_result_type(
                Rvalue::Call {
                    callee: CallTarget::Name("wait_any".to_string()),
                    args: vec![
                        MirArg {
                            name: None,
                            value: Operand::Unit,
                            writeback_place: None,
                        },
                        MirArg {
                            name: None,
                            value: Operand::Duration(5),
                            writeback_place: None,
                        },
                    ],
                },
                Type::Named("WaitAny".to_string(), vec![Type::named("Unknown")]),
            ),
        ),
        (
            "bool to int32 coercion",
            module_with_main_call(Rvalue::Use(Operand::Bool(true))),
        ),
        (
            "unit to int32 coercion",
            module_with_main_call(Rvalue::Use(Operand::Unit)),
        ),
        (
            "boolean and",
            module_with_main_call_result_type(
                Rvalue::Binary {
                    op: BinaryOp::And,
                    left: Operand::Bool(true),
                    right: Operand::Bool(false),
                    span: Span::new(1, 1),
                },
                Type::named("bool"),
            ),
        ),
        (
            "boolean or",
            module_with_main_call_result_type(
                Rvalue::Binary {
                    op: BinaryOp::Or,
                    left: Operand::Bool(true),
                    right: Operand::Bool(false),
                    span: Span::new(1, 1),
                },
                Type::named("bool"),
            ),
        ),
    ] {
        let object = emit_host_object(&module)
            .unwrap_or_else(|error| panic!("{label} should emit direct code: {error}"));
        assert!(!object.is_empty(), "{label} should produce object bytes");
    }
}

#[test]
fn direct_backend_match_and_branch_terminator_edges_cover_enum_and_opaque_paths() {
    let wildcard_match = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![MirLocalType {
                name: "%maybe".to_string(),
                ty: Type::Named("Option".to_string(), vec![Type::named("int32")]),
            }],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![
                BasicBlock {
                    label: "entry".to_string(),
                    instructions: vec![Instruction::Assign {
                        target: "%maybe".to_string(),
                        value: Rvalue::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant_name: "Some".to_string(),
                            payloads: vec![Operand::Int(1)],
                        },
                    }],
                    terminator: Terminator::Match {
                        scrutinee: Operand::Place("%maybe".to_string()),
                        arms: vec![MirMatchArm {
                            enum_name: None,
                            variant_name: None,
                            wildcard: true,
                            label: "wild".to_string(),
                        }],
                        otherwise: "other".to_string(),
                    },
                },
                BasicBlock {
                    label: "wild".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(1)),
                },
                BasicBlock {
                    label: "other".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(0)),
                },
            ],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    assert!(!emit_host_object(&wildcard_match)
        .expect("wildcard enum matches should compile directly")
        .is_empty());

    let opaque_branch = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![
                BasicBlock {
                    label: "entry".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Branch {
                        condition: Operand::String("truthy".to_string()),
                        then_label: "then".to_string(),
                        else_label: "else".to_string(),
                    },
                },
                BasicBlock {
                    label: "then".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(1)),
                },
                BasicBlock {
                    label: "else".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(0)),
                },
            ],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    assert!(!emit_host_object(&opaque_branch)
        .expect("opaque branch conditions should use runtime truthiness")
        .is_empty());

    let scalar_match = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![
                BasicBlock {
                    label: "entry".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Match {
                        scrutinee: Operand::Int(1),
                        arms: Vec::new(),
                        otherwise: "other".to_string(),
                    },
                },
                BasicBlock {
                    label: "other".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(0)),
                },
            ],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    let scalar_error = emit_host_object(&scalar_match)
        .expect_err("scalar match scrutinees should be rejected by direct codegen");
    assert!(scalar_error.contains("expected enum matches to use opaque scrutinees"));

    let module_match = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![MirLocalType {
                name: "%module".to_string(),
                ty: Type::Module("pkg.tools".to_string()),
            }],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![
                BasicBlock {
                    label: "entry".to_string(),
                    instructions: vec![Instruction::Assign {
                        target: "%module".to_string(),
                        value: Rvalue::Use(Operand::Unit),
                    }],
                    terminator: Terminator::Match {
                        scrutinee: Operand::Place("%module".to_string()),
                        arms: Vec::new(),
                        otherwise: "other".to_string(),
                    },
                },
                BasicBlock {
                    label: "other".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(0)),
                },
            ],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    let module_error = emit_host_object(&module_match)
        .expect_err("non-named opaque match scrutinees should be rejected");
    assert!(module_error.contains("expected match scrutinee to carry an enum type name"));
}

#[test]
fn direct_backend_for_range_and_spawn_error_surface_reports_expected_diagnostics() {
    let invalid_for_range = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![MirLocalType {
                name: "item".to_string(),
                ty: Type::named("int32"),
            }],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![
                BasicBlock {
                    label: "entry".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::ForRange {
                        binding: "item".to_string(),
                        iterable: Operand::Int(0),
                        body_label: "body".to_string(),
                        exit_label: "exit".to_string(),
                    },
                },
                BasicBlock {
                    label: "body".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Goto("exit".to_string()),
                },
                BasicBlock {
                    label: "exit".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(0)),
                },
            ],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    let for_range_error = emit_host_object(&invalid_for_range)
        .expect_err("non-place for-range iterables should be rejected");
    assert!(for_range_error.contains("requires `for range` iterables to live in a place"));

    let task_start_source = r#"
def worker(value: int32) -> int32:
    return value

def main() -> int32:
    with TaskGroup() as group:
        task = group.start(worker, 1)
        match task.result():
            case TaskResult.Ready(value):
                return value
            case TaskResult.Error(_message):
                return 0
            case TaskResult.TimedOut:
                return 0
            case TaskResult.Cancelled:
                return 0
"#;
    let mut task_start_mir =
        lower_source_to_mir(task_start_source).expect("task-start source should lower");
    let main = task_start_mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main function should exist");
    main.local_types.push(MirLocalType {
        name: "%group".to_string(),
        ty: Type::named("TaskGroup"),
    });
    main.blocks = vec![BasicBlock {
        label: "entry".to_string(),
        instructions: vec![
            Instruction::Assign {
                target: "%group".to_string(),
                value: Rvalue::Call {
                    callee: CallTarget::Name("TaskGroup".to_string()),
                    args: vec![],
                },
            },
            Instruction::Assign {
                target: "%task".to_string(),
                value: Rvalue::StartTask {
                    returns_handle: true,
                    result_is_copy: true,
                    stack_size: None,
                    task_group: Operand::Place("%group".to_string()),
                    function: test_function_operand(
                        "worker",
                        vec![Type::named("int32")],
                        Type::named("int32"),
                    ),
                    args: vec![MirArg {
                        name: None,
                        value: Operand::Int(1),
                        writeback_place: None,
                    }],
                    span: Span::new(1, 1),
                },
            },
        ],
        terminator: Terminator::Return(Operand::Int(0)),
    }];
    main.local_types.push(MirLocalType {
        name: "%task".to_string(),
        ty: Type::Named("Task".to_string(), vec![Type::named("int32")]),
    });

    let main = task_start_mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .cloned()
        .expect("main function should exist");

    let mut missing_thunk_codegen = NativeCodegen::new(
        &task_start_mir,
        "/tmp/direct_task_start_missing_thunk.au",
        task_start_source,
    )
    .expect("codegen should initialize");
    missing_thunk_codegen.function_thunks.remove("worker");
    let missing_thunk_error = missing_thunk_codegen
        .define_function(&main)
        .expect_err("task start should reject missing thunks");
    assert!(missing_thunk_error.contains("does not know function thunk for `worker`"));

    let mut missing_binder_codegen = NativeCodegen::new(
        &task_start_mir,
        "/tmp/direct_task_start_missing_binder.au",
        task_start_source,
    )
    .expect("codegen should initialize");
    missing_binder_codegen
        .function_default_binders
        .remove("worker");
    let missing_binder_error = missing_binder_codegen
        .define_function(&main)
        .expect_err("task start should reject missing default binders");
    assert!(
        missing_binder_error.contains("does not know function default binder for `worker`"),
        "{missing_binder_error}"
    );

    let mut missing_return_codegen = NativeCodegen::new(
        &task_start_mir,
        "/tmp/direct_task_start_missing_return.au",
        task_start_source,
    )
    .expect("codegen should initialize");
    missing_return_codegen
        .function_return_types
        .remove("worker");
    missing_return_codegen
        .define_function(&main)
        .expect("task start repeatability is carried by MIR and needs no return-type side table");

    let mut borrowed_task_start_mir = task_start_mir.clone();
    let borrowed_main_mut = borrowed_task_start_mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main function should exist");
    borrowed_main_mut.blocks[0].instructions[1] = Instruction::Assign {
        target: "%task".to_string(),
        value: Rvalue::StartTask {
            returns_handle: true,
            result_is_copy: true,
            stack_size: None,
            task_group: Operand::Place("%group".to_string()),
            function: test_function_operand(
                "worker",
                vec![Type::named("int32")],
                Type::named("int32"),
            ),
            args: vec![MirArg {
                name: None,
                value: Operand::Int(1),
                writeback_place: Some("value".to_string()),
            }],
            span: Span::new(1, 1),
        },
    };
    let borrowed_main = borrowed_task_start_mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .cloned()
        .expect("main function should exist");
    let mut borrowed_task_start_codegen = NativeCodegen::new(
        &borrowed_task_start_mir,
        "/tmp/direct_task_start_borrowed_arg.au",
        task_start_source,
    )
    .expect("codegen should initialize");
    let borrowed_error = borrowed_task_start_codegen
        .define_function(&borrowed_main)
        .expect_err("task start should reject borrowed arguments");
    assert!(borrowed_error.contains("does not yet support borrowed task-start arguments"));

    let named_worker = Operand::Function {
        name: "worker".to_string(),
        signature: Box::new(Type::Function {
            params: vec![crate::sema::FunctionParamContract {
                name: "value".to_string(),
                ty: Type::named("int32"),
                passing: crate::ast::ReceiverKind::Value,
                has_default: false,
                default_erased: false,
            }],
            return_type: Box::new(Type::named("int32")),
        }),
    };
    let invalid_bindings = [
        (
            vec![MirArg {
                name: Some("missing".to_string()),
                value: Operand::Int(1),
                writeback_place: None,
            }],
            "task function value has no parameter named `missing`",
        ),
        (
            vec![
                MirArg {
                    name: Some("value".to_string()),
                    value: Operand::Int(1),
                    writeback_place: None,
                },
                MirArg {
                    name: None,
                    value: Operand::Int(2),
                    writeback_place: None,
                },
            ],
            "duplicate task function-value argument",
        ),
    ];
    for (args, expected) in invalid_bindings {
        let mut invalid_mir = task_start_mir.clone();
        let invalid_main = invalid_mir
            .functions
            .iter_mut()
            .find(|function| function.name == "main")
            .expect("main function should exist");
        invalid_main.blocks[0].instructions[1] = Instruction::Assign {
            target: "%task".to_string(),
            value: Rvalue::StartTask {
                returns_handle: true,
                result_is_copy: true,
                stack_size: None,
                task_group: Operand::Place("%group".to_string()),
                function: named_worker.clone(),
                args,
                span: Span::new(1, 1),
            },
        };
        let invalid_main = invalid_main.clone();
        let mut codegen = NativeCodegen::new(
            &invalid_mir,
            "/tmp/direct_task_start_invalid_binding.au",
            task_start_source,
        )
        .expect("codegen should initialize");
        let error = codegen
            .define_function(&invalid_main)
            .expect_err("invalid task callable bindings should fail");
        assert!(
            error.contains(expected),
            "expected `{expected}`, got `{error}`"
        );
    }
}

#[test]
fn direct_backend_emits_object_for_member_call_surface_matrix() {
    let source = r#"
trait Named:
    def tag(self) -> str

impl Named for int32:
    def tag(self) -> str:
        return "number"

class Counter:
    value: int32

    def read(self) -> int32:
        return self.value

    def bump(mut self):
        self.value += 1

def worker(value: int32) -> int32:
    return value + 1

def main() -> int32:
    text = "  Aura repo  "
    cloned = text.clone()
    length = text.len()
    byte_length = text.byte_len()
    has_repo = text.contains("repo")
    starts = text.starts_with("  Au")
    ends = text.ends_with("  ")
    parts = text.split(" ")
    replaced = text.replace("repo", "lang")
    lowered = text.to_lower()
    uppered = text.to_upper()
    stripped_prefix = text.strip_prefix("  ")
    stripped_suffix = text.strip_suffix("  ")
    trimmed = text.trim()
    joined = ", ".join(parts)

    value: int32 = 7
    label = value.to_string()
    tagged = value.tag()
    root = 9.0.sqrt()

    mut values: list[int32] = [1, 2, 3]
    empty = values.is_empty()
    length2 = values.len()
    values.append(4)
    popped = values.pop()
    first = values.get(0)
    direct = values[0]
    previous = values.set(0, 9)
    removed = values.remove(0)
    swapped = values.swap(0, 1)
    contains = values.contains(2)
    inserted = values.insert(0, 5)
    values.reverse()
    other_values: list[int32] = [8, 9]
    values.extend(other_values)
    values.clear()

    mut counts: dict[str, int32] = {"a": 1, "b": 2}
    map_empty = counts.is_empty()
    map_len = counts.len()
    current = counts.get("a")
    direct_count = counts["a"]
    previous_count = counts["a"]
    counts["a"] = 3
    removed_count = counts.remove("b")
    has_key = "a" in counts
    keys = counts.keys()
    vals = counts.values()
    items = counts.items()
    counts.update({"c": 4})
    counts.clear()

    mut names = set[str]()
    set_empty = names.is_empty()
    names.add("aura")
    names.add("repo")
    set_len = names.len()
    has_name = "aura" in names
    removed_name = names.remove("repo")

    jobs = Queue[int32]()
    send_result = jobs.put(1)
    recv_result = jobs.get()
    jobs.close()

    with TaskGroup() as group:
        group.cancel()
        task = group.start(worker, value=1)
        joined_task = task.result()
        print(joined_task)

    mut counter = Counter(value=1)
    current_value = counter.read()
    counter.bump()
    latest = counter.value

    print(cloned)
    print(length)
    print(byte_length)
    print(has_repo)
    print(starts)
    print(ends)
    print(replaced)
    print(lowered)
    print(uppered)
    print(joined)
    print(label)
    print(tagged)
    print(root)
    print(empty)
    print(length2)
    print(contains)
    print(inserted)
    print(map_empty)
    print(map_len)
    print(has_key)
    print(set_empty)
    print(set_len)
    print(has_name)
    print(current_value)
    print(latest)
    return direct + direct_count
"#;
    let mir = lower_source_to_mir(source).expect("member-call matrix source should lower");
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("member-call matrix should contain main");
    for local_name in ["length", "byte_length", "length2", "map_len", "set_len"] {
        let local_type = main
            .local_types
            .iter()
            .find(|local| local.name == local_name)
            .map(|local| local.ty.clone());
        assert_eq!(
            local_type,
            Some(Type::named("int64")),
            "`{local_name}` should infer as exactly int64"
        );
    }
    let object = emit_host_object(&mir).expect("member-call matrix should emit direct code");
    assert!(!object.is_empty());
    let referenced = object_referenced_symbols(&object);
    for required in [
        "aura_direct_string_len",
        "aura_direct_string_byte_len",
        "aura_direct_vec_len",
        "aura_direct_map_len",
        "aura_direct_set_len",
    ] {
        assert!(
            referenced.iter().any(|symbol| symbol.contains(required)),
            "member-length matrix should reference `{required}`: {referenced:?}"
        );
    }
}

#[test]
fn direct_string_to_bytes_routes_through_the_registered_host_builtin() {
    let source = r#"
def main() -> int32:
    payload = "café".to_bytes()
    print(payload)
    return 0
"#;
    let mir = lower_source_to_mir(source).expect("str.to_bytes source should lower to MIR");
    assert!(
        mir.functions
            .iter()
            .flat_map(|function| &function.blocks)
            .any(
                |block| block.instructions.iter().any(|instruction| matches!(
                    instruction,
                    Instruction::Assign {
                        value: Rvalue::Call {
                            callee: CallTarget::Name(name),
                            ..
                        },
                        ..
                    } if name == "str.to_bytes"
                ))
            ),
        "public str.to_bytes syntax must canonicalize to the registered host-builtin call"
    );
    let bytes = emit_host_object(&mir).expect("str.to_bytes should emit direct native code");
    let referenced = object_referenced_symbols(&bytes);
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_host_builtin")),
        "str.to_bytes must use the registered host-builtin ABI: {referenced:?}"
    );

    let object = cranelift_object::object::File::parse(bytes.as_slice())
        .expect("str.to_bytes direct output should be a readable host object");
    assert!(
        object
            .sections()
            .filter_map(|section| section.data().ok())
            .any(|data| data
                .windows(b"str.to_bytes".len())
                .any(|window| window == b"str.to_bytes")),
        "direct code must identify the canonical str.to_bytes host operation"
    );
}

#[test]
fn direct_discarded_with_capacity_calls_preserve_each_collection_constructor() {
    let source = r#"
def main():
    list[int64].with_capacity(2)
    set[str].with_capacity(2)
    dict[str, int64].with_capacity(2)
    print("complete")
"#;
    let mir = lower_source_to_mir(source)
        .expect("discarded collection-capacity calls should lower to MIR");
    let object = emit_host_object(&mir)
        .expect("discarded collection-capacity calls should emit direct native code");
    let referenced = object_referenced_symbols(&object);
    for required in [
        "aura_direct_vec_empty",
        "aura_direct_set_empty",
        "aura_direct_map_empty",
        "aura_direct_collection_operation",
    ] {
        assert!(
            referenced.iter().any(|symbol| symbol.contains(required)),
            "discarded with_capacity calls must retain `{required}`: {referenced:?}"
        );
    }
}

#[test]
fn direct_member_lengths_do_not_emit_implicit_int32_range_checks() {
    let baseline =
        module_with_main_call_result_type(Rvalue::Use(Operand::Int(0)), Type::named("int64"));
    let baseline_object =
        emit_host_object(&baseline).expect("matched int64-result baseline should emit directly");
    let baseline_overflow_references =
        object_referenced_symbol_occurrences(&baseline_object, "aura_direct_fail_int32_overflow");

    let cases = vec![
        (
            "str.len",
            module_with_main_member_call_result_type(
                "text",
                Type::named("str"),
                Rvalue::Use(Operand::String("Aura".to_string())),
                Type::named("int64"),
                "len",
                Vec::new(),
            ),
            "aura_direct_string_len",
        ),
        (
            "str.byte_len",
            module_with_main_member_call_result_type(
                "text",
                Type::named("str"),
                Rvalue::Use(Operand::String("é🎉e\u{301}".to_string())),
                Type::named("int64"),
                "byte_len",
                Vec::new(),
            ),
            "aura_direct_string_byte_len",
        ),
        (
            "list.len",
            module_with_main_member_call_result_type(
                "values",
                Type::Named("list".to_string(), vec![Type::named("str")]),
                Rvalue::VecLiteral {
                    element_type: Type::named("str"),
                    elements: vec![Operand::String("value".to_string())],
                },
                Type::named("int64"),
                "len",
                Vec::new(),
            ),
            "aura_direct_vec_len",
        ),
        (
            "dict.len",
            module_with_main_member_call_result_type(
                "counts",
                Type::Named(
                    "dict".to_string(),
                    vec![Type::named("str"), Type::named("str")],
                ),
                Rvalue::MapLiteral {
                    key_type: Type::named("str"),
                    value_type: Type::named("str"),
                    entries: vec![MirMapEntry {
                        key: Operand::String("key".to_string()),
                        value: Operand::String("value".to_string()),
                    }],
                },
                Type::named("int64"),
                "len",
                Vec::new(),
            ),
            "aura_direct_map_len",
        ),
        (
            "set.len",
            module_with_main_member_call_result_type(
                "values",
                Type::Named("set".to_string(), vec![Type::named("str")]),
                Rvalue::SetLiteral {
                    element_type: Type::named("str"),
                    elements: vec![Operand::String("value".to_string())],
                },
                Type::named("int64"),
                "len",
                Vec::new(),
            ),
            "aura_direct_set_len",
        ),
    ];

    for (label, module, required_symbol) in cases {
        let object = emit_host_object(&module)
            .unwrap_or_else(|error| panic!("{label} should emit directly: {error}"));
        let referenced = object_referenced_symbols(&object);
        assert!(
            referenced
                .iter()
                .any(|symbol| symbol.contains(required_symbol)),
            "{label} should reference `{required_symbol}`: {referenced:?}"
        );
        assert_eq!(
            object_referenced_symbol_occurrences(&object, "aura_direct_fail_int32_overflow"),
            baseline_overflow_references,
            "{label} returns int64 and must not add an implicit int32 range check"
        );
    }
}

#[test]
fn direct_member_length_explicit_int32_cast_keeps_checked_narrowing() {
    let source = r#"
def main() -> int32:
    wide = "Aura".len()
    narrow = wide as int32
    print(narrow)
    return 0
"#;

    let mir = lower_source_to_mir(source).expect("explicit member-length narrowing should lower");
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("member-length narrowing should contain main");
    for (local_name, expected) in [
        ("wide", Type::named("int64")),
        ("narrow", Type::named("int32")),
    ] {
        let local_type = main
            .local_types
            .iter()
            .find(|local| local.name == local_name)
            .map(|local| local.ty.clone());
        assert_eq!(
            local_type,
            Some(expected),
            "unexpected inferred type for `{local_name}`"
        );
    }

    let object =
        emit_host_object(&mir).expect("explicit member-length narrowing should emit directly");
    let referenced = object_referenced_symbols(&object);
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_string_len")),
        "member-length narrowing should call the str length runtime: {referenced:?}"
    );
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_fail_int32_overflow")),
        "an explicit int64-to-int32 member-length cast must retain the checked narrowing guard: {referenced:?}"
    );
}

#[test]
fn direct_int32_range_check_still_traps_exactly_at_the_boundary() {
    // The narrow range check is on the hot path of every `int32` operation, so
    // it is written as one biased unsigned comparison rather than a two-sided
    // signed pair. These cases pin that the cheaper form keeps the same exact
    // boundary: the extremes are representable and one step past either end
    // traps.
    let accepted = crate::run_source(
        r#"
def main() -> int32:
    mut high: int32 = 2147483646
    high += 1
    print(high)
    mut low: int32 = -2147483647
    low -= 1
    print(low)
    return 0
"#,
    )
    .expect("the int32 extremes should be representable");
    assert_eq!(accepted.stdout, "2147483647\n-2147483648\n");

    for (source, value) in [
        (
            "def main() -> int32:\n    mut high: int32 = 2147483647\n    high += 1\n    return 0\n",
            "2147483648",
        ),
        (
            "def main() -> int32:\n    mut low: int32 = -2147483648\n    low -= 1\n    return 0\n",
            "-2147483649",
        ),
    ] {
        let trapped = crate::run_source(source).expect_err("one step past the range should trap");
        assert_eq!(trapped.code, "AU4002", "{source}");
        assert_eq!(
            trapped.message,
            format!("integer value `{value}` does not fit in `int32`"),
            "{source}"
        );
    }

    // The same program still lowers and emits through the direct backend.
    let module = lower_source_to_mir(
        "def main() -> int32:\n    mut index: int32 = 0\n    while index < 4:\n        index += 1\n    print(index)\n    return 0\n",
    )
    .expect("an int32 loop should lower");
    let object = emit_host_object(&module).expect("an int32 loop should emit direct code");
    assert!(!object.is_empty());
}

#[test]
fn direct_backend_prefers_builtin_handle_member_if_collision_reaches_mir() {
    let source = r#"
trait Probe:
    def probe(self) -> int64

impl[T] Probe for Queue[T]:
    def probe(self) -> int64:
        return 99

def main() -> int32:
    queue = Queue[int32]()
    received = queue.get()
    return 0
"#;

    let mut mir = lower_source_to_mir(source).expect("noncolliding source should lower");
    let trait_symbol = {
        let method = mir
            .trait_impls
            .iter_mut()
            .find(|trait_impl| trait_impl.trait_name == "Probe")
            .and_then(|trait_impl| {
                trait_impl
                    .methods
                    .iter_mut()
                    .find(|method| method.name == "probe")
            })
            .expect("Probe.probe should lower");
        let symbol = mangle_symbol(&method.function_name);
        method.name = "get".to_string();
        symbol
    };

    let object =
        emit_host_object(&mir).expect("builtin dispatch should survive malformed internal MIR");
    let referenced = object_referenced_symbols(&object);

    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_channel_recv")),
        "Queue.get should reference the builtin receive helper: {referenced:?}"
    );
    assert!(
        !referenced
            .iter()
            .any(|symbol| symbol.contains(&trait_symbol)),
        "Queue.get must not dispatch to the colliding trait body: {referenced:?}"
    );
}

#[test]
fn direct_backend_member_call_error_surface_reports_expected_diagnostics() {
    let one_arg = vec![MirArg {
        name: None,
        value: Operand::Int(1),
        writeback_place: None,
    }];
    let two_args = vec![
        MirArg {
            name: None,
            value: Operand::Int(1),
            writeback_place: None,
        },
        MirArg {
            name: None,
            value: Operand::Int(2),
            writeback_place: None,
        },
    ];
    let string_object = || Rvalue::Use(Operand::String("aura".to_string()));
    let vec_object = || Rvalue::VecLiteral {
        element_type: Type::named("int32"),
        elements: vec![Operand::Int(1), Operand::Int(2)],
    };
    let map_object = || Rvalue::MapLiteral {
        key_type: Type::named("str"),
        value_type: Type::named("int32"),
        entries: vec![MirMapEntry {
            key: Operand::String("a".to_string()),
            value: Operand::Int(1),
        }],
    };
    let set_object = || Rvalue::SetLiteral {
        element_type: Type::named("str"),
        elements: vec![Operand::String("aura".to_string())],
    };
    let channel_object = || Rvalue::Call {
        callee: CallTarget::Name("Queue".to_string()),
        args: vec![],
    };

    let error_cases = [
        (
            "float sqrt extra arg",
            module_with_main_member_call(
                "value",
                Type::named("float64"),
                Rvalue::Use(Operand::Float(9.0)),
                "sqrt",
                one_arg.clone(),
            ),
            "expected `sqrt()` to take no arguments",
        ),
        (
            "string clone extra arg",
            module_with_main_member_call(
                "text",
                Type::named("str"),
                string_object(),
                "clone",
                one_arg.clone(),
            ),
            "expected `clone()` to take no arguments",
        ),
        (
            "scalar to_string extra arg",
            module_with_main_member_call(
                "value",
                Type::named("int32"),
                Rvalue::Use(Operand::Int(7)),
                "to_string",
                one_arg.clone(),
            ),
            "expected `to_string()` to take no arguments",
        ),
        (
            "string len extra arg",
            module_with_main_member_call(
                "text",
                Type::named("str"),
                string_object(),
                "len",
                one_arg.clone(),
            ),
            "expected `len()` to take no arguments",
        ),
        (
            "string contains missing arg",
            module_with_main_member_call(
                "text",
                Type::named("str"),
                string_object(),
                "contains",
                vec![],
            ),
            "expected `contains`() to receive one string argument",
        ),
        (
            "vec len extra arg",
            module_with_main_member_call(
                "values",
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                vec_object(),
                "len",
                one_arg.clone(),
            ),
            "expected `len()` to take no arguments",
        ),
        (
            "list append missing arg",
            module_with_main_member_call(
                "values",
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                vec_object(),
                "append",
                vec![],
            ),
            "expected `append()` to receive one argument",
        ),
        (
            "vec clear extra arg",
            module_with_main_member_call(
                "values",
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                vec_object(),
                "clear",
                one_arg.clone(),
            ),
            "expected `clear()` to take no arguments",
        ),
        (
            "map len extra arg",
            module_with_main_member_call(
                "counts",
                Type::Named(
                    "dict".to_string(),
                    vec![Type::named("str"), Type::named("int32")],
                ),
                map_object(),
                "len",
                one_arg.clone(),
            ),
            "expected `len()` to take no arguments",
        ),
        (
            "map set missing arg",
            module_with_main_member_call(
                "counts",
                Type::Named(
                    "dict".to_string(),
                    vec![Type::named("str"), Type::named("int32")],
                ),
                map_object(),
                "set",
                one_arg.clone(),
            ),
            "expected `set()` to receive key and value",
        ),
        (
            "set contains missing arg",
            module_with_main_member_call(
                "names",
                Type::Named("set".to_string(), vec![Type::named("str")]),
                set_object(),
                "contains",
                vec![],
            ),
            "expected `contains()` to receive one value argument",
        ),
        (
            "queue get extra arg",
            module_with_main_member_call(
                "jobs",
                Type::Named("Queue".to_string(), vec![Type::named("int32")]),
                channel_object(),
                "get",
                two_args.clone(),
            ),
            "expected `get()` or `get(timeout=...)`",
        ),
        (
            "queue put missing arg",
            module_with_main_member_call(
                "jobs",
                Type::Named("Queue".to_string(), vec![Type::named("int32")]),
                channel_object(),
                "put",
                vec![],
            ),
            "expected `put()` to receive a value argument",
        ),
        (
            "vec swap missing arg",
            module_with_main_member_call(
                "values",
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                vec_object(),
                "swap",
                one_arg.clone(),
            ),
            "expected `swap()` to receive two index arguments",
        ),
        (
            "dict update missing arg",
            module_with_main_member_call(
                "counts",
                Type::Named(
                    "dict".to_string(),
                    vec![Type::named("str"), Type::named("int32")],
                ),
                map_object(),
                "update",
                vec![],
            ),
            "expected `update()` to receive one dict argument",
        ),
        (
            "set remove missing arg",
            module_with_main_member_call(
                "names",
                Type::Named("set".to_string(), vec![Type::named("str")]),
                set_object(),
                "remove",
                vec![],
            ),
            "expected `remove()` to receive one value argument",
        ),
        (
            "unknown runtime member",
            module_with_main_member_call(
                "text",
                Type::named("str"),
                string_object(),
                "missing",
                vec![],
            ),
            "does not know runtime member `str.missing`",
        ),
        (
            "unknown scalar member",
            module_with_main_member_call(
                "value",
                Type::named("int32"),
                Rvalue::Use(Operand::Int(7)),
                "missing",
                vec![],
            ),
            "does not support member call `.missing` on `int32`",
        ),
        (
            "string replace missing arg",
            module_with_main_member_call(
                "text",
                Type::named("str"),
                string_object(),
                "replace",
                one_arg.clone(),
            ),
            "expected `replace()` to receive `from` and `to` string arguments",
        ),
        (
            "vec insert missing arg",
            module_with_main_member_call(
                "values",
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                vec_object(),
                "insert",
                one_arg.clone(),
            ),
            "expected `insert()` to receive index and value",
        ),
        (
            "map items extra arg",
            module_with_main_member_call(
                "counts",
                Type::Named(
                    "dict".to_string(),
                    vec![Type::named("str"), Type::named("int32")],
                ),
                map_object(),
                "items",
                one_arg.clone(),
            ),
            "expected `items()` to take no arguments",
        ),
        (
            "set len extra arg",
            module_with_main_member_call(
                "names",
                Type::Named("set".to_string(), vec![Type::named("str")]),
                set_object(),
                "len",
                one_arg,
            ),
            "expected `len()` to take no arguments",
        ),
        (
            "string join missing arg",
            module_with_main_member_call(
                "text",
                Type::named("str"),
                string_object(),
                "join",
                vec![],
            ),
            "expected `join()` to receive one list argument",
        ),
        (
            "string trim extra arg",
            module_with_main_member_call(
                "text",
                Type::named("str"),
                string_object(),
                "trim",
                two_args[..1].to_vec(),
            ),
            "expected `trim()` to take no arguments",
        ),
    ];

    for (label, module, expected) in error_cases {
        let error = emit_host_object(&module)
            .expect_err(&format!("{label} should be rejected by direct codegen"));
        assert!(
            error.contains(expected),
            "{label} reported `{error}` instead of containing `{expected}`"
        );
    }
}

#[test]
fn direct_backend_operand_and_construct_error_surface_reports_expected_diagnostics() {
    let large_int_module = module_with_main_call(Rvalue::Use(Operand::Int((i64::MAX as u128) + 1)));
    let large_int_object =
        emit_host_object(&large_int_module).expect("large integer operands should box");
    assert!(!large_int_object.is_empty());

    let large_duration_module =
        module_with_main_call(Rvalue::Use(Operand::Duration((i64::MAX as i128) + 1)));
    let large_duration_object = emit_host_object(&large_duration_module)
        .expect("duration literals beyond i64 should use the two-limb runtime ABI");
    assert!(
        object_referenced_symbols(&large_duration_object)
            .iter()
            .any(|symbol| symbol.contains("aura_direct_duration_literal")),
        "direct duration literals should remain opaque after runtime construction"
    );

    let missing_place_module =
        module_with_main_call(Rvalue::Use(Operand::Place("missing".to_string())));
    let missing_place_error =
        emit_host_object(&missing_place_module).expect_err("unknown locals should be rejected");
    assert!(missing_place_error.contains("does not know local `missing`"));

    let empty_place_module = module_with_main_call(Rvalue::Use(Operand::Place(String::new())));
    let empty_place_error =
        emit_host_object(&empty_place_module).expect_err("empty places should be rejected");
    assert!(empty_place_error.contains("does not know local"));

    let stray_pop_cleanup_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![Instruction::PopCleanup {
                    place: "ghost".to_string(),
                    cancel_before_cleanup: false,
                }],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    let stray_pop_cleanup_error = emit_host_object(&stray_pop_cleanup_module)
        .expect_err("unmatched cleanup pops should report the missing cleanup registration");
    assert!(stray_pop_cleanup_error.contains("does not know cleanup registration for `ghost`"));

    let pair_class = crate::mir::MirClass {
        name: "Pair".to_string(),
        type_params: Vec::new(),
        fields: vec![
            crate::mir::MirClassField {
                name: "left".to_string(),
                ty: Type::named("int32"),
            },
            crate::mir::MirClassField {
                name: "right".to_string(),
                ty: Type::named("int32"),
            },
        ],
        methods: Vec::new(),
    };
    let missing_field_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![Instruction::Assign {
                    target: "%t0".to_string(),
                    value: Rvalue::Construct {
                        class_name: "Pair".to_string(),
                        fields: vec![crate::mir::MirFieldInit {
                            name: "left".to_string(),
                            value: Operand::Int(1),
                        }],
                    },
                }],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: vec![pair_class.clone()],
        trait_impls: Vec::new(),
        top_level: None,
    };
    let missing_field_error = emit_host_object(&missing_field_module)
        .expect_err("plain-class construction should require all fields");
    assert!(missing_field_error.contains("construction for `Pair` is missing field `right`"));

    let non_class_construct_module = module_with_main_call(Rvalue::Construct {
        class_name: "int32".to_string(),
        fields: Vec::new(),
    });
    let non_class_construct_error = emit_host_object(&non_class_construct_module)
        .expect_err("constructing scalar types should be rejected");
    assert!(non_class_construct_error.contains("could not construct non-class type `int32`"));

    let plain_cast_target_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![MirLocalType {
                name: "%t0".to_string(),
                ty: Type::named("Pair"),
            }],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![Instruction::Assign {
                    target: "%t0".to_string(),
                    value: Rvalue::Cast {
                        value: Operand::Int(1),
                        ty: Type::named("Pair"),
                        span: Span::new(1, 1),
                    },
                }],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: vec![pair_class.clone()],
        trait_impls: Vec::new(),
        top_level: None,
    };
    let plain_cast_target_error = emit_host_object(&plain_cast_target_module)
        .expect_err("casts to plain classes should be rejected before code emission");
    assert!(plain_cast_target_error
        .contains("direct backend only supports numeric casts, found target `Pair`"));

    let plain_cast_source_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![
                MirLocalType {
                    name: "pair".to_string(),
                    ty: Type::named("Pair"),
                },
                MirLocalType {
                    name: "%t0".to_string(),
                    ty: Type::named("int32"),
                },
            ],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::Assign {
                        target: "pair".to_string(),
                        value: Rvalue::Construct {
                            class_name: "Pair".to_string(),
                            fields: vec![
                                crate::mir::MirFieldInit {
                                    name: "left".to_string(),
                                    value: Operand::Int(1),
                                },
                                crate::mir::MirFieldInit {
                                    name: "right".to_string(),
                                    value: Operand::Int(2),
                                },
                            ],
                        },
                    },
                    Instruction::Assign {
                        target: "%t0".to_string(),
                        value: Rvalue::Cast {
                            value: Operand::Place("pair".to_string()),
                            ty: Type::named("int32"),
                            span: Span::new(1, 1),
                        },
                    },
                ],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: vec![pair_class.clone()],
        trait_impls: Vec::new(),
        top_level: None,
    };
    let plain_cast_source_error = emit_host_object(&plain_cast_source_module)
        .expect_err("casts from plain classes should be rejected before code emission");
    assert!(plain_cast_source_error
        .contains("direct backend only supports numeric casts from scalar values, found `Pair`"));

    for (label, module, expected) in [
        (
            "scalar field access",
            module_with_main_call(Rvalue::Member {
                object: Operand::Int(1),
                field: "missing".to_string(),
            }),
            "direct backend does not know field `missing` on `int64`",
        ),
        (
            "integer boolean op",
            module_with_main_call(Rvalue::Binary {
                op: BinaryOp::And,
                left: Operand::Int(1),
                right: Operand::Int(2),
                span: Span::new(1, 1),
            }),
            "direct backend does not support integer binary operation `And`",
        ),
        (
            "float boolean op",
            module_with_main_call_result_type(
                Rvalue::Binary {
                    op: BinaryOp::And,
                    left: Operand::Float(1.0),
                    right: Operand::Float(2.0),
                    span: Span::new(1, 1),
                },
                Type::named("float64"),
            ),
            "direct backend does not support float binary operation `And`",
        ),
        (
            "bool arithmetic op",
            module_with_main_call_result_type(
                Rvalue::Binary {
                    op: BinaryOp::Add,
                    left: Operand::Bool(true),
                    right: Operand::Bool(false),
                    span: Span::new(1, 1),
                },
                Type::named("bool"),
            ),
            "direct backend does not support boolean binary operation `Add`",
        ),
        (
            "unsupported scalar cast target",
            module_with_main_call(Rvalue::Cast {
                value: Operand::Int(1),
                ty: Type::named("bool"),
                span: Span::new(1, 1),
            }),
            "direct backend only supports numeric casts, found `int64` to `bool`",
        ),
        (
            "nonnumeric cast source",
            module_with_main_call(Rvalue::Cast {
                value: Operand::Unit,
                ty: Type::named("int32"),
                span: Span::new(1, 1),
            }),
            "direct backend only supports numeric casts, found `None` to `int32`",
        ),
    ] {
        let error = emit_host_object(&module).expect_err(&format!("{label} should be rejected"));
        assert!(
            error.contains(expected),
            "{label} reported `{error}` instead of containing `{expected}`"
        );
    }

    let missing_field_access_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![
                MirLocalType {
                    name: "pair".to_string(),
                    ty: Type::named("Pair"),
                },
                MirLocalType {
                    name: "%t0".to_string(),
                    ty: Type::named("int32"),
                },
            ],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::Assign {
                        target: "pair".to_string(),
                        value: Rvalue::Construct {
                            class_name: "Pair".to_string(),
                            fields: vec![
                                crate::mir::MirFieldInit {
                                    name: "left".to_string(),
                                    value: Operand::Int(1),
                                },
                                crate::mir::MirFieldInit {
                                    name: "right".to_string(),
                                    value: Operand::Int(2),
                                },
                            ],
                        },
                    },
                    Instruction::Assign {
                        target: "%t0".to_string(),
                        value: Rvalue::Use(Operand::Place("pair.missing".to_string())),
                    },
                ],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: vec![pair_class],
        trait_impls: Vec::new(),
        top_level: None,
    };
    let missing_field_access_error = emit_host_object(&missing_field_access_module)
        .expect_err("unknown plain-class fields should be rejected");
    assert!(missing_field_access_error.contains("does not know field `missing`"));
}

#[test]
fn native_codegen_reports_invalid_non_boolean_branch_conditions() {
    let invalid_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![MirLocalType {
                name: "%cond".to_string(),
                ty: Type::named("float64"),
            }],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![
                BasicBlock {
                    label: "entry".to_string(),
                    instructions: vec![Instruction::Assign {
                        target: "%cond".to_string(),
                        value: Rvalue::Use(Operand::Float(1.25)),
                    }],
                    terminator: Terminator::Branch {
                        condition: Operand::Place("%cond".to_string()),
                        then_label: "then".to_string(),
                        else_label: "else".to_string(),
                    },
                },
                BasicBlock {
                    label: "then".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(1)),
                },
                BasicBlock {
                    label: "else".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(0)),
                },
            ],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    let error = emit_host_object(&invalid_module)
        .expect_err("non-boolean branch conditions should be rejected by direct codegen");
    assert!(error.contains("cannot use `float64` as a branch condition"));
}

#[test]
fn native_codegen_rejects_try_between_non_result_types() {
    let invalid_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![
                MirLocalType {
                    name: "%source".to_string(),
                    ty: Type::named("int32"),
                },
                MirLocalType {
                    name: "%target".to_string(),
                    ty: Type::named("int32"),
                },
            ],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::Assign {
                        target: "%source".to_string(),
                        value: Rvalue::Use(Operand::Int(1)),
                    },
                    Instruction::Assign {
                        target: "%target".to_string(),
                        value: Rvalue::Try {
                            value: Operand::Place("%source".to_string()),
                        },
                    },
                ],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    let error = emit_host_object(&invalid_module)
        .expect_err("direct backend try should require Result operands and returns");
    assert!(error.contains("requires Result types"));
}

#[test]
fn direct_try_moves_noncopy_ok_payloads_through_the_destructive_runtime_path() {
    let source = r#"
def produce() -> Result[str, str]:
    return Result.Ok("owned payload")

def forward() -> Result[str, str]:
    result = produce()
    value = try result
    return Result.Ok(value)

def main() -> int32:
    return 0
"#;

    let module = lower_source_to_mir(source).expect("owned try source should lower to MIR");
    let object = emit_host_object(&module).expect("owned try source should compile directly");
    let referenced = object_referenced_symbols(&object);
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_variant_take_payload")),
        "owned try must destructively take its successful payload: {referenced:?}"
    );
}

#[test]
fn native_codegen_thunk_helpers_cover_roundtrip_paths() {
    let source =
        "class Pair:\n    left: int32\n    right: bool\n\ndef main() -> int32:\n    return 0\n";
    let mir = lower_source_to_mir(source).expect("thunk helper source should lower");
    let mut codegen = NativeCodegen::new(&mir, "/tmp/thunk_helpers.au", source)
        .expect("codegen should initialize");

    let mut ctx = Context::new();
    ctx.func.signature = cranelift_codegen::ir::Signature::new(codegen.call_conv);
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut builder = cranelift_frontend::FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
    let block = builder.create_block();
    builder.switch_to_block(block);
    builder.seal_block(block);

    let (_first_ptr, _first_len) = thunk_string_constant(&mut codegen, &mut builder, b"aura")
        .expect("first string constant should lower");
    let (_second_ptr, _second_len) = thunk_string_constant(&mut codegen, &mut builder, b"aura")
        .expect("duplicate string constant should reuse existing data");
    assert_eq!(codegen.string_data.len(), 1);

    let opaque_raw = builder.ins().iconst(types::I64, 7);
    let int_raw = builder.ins().iconst(types::I64, 11);
    let bool_raw = builder.ins().iconst(types::I64, 1);
    let float_raw = builder.ins().f64const(3.5);

    let opaque_boxed = box_thunk_value(
        &mut codegen,
        &mut builder,
        &[opaque_raw],
        &DirectType::Opaque(Type::named("str")),
    )
    .expect("opaque thunk values should pass through");
    let opaque_unboxed = unbox_thunk_value(
        &mut codegen,
        &mut builder,
        opaque_boxed,
        &DirectType::Opaque(Type::named("str")),
    )
    .expect("opaque thunk values should unbox directly");
    assert_eq!(opaque_unboxed.len(), 1);

    let boxed_int = box_thunk_value(
        &mut codegen,
        &mut builder,
        &[int_raw],
        &DirectType::Scalar(ScalarKind::Int32),
    )
    .expect("int thunk values should box");
    let unboxed_int = unbox_thunk_value(
        &mut codegen,
        &mut builder,
        boxed_int,
        &DirectType::Scalar(ScalarKind::Int32),
    )
    .expect("int thunk values should unbox");
    assert_eq!(unboxed_int.len(), 1);

    for (label, kind) in [("int64", ScalarKind::Int64), ("uint64", ScalarKind::Uint64)] {
        let boxed = box_thunk_value(
            &mut codegen,
            &mut builder,
            &[int_raw],
            &DirectType::Scalar(kind),
        )
        .unwrap_or_else(|error| panic!("{label} thunk values should box: {error}"));
        let unboxed =
            unbox_thunk_value(&mut codegen, &mut builder, boxed, &DirectType::Scalar(kind))
                .unwrap_or_else(|error| panic!("{label} thunk values should unbox: {error}"));
        assert_eq!(unboxed.len(), 1, "{label} should have one ABI value");
        assert_eq!(
            builder.func.dfg.value_type(unboxed[0]),
            types::I64,
            "{label} thunk round trips must use the I64 ABI"
        );
    }

    let boxed_float = box_thunk_value(
        &mut codegen,
        &mut builder,
        &[float_raw],
        &DirectType::Scalar(ScalarKind::Float64),
    )
    .expect("float thunk values should box");
    let unboxed_float = unbox_thunk_value(
        &mut codegen,
        &mut builder,
        boxed_float,
        &DirectType::Scalar(ScalarKind::Float64),
    )
    .expect("float thunk values should unbox");
    assert_eq!(unboxed_float.len(), 1);

    let boxed_bool = box_thunk_value(
        &mut codegen,
        &mut builder,
        &[bool_raw],
        &DirectType::Scalar(ScalarKind::Bool),
    )
    .expect("bool thunk values should box");
    let unboxed_bool = unbox_thunk_value(
        &mut codegen,
        &mut builder,
        boxed_bool,
        &DirectType::Scalar(ScalarKind::Bool),
    )
    .expect("bool thunk values should unbox");
    assert_eq!(unboxed_bool.len(), 1);

    let boxed_unit = box_thunk_value(
        &mut codegen,
        &mut builder,
        &[],
        &DirectType::Scalar(ScalarKind::Unit),
    )
    .expect("unit thunk values should box");
    let unboxed_unit = unbox_thunk_value(
        &mut codegen,
        &mut builder,
        boxed_unit,
        &DirectType::Scalar(ScalarKind::Unit),
    )
    .expect("unit thunk values should unbox");
    assert_eq!(unboxed_unit.len(), 1);
    let opaque_missing = box_thunk_value(
        &mut codegen,
        &mut builder,
        &[],
        &DirectType::Opaque(Type::named("str")),
    )
    .expect_err("opaque thunk boxing should require a raw value");
    assert!(opaque_missing.contains("task-start thunk expected an opaque value"));

    let pair_ty = DirectType::PlainClass(PlainClassType {
        class_name: "Pair".to_string(),
        fields: vec![
            PlainClassField {
                name: "left".to_string(),
                ty: DirectType::Scalar(ScalarKind::Int32),
            },
            PlainClassField {
                name: "right".to_string(),
                ty: DirectType::Scalar(ScalarKind::Bool),
            },
        ],
    });
    let boxed_pair = box_thunk_value(&mut codegen, &mut builder, &[int_raw, bool_raw], &pair_ty)
        .expect("plain class thunk values should box recursively");
    let unboxed_pair = unbox_thunk_value(&mut codegen, &mut builder, boxed_pair, &pair_ty)
        .expect("plain class thunk values should unbox recursively");
    assert_eq!(unboxed_pair.len(), 2);
    assert!(codegen.string_data.len() >= 3);

    builder.ins().return_(&[]);
    builder.finalize();
}

#[test]
fn native_codegen_release_helpers_cover_cleanup_error_paths() {
    let source = "def main() -> int32:\n    return 0\n";
    let mir = lower_source_to_mir(source).expect("release helper source should lower");
    let mut codegen = NativeCodegen::new(&mir, "/tmp/release_helpers.au", source)
        .expect("codegen should initialize");

    let mut ctx = Context::new();
    ctx.func.signature = cranelift_codegen::ir::Signature::new(codegen.call_conv);
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut builder = cranelift_frontend::FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
    let block = builder.create_block();
    builder.switch_to_block(block);
    builder.seal_block(block);

    let raw_opaque = builder.ins().iconst(types::I64, 7);
    let raw_unit = builder.ins().iconst(types::I64, 0);
    let raw_bool = builder.ins().iconst(types::I64, 1);
    let opaque_string = DirectType::Opaque(Type::named("str"));
    let plain_pair = DirectType::PlainClass(PlainClassType {
        class_name: "Pair".to_string(),
        fields: vec![
            PlainClassField {
                name: "text".to_string(),
                ty: opaque_string.clone(),
            },
            PlainClassField {
                name: "flag".to_string(),
                ty: DirectType::Scalar(ScalarKind::Bool),
            },
        ],
    });

    let unknown_return_error =
        release_direct_call_results(&mut codegen, &mut builder, "missing", &[raw_opaque])
            .expect_err("unknown cleanup return types should fail");
    assert!(unknown_return_error.contains("does not know return type for `missing`"));

    codegen
        .function_return_types
        .insert("needs_opaque".to_string(), opaque_string.clone());
    let too_few_error =
        release_direct_call_results(&mut codegen, &mut builder, "needs_opaque", &[])
            .expect_err("too few cleanup return values should fail");
    assert!(too_few_error.contains("cleanup call `needs_opaque` returned too few values"));

    let missing_opaque_error =
        release_direct_values(&mut codegen, &mut builder, &[], &opaque_string)
            .expect_err("opaque cleanup release should require one value");
    assert!(missing_opaque_error.contains("expected an opaque `str` result"));

    codegen.function_return_types.insert(
        "unit_with_writeback".to_string(),
        DirectType::Scalar(ScalarKind::Unit),
    );
    codegen.function_writeback_types.insert(
        "unit_with_writeback".to_string(),
        vec![opaque_string.clone()],
    );
    let incomplete_writeback_error = release_direct_call_results(
        &mut codegen,
        &mut builder,
        "unit_with_writeback",
        &[raw_unit],
    )
    .expect_err("missing cleanup writeback values should fail");
    assert!(incomplete_writeback_error
        .contains("cleanup call `unit_with_writeback` returned incomplete writeback values"));

    release_direct_call_results(
        &mut codegen,
        &mut builder,
        "unit_with_writeback",
        &[raw_unit, raw_opaque],
    )
    .expect("complete cleanup writeback values should release");

    let incomplete_plain_class_error =
        release_direct_values(&mut codegen, &mut builder, &[raw_opaque], &plain_pair)
            .expect_err("plain class cleanup release should require every field value");
    assert!(incomplete_plain_class_error.contains("expected `2` values for `Pair`"));

    release_direct_values(
        &mut codegen,
        &mut builder,
        &[raw_opaque, raw_bool],
        &plain_pair,
    )
    .expect("complete plain class values should release recursively");

    builder.ins().return_(&[]);
    builder.finalize();
}

#[test]
fn direct_backend_emits_object_for_module_examples() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate should live under repo root")
        .parent()
        .expect("compiler crate should live under repo root")
        .to_path_buf();
    let examples = [
        repo_root.join("examples/modules/namespace_import_types.au"),
        repo_root.join("examples/modules/trait_impl_imports.au"),
    ];

    for path in examples {
        let mir = lower_path_to_mir(&path).expect("module example should lower to MIR");
        let object = emit_host_object(&mir).expect("module example should emit direct object");
        assert!(!object.is_empty(), "{}", path.display());
    }
}

#[test]
fn direct_backend_emits_object_for_broad_maintained_example_surface() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate should live under repo root")
        .parent()
        .expect("compiler crate should live under repo root")
        .to_path_buf();
    let paths = [
        "examples/basics/top_level_script.au",
        "examples/basics/main_function.au",
        "examples/basics/mutable_bindings.au",
        "examples/basics/default_arguments.au",
        "examples/basics/pass_keyword.au",
        "examples/basics/borrow_parameters.au",
        "examples/basics/named_arguments.au",
        "examples/basics/named_builtin_arguments.au",
        "examples/basics/none_values.au",
        "examples/basics/simple_example.au",
        "examples/classes/point_distance.au",
        "examples/classes/default_fields.au",
        "examples/classes/methods.au",
        "examples/classes/copy_class.au",
        "examples/classes/indirect_recursive.au",
        "examples/classes/mutating_methods.au",
        "examples/control_flow/if_elif_else.au",
        "examples/control_flow/for_range.au",
        "examples/control_flow/while_break_continue.au",
        "examples/control_flow/boolean_logic.au",
        "examples/enums/result_match.au",
        "examples/enums/result_option.au",
        "examples/enums/explicit_type_args.au",
        "examples/enums/match_borrow.au",
        "examples/error_handling/try_result.au",
        "examples/generics/box_and_wrapper.au",
        "examples/generics/generic_constructor_specialization.au",
        "examples/traits/greeter.au",
        "examples/traits/multiple_bounds.au",
        "examples/traits/generic_trait_impl.au",
        "examples/traits/specialized_trait_dispatch.au",
        "examples/traits/trait_associated_factory.au",
        "examples/traits/operator_traits.au",
        "examples/traits/ordering_traits.au",
        "examples/basics/copy_return_selection.au",
        "examples/traits/generic_trait_bounds.au",
        "examples/numbers/float_sqrt.au",
        "examples/numbers/float32_values.au",
        "examples/numbers/numeric_casts.au",
        "examples/numbers/uint128_values.au",
        "examples/numbers/unary_minus.au",
        "examples/strings/string_clone.au",
        "examples/strings/f_strings.au",
        "examples/strings/borrow_str.au",
        "examples/strings/string_methods.au",
        "examples/strings/string_parsing_and_formatting.au",
        "examples/concurrency/task_group_start.au",
        "examples/concurrency/queue_iteration.au",
        "examples/concurrency/queue_put_timeout.au",
        "examples/concurrency/queue_get_timeout_named.au",
        "examples/concurrency/task_group_start_soon.au",
        "examples/concurrency/task_group_cancel.au",
        "examples/concurrency/task_group_queue_sum.au",
        "examples/io/read_text_file.au",
        "examples/io/bytes_file_io.au",
        "examples/io/process_run.au",
        "examples/io/process_supervisor.au",
        "examples/io/tcp_echo.au",
        "examples/io/tcp_bytes.au",
        "examples/io/udp_echo.au",
        "examples/io/http_roundtrip.au",
        "examples/io/websocket_roundtrip.au",
        "examples/resources/with_resource.au",
        "examples/modules/namespace_import_types.au",
        "examples/modules/trait_impl_imports.au",
        "examples/basic_addition.au",
        "examples/control_flow.au",
        "examples/point.au",
        "examples/simple_addition.au",
        "examples/top_level_addition.au",
    ];

    for relative in paths {
        let path = repo_root.join(relative);
        let mir = lower_path_to_mir(&path).expect("maintained example should lower to MIR");
        let object =
            emit_host_object(&mir).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert!(!object.is_empty(), "{}", path.display());
    }
}

fn assert_direct_backend_emits_object_for_scratch_repro(relative: &str) {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate should live under repo root")
        .parent()
        .expect("compiler crate should live under repo root")
        .to_path_buf();
    let path = repo_root.join(relative);
    let mir = lower_path_to_mir(&path).expect("scratch repro should lower to MIR");
    let object = emit_host_object(&mir).expect("scratch repro should emit direct object");
    assert!(!object.is_empty(), "{}", path.display());
}

#[test]
fn direct_backend_emits_object_for_generic_trait_bound_returning_int_repro() {
    assert_direct_backend_emits_object_for_scratch_repro(
        "test_edge/gt_26_generic_fn_with_trait_bound_returns_int.au",
    );
}

#[test]
fn direct_backend_emits_object_for_multi_param_trait_bound_repro() {
    assert_direct_backend_emits_object_for_scratch_repro(
        "test_edge/gt_49_trait_bound_with_multiple_params.au",
    );
}

#[test]
fn direct_backend_emits_object_for_generic_sort_trait_bound_repro() {
    assert_direct_backend_emits_object_for_scratch_repro(
        "test_edge/test_complex_15_generic_sort.au",
    );
}

#[test]
fn direct_backend_emits_object_for_multiple_trait_methods_repro() {
    assert_direct_backend_emits_object_for_scratch_repro(
        "test_edge/test_trait_multiple_methods.au",
    );
}

#[test]
fn mangle_symbol_rewrites_non_alphanumeric_characters() {
    assert_eq!(mangle_symbol("main"), "aura_fn_main");
    assert_eq!(
        mangle_symbol("helpers.math.double"),
        "aura_fn_helpers_math_double"
    );
}

#[test]
fn direct_type_supports_plain_classes_and_scalars() {
    let source = include_str!("../../../examples/classes/methods.au");
    let mir = lower_source_to_mir(source).expect("methods example should lower");
    let classes = mir
        .classes
        .iter()
        .cloned()
        .map(|class| (class.name.clone(), class))
        .collect::<HashMap<_, _>>();

    assert_eq!(
        scalar_kind_for_tests(&Type::named("int32")),
        Some(ScalarKind::Int32)
    );
    assert_eq!(
        scalar_kind_for_tests(&Type::named("float64")),
        Some(ScalarKind::Float64)
    );
    assert_eq!(
        scalar_kind_for_tests(&Type::named("bool")),
        Some(ScalarKind::Bool)
    );
    assert_eq!(scalar_kind_for_tests(&Type::Unit), Some(ScalarKind::Unit));

    let counter = direct_type(&Type::named("Counter"), &classes).expect("Counter should be direct");
    assert_eq!(render_direct_type(&counter), "Counter");
    assert_eq!(counter.value_count(), 1);
}

#[test]
fn ticket9_int64_and_uint64_are_unboxed_i64_direct_scalars() {
    let classes = HashMap::new();

    for (name, kind) in [("int64", ScalarKind::Int64), ("uint64", ScalarKind::Uint64)] {
        let direct = direct_type(&Type::named(name), &classes)
            .unwrap_or_else(|| panic!("{name} should have a direct type"));

        assert_eq!(direct, DirectType::Scalar(kind));
        assert_eq!(direct.scalar_kind(), Some(kind));
        assert_eq!(kind.signature_type(), types::I64);
        assert_eq!(direct.abi_types(), vec![types::I64]);
        assert_eq!(render_direct_type(&direct), name);
        assert_eq!(direct_type_to_type(&direct), Type::named(name));
        assert!(!kind.is_float());
    }

    let source = r#"
def wide_identity(signed: int64, unsigned: uint64) -> int64:
    if unsigned > 0:
        return signed
    return 0

def main() -> int32:
    return 0
"#;
    let mir = lower_source_to_mir(source).expect("wide scalar signature should lower");
    let function = mir
        .functions
        .iter()
        .find(|function| function.name == "wide_identity")
        .expect("wide_identity should be present in MIR");
    let signature = signature_for(
        function,
        &classes,
        cranelift_codegen::isa::CallConv::SystemV,
    )
    .expect("int64 and uint64 should have direct signatures");

    assert_eq!(
        signature
            .params
            .iter()
            .map(|parameter| parameter.value_type)
            .collect::<Vec<_>>(),
        vec![types::I64, types::I64]
    );
    assert_eq!(
        signature
            .returns
            .iter()
            .map(|result| result.value_type)
            .collect::<Vec<_>>(),
        vec![types::I64]
    );
}

#[test]
fn d2_numeric_member_and_floor_division_inference_preserve_backend_result_types() {
    let variable_types = HashMap::from([
        (
            "signed32".to_string(),
            DirectType::Scalar(ScalarKind::Int32),
        ),
        (
            "signed64".to_string(),
            DirectType::Scalar(ScalarKind::Int64),
        ),
        (
            "unsigned64".to_string(),
            DirectType::Scalar(ScalarKind::Uint64),
        ),
        (
            "floating".to_string(),
            DirectType::Scalar(ScalarKind::Float64),
        ),
        (
            "signed128".to_string(),
            DirectType::Opaque(Type::named("int128")),
        ),
    ]);
    let function_return_types = HashMap::new();
    let classes = HashMap::new();

    for (place, expected) in [
        ("signed32", DirectType::Scalar(ScalarKind::Int32)),
        ("signed64", DirectType::Scalar(ScalarKind::Int64)),
        ("unsigned64", DirectType::Scalar(ScalarKind::Uint64)),
        ("floating", DirectType::Scalar(ScalarKind::Float64)),
        ("signed128", DirectType::Opaque(Type::named("int128"))),
    ] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Binary {
                    op: BinaryOp::FloorDiv,
                    left: Operand::Place(place.to_string()),
                    right: Operand::Place(place.to_string()),
                    span: Span::new(1, 1),
                },
                &variable_types,
                &function_return_types,
                &classes,
            ),
            Some(expected),
            "floor division should preserve the numeric type of `{place}`",
        );
    }

    for place in ["signed32", "signed64", "unsigned64", "signed128"] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Call {
                    callee: CallTarget::Member {
                        object: Operand::Place(place.to_string()),
                        field: "to_float".to_string(),
                        receiver_place: None,
                    },
                    args: Vec::new(),
                },
                &variable_types,
                &function_return_types,
                &classes,
            ),
            Some(DirectType::Scalar(ScalarKind::Float64)),
            "integer `{place}` should infer `to_float()` as float64",
        );
    }

    for integer_type in [
        "int8", "int16", "int128", "intsize", "uint8", "uint16", "uint32", "uint128", "uintsize",
    ] {
        assert_eq!(
            builtin_opaque_member_return_type(&Type::named(integer_type), "to_float", &classes),
            Some(DirectType::Scalar(ScalarKind::Float64)),
            "boxed integer `{integer_type}` should expose `to_float() -> float64`",
        );
    }
}

#[test]
fn s1_direct_inference_pins_numeric_collection_and_constant_abis() {
    let variable_types = HashMap::from([
        (
            "narrow".to_string(),
            DirectType::Opaque(Type::named("int8")),
        ),
        (
            "small_float".to_string(),
            DirectType::Scalar(ScalarKind::Float32),
        ),
        ("shifted".to_string(), DirectType::Scalar(ScalarKind::Int64)),
    ]);
    let returns = HashMap::new();
    let classes = HashMap::new();
    let argument = |place: &str| MirArg {
        name: None,
        value: Operand::Place(place.to_string()),
        writeback_place: None,
    };

    assert_eq!(
        infer_rvalue_type(
            &Rvalue::ModuleConstant {
                key: "settings::limit".to_string(),
                initializer: "settings::__constant_limit".to_string(),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        None,
        "module-constant reads use their declared destination ABI instead of guessing from the key",
    );

    for (place, expected) in [
        ("small_float", DirectType::Scalar(ScalarKind::Int64)),
        ("narrow", DirectType::Opaque(Type::named("int8"))),
    ] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Call {
                    callee: CallTarget::Name("round".to_string()),
                    args: vec![argument(place)],
                },
                &variable_types,
                &returns,
                &classes,
            ),
            Some(expected),
            "round must preserve integer width and return int64 for float input",
        );
    }

    for (place, element_type) in [
        ("small_float", Type::named("float32")),
        ("narrow", Type::named("int8")),
    ] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Call {
                    callee: CallTarget::Name("divmod".to_string()),
                    args: vec![argument(place), argument(place)],
                },
                &variable_types,
                &returns,
                &classes,
            ),
            Some(DirectType::Opaque(Type::Tuple(vec![
                element_type.clone(),
                element_type,
            ]))),
            "divmod must preserve its exact numeric operand type in both tuple fields",
        );
    }

    for field in [
        "wrapping_shl",
        "wrapping_shr",
        "saturating_shl",
        "saturating_shr",
    ] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Call {
                    callee: CallTarget::Member {
                        object: Operand::Place("shifted".to_string()),
                        field: field.to_string(),
                        receiver_place: None,
                    },
                    args: vec![argument("shifted")],
                },
                &variable_types,
                &returns,
                &classes,
            ),
            Some(DirectType::Scalar(ScalarKind::Int64)),
            "{field} must preserve the receiver's direct scalar lane",
        );
    }

    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Call {
                callee: CallTarget::Name("random::secure_bytes".to_string()),
                args: vec![MirArg {
                    name: Some("length".to_string()),
                    value: Operand::Int(4),
                    writeback_place: None,
                }],
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![Type::named("uint8")],
        ))),
        "secure bytes must use the canonical list[uint8] direct ABI",
    );

    for (object_type, field, expected) in [
        (
            Type::Named("Array".to_string(), vec![Type::named("float32")]),
            "shape",
            DirectType::Opaque(Type::Named("list".to_string(), vec![Type::named("int64")])),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("str")]),
            "__slice",
            DirectType::Opaque(Type::Named("list".to_string(), vec![Type::named("str")])),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("str")]),
            "index",
            DirectType::Scalar(ScalarKind::Int64),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("str")]),
            "count",
            DirectType::Scalar(ScalarKind::Int64),
        ),
    ] {
        assert_eq!(
            builtin_opaque_member_return_type(&object_type, field, &classes),
            Some(expected),
            "{object_type}.{field} must retain its canonical direct return ABI",
        );
    }
}

#[test]
fn infer_operand_and_rvalue_types_track_plain_classes() {
    let mut variable_types = HashMap::new();
    variable_types.insert("flag".to_string(), DirectType::Scalar(ScalarKind::Bool));
    variable_types.insert("number".to_string(), DirectType::Scalar(ScalarKind::Int32));
    variable_types.insert("ratio".to_string(), DirectType::Scalar(ScalarKind::Float64));
    variable_types.insert("word".to_string(), DirectType::Opaque(Type::named("str")));
    variable_types.insert("node".to_string(), DirectType::Opaque(Type::named("Node")));
    variable_types.insert(
        "tasks".to_string(),
        DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![Type::Named("Task".to_string(), vec![Type::named("str")])],
        )),
    );
    variable_types.insert(
        "unit_tasks".to_string(),
        DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![Type::Named("Task".to_string(), Vec::new())],
        )),
    );
    variable_types.insert(
        "strings".to_string(),
        DirectType::Opaque(Type::Named("list".to_string(), vec![Type::named("str")])),
    );
    variable_types.insert(
        "result".to_string(),
        DirectType::Opaque(Type::Named(
            "Result".to_string(),
            vec![Type::named("str"), Type::named("io.Error")],
        )),
    );
    variable_types.insert(
        "point".to_string(),
        DirectType::PlainClass(super::PlainClassType {
            class_name: "Point".to_string(),
            fields: vec![
                super::PlainClassField {
                    name: "x".to_string(),
                    ty: DirectType::Scalar(ScalarKind::Float64),
                },
                super::PlainClassField {
                    name: "y".to_string(),
                    ty: DirectType::Scalar(ScalarKind::Float64),
                },
            ],
        }),
    );
    let mut returns = HashMap::new();
    returns.insert(
        "helper".to_string(),
        DirectType::Scalar(ScalarKind::Float64),
    );
    returns.insert(
        "Point::norm".to_string(),
        DirectType::Scalar(ScalarKind::Float64),
    );
    returns.insert(
        "Node::size".to_string(),
        DirectType::Scalar(ScalarKind::Int32),
    );
    let classes = HashMap::from([
        (
            "Point".to_string(),
            crate::mir::MirClass {
                name: "Point".to_string(),
                type_params: Vec::new(),
                fields: Vec::new(),
                methods: vec![crate::mir::MirMethod {
                    name: "norm".to_string(),
                    function_name: "Point::norm".to_string(),
                    receiver: Some(MirReceiverKind::Borrow),
                }],
            },
        ),
        (
            "Node".to_string(),
            crate::mir::MirClass {
                name: "Node".to_string(),
                type_params: Vec::new(),
                fields: Vec::new(),
                methods: vec![crate::mir::MirMethod {
                    name: "size".to_string(),
                    function_name: "Node::size".to_string(),
                    receiver: Some(MirReceiverKind::Borrow),
                }],
            },
        ),
    ]);

    assert_eq!(
        infer_operand_type(
            &Operand::Place("flag".to_string()),
            &variable_types,
            &HashMap::new()
        ),
        Some(DirectType::Scalar(ScalarKind::Bool))
    );
    assert_eq!(
        infer_operand_type(
            &Operand::Place("point.x".to_string()),
            &variable_types,
            &HashMap::new()
        ),
        Some(DirectType::Scalar(ScalarKind::Float64))
    );
    assert_eq!(
        infer_operand_type(&Operand::Bool(true), &variable_types, &classes),
        Some(DirectType::Scalar(ScalarKind::Bool))
    );
    assert_eq!(
        infer_operand_type(&Operand::Float(1.25), &variable_types, &classes),
        Some(DirectType::Scalar(ScalarKind::Float64))
    );
    assert_eq!(
        infer_operand_type(&Operand::Unit, &variable_types, &classes),
        Some(DirectType::Scalar(ScalarKind::Unit))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Unary {
                op: UnaryOp::Not,
                value: Operand::Place("flag".to_string()),
                span: Span::new(1, 1),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Scalar(ScalarKind::Bool))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Unary {
                op: UnaryOp::Neg,
                value: Operand::Place("number".to_string()),
                span: Span::new(1, 1),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Scalar(ScalarKind::Int32))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Unary {
                op: UnaryOp::Neg,
                value: Operand::Place("ratio".to_string()),
                span: Span::new(1, 1),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Scalar(ScalarKind::Float64))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Unary {
                op: UnaryOp::Neg,
                value: Operand::Place("word".to_string()),
                span: Span::new(1, 1),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        None
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Binary {
                op: BinaryOp::Add,
                left: Operand::Place("number".to_string()),
                right: Operand::Int(2),
                span: Span::new(1, 1),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Scalar(ScalarKind::Int32))
    );
    for op in [BinaryOp::And, BinaryOp::Or] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Binary {
                    op,
                    left: Operand::Place("flag".to_string()),
                    right: Operand::Bool(false),
                    span: Span::new(1, 1),
                },
                &variable_types,
                &returns,
                &classes,
            ),
            Some(DirectType::Scalar(ScalarKind::Bool)),
            "boolean operator `{op:?}` should infer bool",
        );
    }
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Call {
                callee: CallTarget::Name("print".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Bool(true),
                    writeback_place: None,
                }],
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Scalar(ScalarKind::Unit))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Call {
                callee: CallTarget::Name("helper".to_string()),
                args: Vec::new(),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Scalar(ScalarKind::Float64))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::FormatString {
                parts: vec![MirFormatPart::Literal("hello".to_string())],
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Opaque(Type::named("str")))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Cast {
                value: Operand::Place("number".to_string()),
                ty: Type::named("float64"),
                span: Span::new(1, 1),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Scalar(ScalarKind::Float64))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::VecLiteral {
                element_type: Type::named("int32"),
                elements: vec![Operand::Int(1), Operand::Int(2)],
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![Type::named("int32")],
        )))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::MapLiteral {
                key_type: Type::named("str"),
                value_type: Type::named("int32"),
                entries: vec![MirMapEntry {
                    key: Operand::String("count".to_string()),
                    value: Operand::Int(1),
                }],
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Opaque(Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        )))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::SetLiteral {
                element_type: Type::named("str"),
                elements: vec![Operand::String("x".to_string())],
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Opaque(Type::Named(
            "set".to_string(),
            vec![Type::named("str")],
        )))
    );
    for (name, expected) in [
        ("range", DirectType::Opaque(Type::named("Range"))),
        (
            "Queue",
            DirectType::Opaque(Type::Named(
                "Queue".to_string(),
                vec![Type::named("Unknown")],
            )),
        ),
        (
            "list",
            DirectType::Opaque(Type::Named(
                "list".to_string(),
                vec![Type::named("Unknown")],
            )),
        ),
        (
            "set",
            DirectType::Opaque(Type::Named("set".to_string(), vec![Type::named("Unknown")])),
        ),
        (
            "dict",
            DirectType::Opaque(Type::Named(
                "dict".to_string(),
                vec![Type::named("Unknown"), Type::named("Unknown")],
            )),
        ),
        ("TaskGroup", DirectType::Opaque(Type::named("TaskGroup"))),
        ("cancelled", DirectType::Scalar(ScalarKind::Bool)),
        ("yield_now", DirectType::Scalar(ScalarKind::Unit)),
        ("sleep", DirectType::Scalar(ScalarKind::Unit)),
        (
            "parse_int32",
            DirectType::Opaque(Type::Named(
                "Result".to_string(),
                vec![Type::named("int32"), Type::named("str")],
            )),
        ),
        (
            "parse_int64",
            DirectType::Opaque(Type::Named(
                "Result".to_string(),
                vec![Type::named("int64"), Type::named("str")],
            )),
        ),
        (
            "parse_float64",
            DirectType::Opaque(Type::Named(
                "Result".to_string(),
                vec![Type::named("float64"), Type::named("str")],
            )),
        ),
    ] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Call {
                    callee: CallTarget::Name(name.to_string()),
                    args: Vec::new(),
                },
                &variable_types,
                &returns,
                &classes,
            ),
            Some(expected),
            "expected builtin `{name}` to infer correctly",
        );
    }

    for (name, expected_variant) in [("wait_any", "WaitAny"), ("wait_all", "WaitAll")] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Call {
                    callee: CallTarget::Name(name.to_string()),
                    args: vec![MirArg {
                        name: None,
                        value: Operand::Place("tasks".to_string()),
                        writeback_place: None,
                    }],
                },
                &variable_types,
                &returns,
                &classes,
            ),
            Some(DirectType::Opaque(Type::Named(
                expected_variant.to_string(),
                vec![Type::named("str")],
            ))),
            "expected `{name}` to infer the task payload type",
        );
    }
    for (name, args) in [
        ("wait_any", Vec::new()),
        (
            "wait_all",
            vec![MirArg {
                name: None,
                value: Operand::Place("flag".to_string()),
                writeback_place: None,
            }],
        ),
    ] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Call {
                    callee: CallTarget::Name(name.to_string()),
                    args,
                },
                &variable_types,
                &returns,
                &classes,
            ),
            Some(DirectType::Opaque(Type::Named(
                if name == "wait_any" {
                    "WaitAny".to_string()
                } else {
                    "WaitAll".to_string()
                },
                vec![Type::named("Unknown")],
            ))),
            "expected `{name}` to fall back to an unknown task payload",
        );
    }
    for (place, expected_payload) in [
        ("unit_tasks", Type::Unit),
        ("strings", Type::named("Unknown")),
    ] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Call {
                    callee: CallTarget::Name("wait_any".to_string()),
                    args: vec![MirArg {
                        name: None,
                        value: Operand::Place(place.to_string()),
                        writeback_place: None,
                    }],
                },
                &variable_types,
                &returns,
                &classes,
            ),
            Some(DirectType::Opaque(Type::Named(
                "WaitAny".to_string(),
                vec![expected_payload],
            ))),
            "expected wait_any on `{place}` to infer the maintained fallback payload",
        );
    }

    let named_type = |name: &str| Type::Named(name.to_string(), Vec::new());
    let result_type =
        |ok: Type, err: Type| DirectType::Opaque(Type::Named("Result".to_string(), vec![ok, err]));
    let io_error = named_type("io.Error");
    let process_error = named_type("process.Error");
    for (name, expected) in [
        ("io::write", result_type(Type::Unit, io_error.clone())),
        ("io::flush", result_type(Type::Unit, io_error.clone())),
        (
            "io::read_line",
            result_type(
                Type::Named("Option".to_string(), vec![Type::named("str")]),
                io_error.clone(),
            ),
        ),
        ("fs::exists", DirectType::Scalar(ScalarKind::Bool)),
        (
            "fs::read_to_string",
            result_type(Type::named("str"), io_error.clone()),
        ),
        (
            "fs::read_bytes",
            result_type(
                Type::Named("list".to_string(), vec![Type::named("uint8")]),
                io_error.clone(),
            ),
        ),
        (
            "fs::write_string",
            result_type(Type::Unit, io_error.clone()),
        ),
        ("fs::write_bytes", result_type(Type::Unit, io_error.clone())),
        (
            "fs::append_string",
            result_type(Type::Unit, io_error.clone()),
        ),
        (
            "fs::append_bytes",
            result_type(Type::Unit, io_error.clone()),
        ),
        ("fs::create_dir", result_type(Type::Unit, io_error.clone())),
        ("fs::remove_file", result_type(Type::Unit, io_error.clone())),
        (
            "fs::read_dir",
            result_type(
                Type::Named("list".to_string(), vec![Type::named("str")]),
                io_error.clone(),
            ),
        ),
        (
            "fs::open",
            result_type(named_type("fs.File"), io_error.clone()),
        ),
        (
            "fs::create",
            result_type(named_type("fs.File"), io_error.clone()),
        ),
        (
            "fs::append",
            result_type(named_type("fs.File"), io_error.clone()),
        ),
        (
            "process::inherit",
            DirectType::Opaque(named_type("process.Stdio")),
        ),
        (
            "process::null",
            DirectType::Opaque(named_type("process.Stdio")),
        ),
        (
            "process::pipe",
            DirectType::Opaque(named_type("process.Stdio")),
        ),
        (
            "process::supervisor",
            DirectType::Opaque(named_type("process.Supervisor")),
        ),
        (
            "process::start",
            result_type(named_type("process.Child"), process_error.clone()),
        ),
        (
            "process::run",
            result_type(named_type("process.Completed"), process_error.clone()),
        ),
        (
            "net::connect",
            result_type(named_type("net.TcpStream"), io_error.clone()),
        ),
        (
            "net::connect_timeout",
            result_type(named_type("net.TcpStream"), io_error.clone()),
        ),
        (
            "net::listen",
            result_type(named_type("net.TcpListener"), io_error.clone()),
        ),
        (
            "net::udp_bind",
            result_type(named_type("net.UdpSocket"), io_error.clone()),
        ),
        (
            "net::unix_listen",
            result_type(named_type("net.UnixListener"), io_error.clone()),
        ),
        (
            "net::unix_connect",
            result_type(named_type("net.UnixStream"), io_error.clone()),
        ),
        (
            "net::unix_connect_timeout",
            result_type(named_type("net.UnixStream"), io_error.clone()),
        ),
        (
            "net::tls_listen",
            result_type(named_type("net.TlsListener"), io_error.clone()),
        ),
        (
            "net::tls_connect",
            result_type(named_type("net.TlsStream"), io_error.clone()),
        ),
        (
            "net::tls_connect_timeout",
            result_type(named_type("net.TlsStream"), io_error.clone()),
        ),
        (
            "net::http_listen",
            result_type(named_type("net.HttpListener"), io_error.clone()),
        ),
        (
            "net::http_request_text",
            result_type(named_type("net.HttpResponse"), io_error.clone()),
        ),
        (
            "net::http_request_text_timeout",
            result_type(named_type("net.HttpResponse"), io_error.clone()),
        ),
        (
            "net::http_request_bytes",
            result_type(named_type("net.HttpResponse"), io_error.clone()),
        ),
        (
            "net::http_request_bytes_timeout",
            result_type(named_type("net.HttpResponse"), io_error.clone()),
        ),
        (
            "net::websocket_listen",
            result_type(named_type("net.WebSocketListener"), io_error.clone()),
        ),
        (
            "net::websocket_connect",
            result_type(named_type("net.WebSocket"), io_error.clone()),
        ),
        (
            "net::websocket_connect_timeout",
            result_type(named_type("net.WebSocket"), io_error.clone()),
        ),
    ] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Call {
                    callee: CallTarget::Name(name.to_string()),
                    args: Vec::new(),
                },
                &variable_types,
                &returns,
                &classes,
            ),
            Some(expected),
            "expected direct builtin `{name}` to infer correctly",
        );
    }

    for (object, field, expected) in [
        (
            Operand::Place("ratio".to_string()),
            "sqrt",
            Some(DirectType::Scalar(ScalarKind::Float64)),
        ),
        (
            Operand::Place("number".to_string()),
            "to_string",
            Some(DirectType::Opaque(Type::named("str"))),
        ),
        (
            Operand::Place("point".to_string()),
            "norm",
            Some(DirectType::Scalar(ScalarKind::Float64)),
        ),
        (
            Operand::Place("node".to_string()),
            "size",
            Some(DirectType::Scalar(ScalarKind::Int32)),
        ),
        (
            Operand::Place("word".to_string()),
            "missing",
            Some(DirectType::Opaque(Type::named("Unknown"))),
        ),
        (Operand::Place("flag".to_string()), "missing", None),
    ] {
        assert_eq!(
            infer_rvalue_type(
                &Rvalue::Call {
                    callee: CallTarget::Member {
                        object,
                        field: field.to_string(),
                        receiver_place: None,
                    },
                    args: Vec::new(),
                },
                &variable_types,
                &returns,
                &classes,
            ),
            expected,
            "expected direct member `{field}` to infer correctly",
        );
    }

    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Try {
                value: Operand::Place("result".to_string()),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Opaque(Type::named("str")))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Try {
                value: Operand::Place("flag".to_string()),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Opaque(Type::named("Unknown")))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Construct {
                class_name: "Point".to_string(),
                fields: Vec::new(),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::PlainClass(PlainClassType {
            class_name: "Point".to_string(),
            fields: Vec::new(),
        }))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::Member {
                object: Operand::Place("point".to_string()),
                field: "y".to_string(),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Scalar(ScalarKind::Float64))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::EnumVariant {
                enum_name: "Option".to_string(),
                variant_name: "None".to_string(),
                payloads: Vec::new(),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Opaque(Type::named("Option")))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::VariantPayload {
                scrutinee: Operand::Place("flag".to_string()),
                variant_name: "Ready".to_string(),
                index: 0,
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Opaque(Type::named("Unknown")))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::StartTask {
                returns_handle: true,
                result_is_copy: true,
                stack_size: None,
                task_group: Operand::Unit,
                function: test_function_operand("helper", Vec::new(), Type::named("float64"),),
                args: Vec::new(),
                span: Span::new(1, 1),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Opaque(Type::Named(
            "Task".to_string(),
            vec![Type::named("float64")],
        )))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::StartTask {
                returns_handle: false,
                result_is_copy: true,
                stack_size: None,
                task_group: Operand::Unit,
                function: test_function_operand("helper", Vec::new(), Type::named("float64"),),
                args: Vec::new(),
                span: Span::new(1, 1),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        Some(DirectType::Scalar(ScalarKind::Unit))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::StartTask {
                returns_handle: true,
                result_is_copy: true,
                stack_size: None,
                task_group: Operand::Unit,
                function: Operand::Place("missing".to_string()),
                args: Vec::new(),
                span: Span::new(1, 1),
            },
            &variable_types,
            &returns,
            &classes,
        ),
        None
    );
}

#[test]
fn validate_operand_accepts_nested_places() {
    validate_operand(&Operand::Place("point.x".to_string()))
        .expect("nested places should now validate directly");
}

#[test]
fn direct_validation_rejects_move_place_in_non_consuming_expressions() {
    let function = MirFunction {
        name: "main".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: Vec::new(),
        local_types: vec![MirLocalType {
            name: "value".to_string(),
            ty: Type::named("int32"),
        }],
        return_type: Type::named("int32"),
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: vec![Instruction::Assign {
                target: "%negated".to_string(),
                value: Rvalue::Unary {
                    op: UnaryOp::Neg,
                    value: Operand::MovePlace("value".to_string()),
                    span: Span::new(1, 1),
                },
            }],
            terminator: Terminator::Return(Operand::Int(0)),
        }],
    };

    let error = validate_function(&function, &HashMap::new())
        .expect_err("MovePlace must be rejected for a non-consuming unary read");
    assert!(error.contains("only permits `MovePlace` in consuming contexts"));

    let mut consuming = function;
    consuming.blocks[0].instructions[0] = Instruction::Assign {
        target: "%moved".to_string(),
        value: Rvalue::Use(Operand::MovePlace("value".to_string())),
    };
    validate_function(&consuming, &HashMap::new())
        .expect("MovePlace should remain valid for a consuming assignment");

    let task_start = Rvalue::StartTask {
        returns_handle: true,
        result_is_copy: true,
        stack_size: Some(Operand::MovePlace("stack_bytes".to_string())),
        task_group: Operand::Place("group".to_string()),
        function: test_function_operand("worker", Vec::new(), Type::Unit),
        args: Vec::new(),
        span: Span::new(1, 1),
    };
    let error = validate_rvalue(&task_start, &HashMap::new())
        .expect_err("a task stack override is a non-consuming scalar read");
    assert_eq!(
        error,
        "direct backend only permits `MovePlace` in consuming contexts, not in a task stack size"
    );

    let task_start = Rvalue::StartTask {
        returns_handle: true,
        result_is_copy: true,
        stack_size: Some(Operand::Place("stack_bytes".to_string())),
        task_group: Operand::MovePlace("group".to_string()),
        function: test_function_operand("worker", Vec::new(), Type::Unit),
        args: Vec::new(),
        span: Span::new(1, 1),
    };
    let error = validate_rvalue(&task_start, &HashMap::new())
        .expect_err("a task-group receiver must stay live until task registration completes");
    assert_eq!(
        error,
        "direct backend only permits `MovePlace` in consuming contexts, not in a task-group receiver"
    );

    let task_start = Rvalue::StartTask {
        returns_handle: true,
        result_is_copy: true,
        stack_size: Some(Operand::Place("stack_bytes".to_string())),
        task_group: Operand::Place("group".to_string()),
        function: test_function_operand("worker", Vec::new(), Type::Unit),
        args: Vec::new(),
        span: Span::new(1, 1),
    };
    validate_rvalue(&task_start, &HashMap::new())
        .expect("shared reads of a stack override and task-group receiver must validate");
}

#[test]
fn ensure_direct_type_maps_runtime_backed_types_to_opaque_values() {
    let ty = ensure_direct_type(&Type::named("str"), &HashMap::new(), "test type")
        .expect("runtime-backed types should still be representable directly");
    assert_eq!(ty, DirectType::Opaque(Type::named("str")));
}

#[test]
fn native_codegen_receiver_and_type_param_helpers_cover_missing_receiver_and_terminal_types() {
    let function = MirFunction {
        name: "Widget.read".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: Some(MirReceiverKind::Borrow),
        params: Vec::new(),
        local_types: Vec::new(),
        return_type: Type::named("int32"),
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: Vec::new(),
            terminator: Terminator::Return(Operand::Int(0)),
        }],
    };
    let error = super::receiver_type(&function, &HashMap::new())
        .expect_err("receiver functions without a self local should be rejected");
    assert!(error.contains("could not find receiver local type for `Widget.read`"));

    let mut collected = BTreeSet::new();
    collect_type_params_from_type(&Type::Unit, &mut collected);
    collect_type_params_from_type(&Type::Module("pkg.tools".to_string()), &mut collected);
    assert!(collected.is_empty());
}

#[test]
fn signature_helpers_flatten_plain_class_abi_types() {
    let mut classes = HashMap::new();
    classes.insert(
        "Point".to_string(),
        crate::mir::MirClass {
            name: "Point".to_string(),
            type_params: Vec::new(),
            fields: vec![
                crate::mir::MirClassField {
                    name: "x".to_string(),
                    ty: Type::named("float64"),
                },
                crate::mir::MirClassField {
                    name: "y".to_string(),
                    ty: Type::named("float64"),
                },
            ],
            methods: Vec::new(),
        },
    );
    let function = MirFunction {
        name: "demo".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: crate::diag::Span::new(1, 1),
        receiver: Some(MirReceiverKind::Borrow),
        params: vec![crate::mir::MirParam {
            name: "other".to_string(),
            passing: MirReceiverKind::Value,
            ty: Type::named("Point"),
            default_function: None,
        }],
        local_types: vec![crate::mir::MirLocalType {
            name: "self".to_string(),
            ty: Type::named("Point"),
        }],
        return_type: Type::named("float64"),
        entry: "entry".to_string(),
        blocks: Vec::new(),
    };

    let sig = signature_for(
        &function,
        &classes,
        cranelift_codegen::isa::CallConv::SystemV,
    )
    .expect("signature should flatten point receiver and param");
    let main_sig = main_signature(cranelift_codegen::isa::CallConv::SystemV);

    assert_eq!(sig.params.len(), 4);
    assert_eq!(sig.returns.len(), 1);
    assert_eq!(main_sig.returns.len(), 1);

    let bool_ty = DirectType::Scalar(ScalarKind::Bool);
    assert_eq!(bool_ty.abi_types(), vec![cranelift_codegen::ir::types::I64]);
    assert_eq!(bool_ty.value_count(), 1);
    assert_eq!(bool_ty.scalar_kind(), Some(ScalarKind::Bool));
    assert!(!ScalarKind::Bool.is_float());
    assert!(ScalarKind::Float64.is_float());

    let point_ty = DirectType::PlainClass(PlainClassType {
        class_name: "Point".to_string(),
        fields: vec![
            PlainClassField {
                name: "x".to_string(),
                ty: DirectType::Scalar(ScalarKind::Float64),
            },
            PlainClassField {
                name: "visible".to_string(),
                ty: DirectType::Scalar(ScalarKind::Bool),
            },
        ],
    });
    assert_eq!(
        point_ty.abi_types(),
        vec![
            cranelift_codegen::ir::types::F64,
            cranelift_codegen::ir::types::I64,
        ]
    );
    assert_eq!(point_ty.value_count(), 2);
    assert_eq!(
        point_ty.field_slice("x"),
        Some((0, 1, DirectType::Scalar(ScalarKind::Float64)))
    );
    assert_eq!(
        point_ty.field_slice("visible"),
        Some((1, 2, DirectType::Scalar(ScalarKind::Bool)))
    );
    assert_eq!(point_ty.field_slice("missing"), None);
    assert_eq!(render_direct_type(&point_ty), "Point");
}

#[test]
fn cleanup_place_type_resolves_receivers_params_locals_and_inferred_values() {
    let resource_class = crate::mir::MirClass {
        name: "Resource".to_string(),
        type_params: Vec::new(),
        fields: vec![crate::mir::MirClassField {
            name: "closed".to_string(),
            ty: Type::named("bool"),
        }],
        methods: Vec::new(),
    };
    let holder_class = crate::mir::MirClass {
        name: "Holder".to_string(),
        type_params: Vec::new(),
        fields: vec![crate::mir::MirClassField {
            name: "resource".to_string(),
            ty: Type::named("Resource"),
        }],
        methods: Vec::new(),
    };
    let classes = HashMap::from([
        ("Resource".to_string(), resource_class),
        ("Holder".to_string(), holder_class),
    ]);
    let function = MirFunction {
        name: "cleanup_demo".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: Some(MirReceiverKind::BorrowMut),
        params: vec![MirParam {
            name: "input".to_string(),
            passing: MirReceiverKind::Borrow,
            ty: Type::named("Holder"),
            default_function: None,
        }],
        local_types: vec![
            MirLocalType {
                name: "self".to_string(),
                ty: Type::named("Holder"),
            },
            MirLocalType {
                name: "local".to_string(),
                ty: Type::named("Resource"),
            },
        ],
        return_type: Type::Unit,
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: vec![Instruction::Assign {
                target: "%made".to_string(),
                value: Rvalue::Construct {
                    class_name: "Resource".to_string(),
                    fields: Vec::new(),
                },
            }],
            terminator: Terminator::Return(Operand::Unit),
        }],
    };
    let function_return_types = HashMap::new();

    assert_eq!(
        cleanup_place_type(
            &function,
            &classes,
            "self.resource.closed",
            &function_return_types,
        )
        .expect("receiver cleanup fields should resolve"),
        DirectType::Scalar(ScalarKind::Bool)
    );
    assert_eq!(
        render_direct_type(
            &cleanup_place_type(
                &function,
                &classes,
                "input.resource",
                &function_return_types
            )
            .expect("parameter cleanup fields should resolve")
        ),
        "Resource"
    );
    assert_eq!(
        render_direct_type(
            &cleanup_place_type(&function, &classes, "local", &function_return_types)
                .expect("typed locals should resolve as cleanup places")
        ),
        "Resource"
    );
    assert_eq!(
        cleanup_place_type(&function, &classes, "%made.closed", &function_return_types)
            .expect("assigned values should infer cleanup field types"),
        DirectType::Scalar(ScalarKind::Bool)
    );
    assert!(
        cleanup_place_type(&function, &classes, "missing", &function_return_types)
            .expect_err("unknown cleanup roots should be rejected")
            .contains("does not know cleanup place `missing`")
    );
    assert!(cleanup_place_type(
        &function,
        &classes,
        "self.resource.missing",
        &function_return_types,
    )
    .expect_err("unknown cleanup fields should be rejected")
    .contains("does not know cleanup field `missing`"));
}

#[test]
fn builtin_member_type_helpers_cover_collection_runtime_surface() {
    let classes = HashMap::new();

    assert_eq!(
        builtin_opaque_member_return_type(
            &Type::Named("str".to_string(), vec![]),
            "split",
            &classes,
        ),
        Some(DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![Type::named("str")],
        )))
    );
    assert_eq!(
        builtin_opaque_member_return_type(
            &Type::Named("list".to_string(), vec![Type::named("int32")]),
            "insert",
            &classes,
        ),
        Some(DirectType::Scalar(ScalarKind::Unit))
    );
    assert_eq!(
        builtin_opaque_member_return_type(
            &Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "items",
            &classes,
        ),
        Some(DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![Type::Tuple(vec![Type::named("str"), Type::named("int32")])],
        )))
    );
    assert_eq!(
        builtin_opaque_member_return_type(
            &Type::Named("set".to_string(), vec![Type::named("str")]),
            "__index_option",
            &classes,
        ),
        Some(DirectType::Opaque(Type::Named(
            "Option".to_string(),
            vec![Type::named("str")],
        )))
    );
    assert_eq!(
        builtin_opaque_member_return_type(
            &Type::Named("Task".to_string(), vec![Type::named("int32")]),
            "result",
            &classes,
        ),
        Some(DirectType::Opaque(Type::Named(
            "TaskResult".to_string(),
            vec![Type::named("int32")],
        )))
    );
    assert_eq!(
        builtin_opaque_member_return_type(
            &Type::Named("set".to_string(), vec![Type::named("str")]),
            "copy",
            &classes,
        ),
        Some(DirectType::Opaque(Type::Named(
            "set".to_string(),
            vec![Type::named("str")],
        )))
    );
    assert_eq!(
        builtin_opaque_member_return_type(
            &Type::Named("TaskGroup".to_string(), vec![]),
            "start",
            &classes,
        ),
        None
    );
    assert_eq!(
        builtin_opaque_member_return_type(
            &Type::Named("TaskGroup".to_string(), vec![]),
            "missing",
            &classes,
        ),
        None
    );
    for (object_ty, field, expected) in [
        (
            Type::Named("str".to_string(), vec![]),
            "replace",
            DirectType::Opaque(Type::named("str")),
        ),
        (
            Type::Named("str".to_string(), vec![]),
            "strip_prefix",
            DirectType::Opaque(Type::Named("Option".to_string(), vec![Type::named("str")])),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "clear",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "__index",
            DirectType::Scalar(ScalarKind::Int32),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "values",
            DirectType::Opaque(Type::Named("list".to_string(), vec![Type::named("int32")])),
        ),
        (
            Type::Named("set".to_string(), vec![Type::named("str")]),
            "remove",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            Type::Named("Queue".to_string(), vec![Type::named("int32")]),
            "put",
            DirectType::Opaque(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named("SendError".to_string(), vec![Type::named("int32")]),
                ],
            )),
        ),
        (
            Type::Named("TaskGroup".to_string(), vec![]),
            "cancel",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            Type::Named("Task".to_string(), vec![Type::named("int32")]),
            "result",
            DirectType::Opaque(Type::Named(
                "TaskResult".to_string(),
                vec![Type::named("int32")],
            )),
        ),
        (
            Type::Named("Queue".to_string(), vec![Type::named("int32")]),
            "get",
            DirectType::Opaque(Type::Named(
                "QueueReceive".to_string(),
                vec![Type::named("int32")],
            )),
        ),
    ] {
        assert_eq!(
            builtin_opaque_member_return_type(&object_ty, field, &classes),
            Some(expected),
            "expected `{object_ty}.{field}` to infer correctly",
        );
    }
    assert_eq!(
        builtin_opaque_member_return_type(&Type::Unit, "len", &classes),
        None
    );
    for (object_ty, field, expected) in [
        (
            Type::Named("list".to_string(), Vec::new()),
            "pop",
            DirectType::Opaque(Type::named("Unknown")),
        ),
        (
            Type::Named("dict".to_string(), vec![Type::named("str")]),
            "get",
            DirectType::Opaque(Type::Named(
                "Option".to_string(),
                vec![Type::named("Unknown")],
            )),
        ),
        (
            Type::Named("dict".to_string(), Vec::new()),
            "keys",
            DirectType::Opaque(Type::Named(
                "list".to_string(),
                vec![Type::named("Unknown")],
            )),
        ),
        (
            Type::Named("dict".to_string(), Vec::new()),
            "items",
            DirectType::Opaque(Type::Named(
                "list".to_string(),
                vec![Type::Tuple(vec![
                    Type::named("Unknown"),
                    Type::named("Unknown"),
                ])],
            )),
        ),
        (
            Type::Named("set".to_string(), Vec::new()),
            "__index_option",
            DirectType::Opaque(Type::Named(
                "Option".to_string(),
                vec![Type::named("Unknown")],
            )),
        ),
        (
            Type::Named("Queue".to_string(), Vec::new()),
            "put",
            DirectType::Opaque(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named("SendError".to_string(), vec![Type::named("Unknown")]),
                ],
            )),
        ),
        (
            Type::Named("Queue".to_string(), Vec::new()),
            "get",
            DirectType::Opaque(Type::Named(
                "QueueReceive".to_string(),
                vec![Type::named("Unknown")],
            )),
        ),
        (
            Type::Named("Task".to_string(), Vec::new()),
            "result",
            DirectType::Opaque(Type::Named("TaskResult".to_string(), vec![Type::Unit])),
        ),
    ] {
        assert_eq!(
            builtin_opaque_member_return_type(&object_ty, field, &classes),
            Some(expected),
            "expected malformed `{object_ty}.{field}` to use the defensive fallback type",
        );
    }
}

#[test]
fn direct_field_and_try_helpers_cover_remaining_direct_inference_paths() {
    let classes = HashMap::from([
        (
            "Entry".to_string(),
            crate::mir::MirClass {
                name: "Entry".to_string(),
                type_params: Vec::new(),
                fields: vec![crate::mir::MirClassField {
                    name: "name".to_string(),
                    ty: Type::named("str"),
                }],
                methods: Vec::new(),
            },
        ),
        (
            "Box".to_string(),
            crate::mir::MirClass {
                name: "Box".to_string(),
                type_params: vec!["T".to_string()],
                fields: vec![crate::mir::MirClassField {
                    name: "value".to_string(),
                    ty: Type::TypeParam("T".to_string()),
                }],
                methods: Vec::new(),
            },
        ),
    ]);
    let variable_types = HashMap::from([
        (
            "result".to_string(),
            DirectType::Opaque(Type::Named(
                "Result".to_string(),
                vec![Type::named("int32"), Type::named("str")],
            )),
        ),
        (
            "maybe".to_string(),
            DirectType::Opaque(Type::Named(
                "Option".to_string(),
                vec![Type::named("Entry")],
            )),
        ),
    ]);

    assert_eq!(
        direct_field_type(
            &DirectType::Opaque(Type::Named("Entry".to_string(), vec![])),
            "name",
            &classes,
        ),
        Some(DirectType::Opaque(Type::named("str")))
    );
    assert_eq!(
        direct_field_type(
            &DirectType::Opaque(Type::Named("Entry".to_string(), vec![Type::named("int32")])),
            "name",
            &classes,
        ),
        None
    );
    assert_eq!(
        direct_field_type(
            &DirectType::Opaque(Type::Named("Box".to_string(), vec![Type::named("str")],)),
            "value",
            &classes,
        ),
        Some(DirectType::Opaque(Type::named("str")))
    );
    assert_eq!(
        infer_try_type(
            &Operand::Place("result".to_string()),
            &variable_types,
            &classes
        ),
        Some(DirectType::Scalar(ScalarKind::Int32))
    );
    assert_eq!(
        infer_operand_type(&Operand::Unit, &variable_types, &classes),
        Some(DirectType::Scalar(ScalarKind::Unit))
    );
    assert_eq!(
        infer_operand_type(
            &Operand::String("aura".to_string()),
            &variable_types,
            &classes,
        ),
        Some(DirectType::Opaque(Type::named("str")))
    );
    assert_eq!(
        infer_operand_type(&Operand::Duration(5), &variable_types, &classes),
        Some(DirectType::Opaque(Type::named("Duration")))
    );
    assert_eq!(
        infer_operand_type(
            &Operand::Int((i64::MAX as u128) + 1),
            &variable_types,
            &classes,
        ),
        Some(DirectType::Opaque(Type::named("Unknown")))
    );
    assert_eq!(
        infer_rvalue_type(
            &Rvalue::VariantPayload {
                scrutinee: Operand::Place("maybe".to_string()),
                variant_name: "Some".to_string(),
                index: 0,
            },
            &variable_types,
            &HashMap::new(),
            &classes,
        ),
        Some(DirectType::Opaque(Type::named("Entry")))
    );
}

#[test]
fn native_codegen_variant_payload_helpers_cover_builtin_result_shapes() {
    let classes = HashMap::new();
    let named = |name: &str| Type::Named(name.to_string(), Vec::new());
    let opaque_named =
        |name: &str, args: Vec<Type>| DirectType::Opaque(Type::Named(name.to_string(), args));
    let direct_int = DirectType::Scalar(ScalarKind::Int32);
    let direct_index = DirectType::Scalar(ScalarKind::Int64);
    let direct_string = DirectType::Opaque(Type::named("str"));
    let direct_vec_int =
        DirectType::Opaque(Type::Named("list".to_string(), vec![Type::named("int32")]));

    for (target, enum_name, variant, expected) in [
        (
            opaque_named("Option", vec![Type::named("int32")]),
            "Option",
            "Some",
            Some(vec![direct_int.clone()]),
        ),
        (
            opaque_named("Option", vec![Type::named("int32")]),
            "Option",
            "None",
            Some(Vec::new()),
        ),
        (
            opaque_named("Result", vec![Type::named("str"), named("io.Error")]),
            "Result",
            "Ok",
            Some(vec![direct_string.clone()]),
        ),
        (
            opaque_named("Result", vec![Type::named("str"), named("io.Error")]),
            "Result",
            "Err",
            Some(vec![DirectType::Opaque(named("io.Error"))]),
        ),
        (
            opaque_named("SendError", vec![Type::named("int32")]),
            "SendError",
            "Full",
            Some(vec![direct_int.clone()]),
        ),
        (
            opaque_named("QueueReceive", vec![Type::named("str")]),
            "QueueReceive",
            "Item",
            Some(vec![direct_string.clone()]),
        ),
        (
            opaque_named("QueueReceive", vec![Type::named("str")]),
            "QueueReceive",
            "Closed",
            Some(Vec::new()),
        ),
        (
            opaque_named("TaskResult", vec![Type::named("int32")]),
            "TaskResult",
            "Ready",
            Some(vec![direct_int.clone()]),
        ),
        (
            opaque_named("TaskResult", vec![Type::named("int32")]),
            "TaskResult",
            "Error",
            Some(vec![direct_string.clone()]),
        ),
        (
            opaque_named("TaskResult", vec![Type::named("int32")]),
            "TaskResult",
            "TimedOut",
            Some(Vec::new()),
        ),
        (
            opaque_named("WaitAny", vec![Type::named("str")]),
            "WaitAny",
            "Ready",
            Some(vec![direct_index.clone(), direct_string.clone()]),
        ),
        (
            opaque_named("WaitAny", vec![Type::named("str")]),
            "WaitAny",
            "Error",
            Some(vec![direct_index.clone(), direct_string.clone()]),
        ),
        (
            opaque_named("WaitAny", vec![Type::named("str")]),
            "WaitAny",
            "Cancelled",
            Some(Vec::new()),
        ),
        (
            opaque_named("WaitAll", vec![Type::named("int32")]),
            "WaitAll",
            "Ready",
            Some(vec![direct_vec_int.clone()]),
        ),
        (
            opaque_named("WaitAll", vec![Type::named("int32")]),
            "WaitAll",
            "Error",
            Some(vec![direct_index.clone(), direct_string.clone()]),
        ),
        (
            opaque_named("WaitAll", vec![Type::named("int32")]),
            "WaitAll",
            "TimedOut",
            Some(Vec::new()),
        ),
        (
            opaque_named(
                "SelectOutcome",
                vec![Type::named("str"), Type::named("int32")],
            ),
            "SelectOutcome",
            "Queue",
            Some(vec![
                direct_index.clone(),
                opaque_named("QueueReceive", vec![Type::named("str")]),
            ]),
        ),
        (
            opaque_named(
                "SelectOutcome",
                vec![Type::named("str"), Type::named("int32")],
            ),
            "SelectOutcome",
            "Task",
            Some(vec![
                direct_index.clone(),
                opaque_named("TaskResult", vec![Type::named("int32")]),
            ]),
        ),
        (
            opaque_named(
                "SelectOutcome",
                vec![Type::named("str"), Type::named("int32")],
            ),
            "SelectOutcome",
            "Deadline",
            Some(vec![direct_index.clone()]),
        ),
        (
            opaque_named(
                "SelectOutcome",
                vec![Type::named("str"), Type::named("int32")],
            ),
            "SelectOutcome",
            "Cancelled",
            Some(Vec::new()),
        ),
        (
            opaque_named("Result", vec![Type::named("str"), named("io.Error")]),
            "Option",
            "Some",
            None,
        ),
        (
            opaque_named("Result", vec![Type::named("str"), named("io.Error")]),
            "Result",
            "Missing",
            None,
        ),
    ] {
        assert_eq!(
            enum_variant_payload_types_for_target(&enum_name, variant, &target, &classes),
            expected,
            "unexpected payload types for {enum_name}.{variant}"
        );
    }
    assert_eq!(
        enum_variant_payload_types_for_target(
            "Result",
            "Ok",
            &DirectType::Scalar(ScalarKind::Int32),
            &classes,
        ),
        None
    );

    let variable_types = HashMap::from([
        (
            "maybe".to_string(),
            opaque_named("Option", vec![Type::named("int32")]),
        ),
        (
            "result".to_string(),
            opaque_named("Result", vec![Type::named("str"), named("io.Error")]),
        ),
        (
            "send".to_string(),
            opaque_named("SendError", vec![Type::named("int32")]),
        ),
        (
            "recv".to_string(),
            opaque_named("QueueReceive", vec![Type::named("str")]),
        ),
        (
            "task".to_string(),
            opaque_named("TaskResult", vec![Type::named("int32")]),
        ),
        (
            "any".to_string(),
            opaque_named("WaitAny", vec![Type::named("str")]),
        ),
        (
            "all".to_string(),
            opaque_named("WaitAll", vec![Type::named("int32")]),
        ),
        (
            "selected".to_string(),
            opaque_named(
                "SelectOutcome",
                vec![Type::named("str"), Type::named("int32")],
            ),
        ),
        ("count".to_string(), direct_int.clone()),
    ]);

    for (place, variant, index, expected) in [
        ("maybe", "Some", 0usize, Some(direct_int.clone())),
        ("result", "Ok", 0, Some(direct_string.clone())),
        (
            "result",
            "Err",
            0,
            Some(DirectType::Opaque(named("io.Error"))),
        ),
        ("send", "Closed", 0, Some(direct_int.clone())),
        ("recv", "Item", 0, Some(direct_string.clone())),
        ("task", "Ready", 0, Some(direct_int.clone())),
        ("task", "Error", 0, Some(direct_string.clone())),
        ("any", "Ready", 0, Some(direct_index.clone())),
        ("any", "Ready", 1, Some(direct_string.clone())),
        ("any", "Error", 1, Some(direct_string.clone())),
        ("all", "Ready", 0, Some(direct_vec_int.clone())),
        ("all", "Error", 0, Some(direct_index.clone())),
        ("all", "Error", 1, Some(direct_string.clone())),
        ("selected", "Queue", 0, Some(direct_index.clone())),
        (
            "selected",
            "Queue",
            1,
            Some(opaque_named("QueueReceive", vec![Type::named("str")])),
        ),
        ("selected", "Task", 0, Some(direct_index.clone())),
        (
            "selected",
            "Task",
            1,
            Some(opaque_named("TaskResult", vec![Type::named("int32")])),
        ),
        ("selected", "Deadline", 0, Some(direct_index.clone())),
        ("count", "Ready", 0, None),
        ("result", "Missing", 0, None),
    ] {
        assert_eq!(
            infer_variant_payload_type(
                &Operand::Place(place.to_string()),
                variant,
                index,
                &variable_types,
                &classes,
            ),
            expected,
            "unexpected inferred payload for {place}.{variant}[{index}]"
        );
    }
}

#[test]
fn native_codegen_helper_utilities_cover_signatures_wildcards_and_metadata() {
    let call_conv = cranelift_codegen::isa::CallConv::SystemV;
    let main_sig = main_signature(call_conv);
    assert_eq!(main_sig.params.len(), 0);
    assert_eq!(main_sig.returns.len(), 1);

    let thunk_sig = thunk_signature(call_conv);
    assert_eq!(thunk_sig.params.len(), 2);
    assert_eq!(thunk_sig.returns.len(), 1);

    assert_eq!(
        mangle_thunk_symbol("pkg.main::worker"),
        "aura_thunk_pkg_main__worker"
    );

    assert_eq!(
        direct_type_to_type(&DirectType::Scalar(ScalarKind::Unit)),
        Type::Unit
    );
    assert_eq!(
        direct_type_to_type(&DirectType::Scalar(ScalarKind::Float32)),
        Type::named("float32")
    );
    assert_eq!(
        direct_type_to_type(&DirectType::Scalar(ScalarKind::Float64)),
        Type::named("float64")
    );
    assert_eq!(
        direct_type_to_type(&DirectType::Scalar(ScalarKind::Bool)),
        Type::named("bool")
    );
    assert_eq!(
        direct_type_to_type(&DirectType::PlainClass(PlainClassType {
            class_name: "Point".to_string(),
            fields: vec![PlainClassField {
                name: "x".to_string(),
                ty: DirectType::Scalar(ScalarKind::Int32),
            }],
        })),
        Type::named("Point")
    );
    assert_eq!(
        direct_type_to_type(&DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![Type::named("int32")],
        ))),
        Type::Named("list".to_string(), vec![Type::named("int32")])
    );
    assert_eq!(
        DirectType::Scalar(ScalarKind::Bool).scalar_kind(),
        Some(ScalarKind::Bool)
    );
    assert_eq!(DirectType::Opaque(Type::named("str")).scalar_kind(), None);
    assert_eq!(
        render_direct_type(&DirectType::Scalar(ScalarKind::Float32)),
        "float32"
    );
    assert_eq!(
        render_direct_type(&DirectType::Scalar(ScalarKind::Float64)),
        "float64"
    );
    assert_eq!(
        render_direct_type(&DirectType::Scalar(ScalarKind::Bool)),
        "bool"
    );
    assert!(!ScalarKind::Bool.is_float());
    assert!(ScalarKind::Float32.is_float());
    assert_eq!(
        DirectType::Scalar(ScalarKind::Float64).abi_types(),
        vec![cranelift_codegen::ir::types::F64]
    );
    assert_eq!(
        DirectType::Opaque(Type::named("str")).abi_types(),
        vec![cranelift_codegen::ir::types::I64]
    );
    let flat_class = DirectType::PlainClass(PlainClassType {
        class_name: "Pair".to_string(),
        fields: vec![
            PlainClassField {
                name: "left".to_string(),
                ty: DirectType::Scalar(ScalarKind::Int32),
            },
            PlainClassField {
                name: "right".to_string(),
                ty: DirectType::Scalar(ScalarKind::Float64),
            },
        ],
    });
    assert_eq!(
        flat_class.abi_types(),
        vec![
            cranelift_codegen::ir::types::I64,
            cranelift_codegen::ir::types::F64,
        ]
    );
    assert_eq!(flat_class.value_count(), 2);
    assert_eq!(
        flat_class.field_slice("right"),
        Some((1, 2, DirectType::Scalar(ScalarKind::Float64)))
    );
    assert_eq!(flat_class.field_slice("missing"), None);

    let mut ctx = Context::new();
    ctx.func.signature = cranelift_codegen::ir::Signature::new(call_conv);
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut builder = cranelift_frontend::FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
    let block = builder.create_block();
    builder.switch_to_block(block);
    builder.seal_block(block);

    let scalar_zero = DirectType::Scalar(ScalarKind::Int32).zero_values(&mut builder);
    let opaque_zero = DirectType::Opaque(Type::named("str")).zero_values(&mut builder);
    let class_zero = flat_class.zero_values(&mut builder);
    assert_eq!(scalar_zero.len(), 1);
    assert_eq!(opaque_zero.len(), 1);
    assert_eq!(class_zero.len(), 2);
    builder.ins().return_(&[]);
    builder.finalize();

    assert!(is_numeric_type_name(&Type::named("uint64")));
    assert!(is_numeric_type_name(&Type::named("float32")));
    assert!(!is_numeric_type_name(&Type::named("str")));
    assert!(!is_numeric_type_name(&Type::Named(
        "list".to_string(),
        vec![Type::named("int32")],
    )));

    assert!(runtime_type_is_wildcard(&Type::TypeParam("T".to_string())));
    assert!(runtime_type_is_wildcard(&Type::named("Unknown")));
    assert!(runtime_type_is_wildcard(&Type::Named(
        "dict".to_string(),
        vec![Type::named("str"), Type::named("Unknown")],
    )));
    assert!(!runtime_type_is_wildcard(&Type::Named(
        "list".to_string(),
        vec![Type::named("int32")],
    )));
    assert!(!runtime_type_is_wildcard(&Type::Unit));

    let source = "def main() -> int32:\n    return 0\n";
    let mir = lower_source_to_mir(source).expect("simple source should lower");
    let object = emit_host_object_with_metadata(&mir, "/tmp/demo.au", source)
        .expect("metadata-backed object emission should succeed");
    assert!(!object.is_empty());

    let invalid_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Unreachable,
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    let error = emit_host_object(&invalid_module)
        .expect_err("invalid modules should be rejected before codegen");
    assert!(error.contains("does not yet support MIR terminator"));
}

#[test]
fn native_codegen_orders_named_builtin_args_and_reports_binding_errors() {
    let arg = |name: Option<&str>, value: u128| MirArg {
        name: name.map(str::to_string),
        value: Operand::Int(value),
        writeback_place: None,
    };
    let int_arg = |argument: &MirArg| match &argument.value {
        Operand::Int(value) => *value,
        other => panic!("expected int argument, found {other:?}"),
    };

    let mixed_args = vec![arg(Some("timeout"), 30), arg(None, 7)];
    let ordered = ordered_named_args(&["value", "timeout"], &mixed_args)
        .expect("named and positional arguments should bind by expected parameter order");
    assert_eq!(int_arg(ordered[0]), 7);
    assert_eq!(int_arg(ordered[1]), 30);

    let positional_after_named = vec![arg(Some("value"), 7), arg(None, 30)];
    let ordered = ordered_named_args(&["value", "timeout"], &positional_after_named)
        .expect("positional arguments should skip slots filled by named arguments");
    assert_eq!(int_arg(ordered[0]), 7);
    assert_eq!(int_arg(ordered[1]), 30);

    assert!(ordered_named_args(&["value"], &[arg(Some("missing"), 1)])
        .expect_err("unknown names should fail")
        .contains("does not recognize builtin argument `missing`"));
    assert!(
        ordered_named_args(&["value"], &[arg(Some("value"), 1), arg(Some("value"), 2)])
            .expect_err("duplicate names should fail")
            .contains("duplicate builtin argument `value`")
    );
    assert!(
        ordered_named_args(&["value"], &[arg(None, 1), arg(None, 2)])
            .expect_err("extra positional arguments should fail")
            .contains("too many builtin arguments")
    );
    assert!(ordered_named_args(&["value", "timeout"], &[arg(None, 1)])
        .expect_err("missing required arguments should fail")
        .contains("missing a builtin argument"));

    let optional = ordered_optional_named_args(&["default", "timeout"], &mixed_args)
        .expect("optional named arguments should preserve empty slots");
    assert_eq!(int_arg(optional[0].expect("default should bind")), 7);
    assert_eq!(int_arg(optional[1].expect("timeout should bind")), 30);

    let optional_after_named = vec![arg(Some("default"), 7), arg(None, 30)];
    let optional = ordered_optional_named_args(&["default", "timeout"], &optional_after_named)
        .expect("optional positional arguments should skip named slots");
    assert_eq!(int_arg(optional[0].expect("default should bind")), 7);
    assert_eq!(int_arg(optional[1].expect("timeout should bind")), 30);
    assert!(
        ordered_optional_named_args(&["timeout"], &[arg(Some("missing"), 1)])
            .expect_err("unknown optional names should fail")
            .contains("does not recognize builtin argument `missing`")
    );
    assert!(ordered_optional_named_args(
        &["timeout"],
        &[arg(Some("timeout"), 1), arg(Some("timeout"), 2)]
    )
    .expect_err("duplicate optional names should fail")
    .contains("duplicate builtin argument `timeout`"));
    assert!(
        ordered_optional_named_args(&["timeout"], &[arg(None, 1), arg(None, 2)])
            .expect_err("extra optional positional arguments should fail")
            .contains("too many builtin arguments")
    );
}

#[test]
fn native_codegen_builtin_member_tables_and_trait_lookup_cover_additional_paths() {
    let classes = HashMap::from([
        (
            "Node".to_string(),
            crate::mir::MirClass {
                name: "Node".to_string(),
                type_params: Vec::new(),
                fields: vec![crate::mir::MirClassField {
                    name: "next".to_string(),
                    ty: Type::named("Node"),
                }],
                methods: Vec::new(),
            },
        ),
        (
            "Box".to_string(),
            crate::mir::MirClass {
                name: "Box".to_string(),
                type_params: vec!["T".to_string()],
                fields: vec![crate::mir::MirClassField {
                    name: "value".to_string(),
                    ty: Type::TypeParam("T".to_string()),
                }],
                methods: Vec::new(),
            },
        ),
    ]);

    for (object_ty, field, expected) in [
        (
            Type::named("int32"),
            "to_string",
            Some(DirectType::Opaque(Type::named("str"))),
        ),
        (
            Type::named("float64"),
            "to_string",
            Some(DirectType::Opaque(Type::named("str"))),
        ),
        (
            Type::named("bool"),
            "to_string",
            Some(DirectType::Opaque(Type::named("str"))),
        ),
        (
            Type::named("str"),
            "len",
            direct_type(&Type::named("int64"), &classes),
        ),
        (
            Type::named("str"),
            "byte_len",
            direct_type(&Type::named("int64"), &classes),
        ),
        (
            Type::named("str"),
            "contains",
            Some(DirectType::Scalar(ScalarKind::Bool)),
        ),
        (
            Type::named("str"),
            "starts_with",
            Some(DirectType::Scalar(ScalarKind::Bool)),
        ),
        (
            Type::named("str"),
            "ends_with",
            Some(DirectType::Scalar(ScalarKind::Bool)),
        ),
        (
            Type::named("str"),
            "split",
            Some(DirectType::Opaque(Type::Named(
                "list".to_string(),
                vec![Type::named("str")],
            ))),
        ),
        (
            Type::named("str"),
            "replace",
            Some(DirectType::Opaque(Type::named("str"))),
        ),
        (
            Type::named("str"),
            "add",
            Some(DirectType::Opaque(Type::named("str"))),
        ),
        (
            Type::named("str"),
            "to_lower",
            Some(DirectType::Opaque(Type::named("str"))),
        ),
        (
            Type::named("str"),
            "to_upper",
            Some(DirectType::Opaque(Type::named("str"))),
        ),
        (
            Type::named("str"),
            "trim",
            Some(DirectType::Opaque(Type::named("str"))),
        ),
        (
            Type::named("str"),
            "clone",
            Some(DirectType::Opaque(Type::named("str"))),
        ),
        (
            Type::named("str"),
            "join",
            Some(DirectType::Opaque(Type::named("str"))),
        ),
        (
            Type::named("str"),
            "strip_prefix",
            Some(DirectType::Opaque(Type::Named(
                "Option".to_string(),
                vec![Type::named("str")],
            ))),
        ),
        (
            Type::named("str"),
            "strip_suffix",
            Some(DirectType::Opaque(Type::Named(
                "Option".to_string(),
                vec![Type::named("str")],
            ))),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "len",
            direct_type(&Type::named("int64"), &classes),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "is_empty",
            Some(DirectType::Scalar(ScalarKind::Bool)),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "copy",
            Some(DirectType::Opaque(Type::Named(
                "list".to_string(),
                vec![Type::named("int32")],
            ))),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "append",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "extend",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "clear",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "reverse",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "__set_index",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "swap",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "contains",
            Some(DirectType::Scalar(ScalarKind::Bool)),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "insert",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "pop",
            direct_type(&Type::named("int32"), &classes),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "get",
            Some(DirectType::Opaque(Type::Named(
                "Option".to_string(),
                vec![Type::named("int32")],
            ))),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "set",
            direct_type(&Type::named("int32"), &classes),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "remove",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "__index_option",
            Some(DirectType::Opaque(Type::Named(
                "Option".to_string(),
                vec![Type::named("int32")],
            ))),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            "__index",
            direct_type(&Type::named("int32"), &classes),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "len",
            direct_type(&Type::named("int64"), &classes),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "is_empty",
            Some(DirectType::Scalar(ScalarKind::Bool)),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "contains_key",
            Some(DirectType::Scalar(ScalarKind::Bool)),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "copy",
            Some(DirectType::Opaque(Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ))),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "get",
            Some(DirectType::Opaque(Type::Named(
                "Option".to_string(),
                vec![Type::named("int32")],
            ))),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "remove",
            Some(DirectType::Opaque(Type::Named(
                "Option".to_string(),
                vec![Type::named("int32")],
            ))),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "keys",
            Some(DirectType::Opaque(Type::Named(
                "list".to_string(),
                vec![Type::named("str")],
            ))),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "values",
            Some(DirectType::Opaque(Type::Named(
                "list".to_string(),
                vec![Type::named("int32")],
            ))),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "items",
            Some(DirectType::Opaque(Type::Named(
                "list".to_string(),
                vec![Type::Tuple(vec![Type::named("str"), Type::named("int32")])],
            ))),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "clear",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "update",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "__index",
            direct_type(&Type::named("int32"), &classes),
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "__set_index",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("set".to_string(), vec![Type::named("str")]),
            "len",
            direct_type(&Type::named("int64"), &classes),
        ),
        (
            Type::Named("set".to_string(), vec![Type::named("str")]),
            "is_empty",
            Some(DirectType::Scalar(ScalarKind::Bool)),
        ),
        (
            Type::Named("set".to_string(), vec![Type::named("str")]),
            "copy",
            Some(DirectType::Opaque(Type::Named(
                "set".to_string(),
                vec![Type::named("str")],
            ))),
        ),
        (
            Type::Named("set".to_string(), vec![Type::named("str")]),
            "contains",
            Some(DirectType::Scalar(ScalarKind::Bool)),
        ),
        (
            Type::Named("set".to_string(), vec![Type::named("str")]),
            "add",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("set".to_string(), vec![Type::named("str")]),
            "remove",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("set".to_string(), vec![Type::named("str")]),
            "__index_option",
            Some(DirectType::Opaque(Type::Named(
                "Option".to_string(),
                vec![Type::named("str")],
            ))),
        ),
        (
            Type::Named("Queue".to_string(), vec![Type::named("int32")]),
            "put",
            Some(DirectType::Opaque(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named("SendError".to_string(), vec![Type::named("int32")]),
                ],
            ))),
        ),
        (
            Type::Named("Queue".to_string(), vec![Type::named("int32")]),
            "try_put",
            Some(DirectType::Opaque(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named("SendError".to_string(), vec![Type::named("int32")]),
                ],
            ))),
        ),
        (
            Type::Named("Queue".to_string(), vec![Type::named("int32")]),
            "get",
            Some(DirectType::Opaque(Type::Named(
                "QueueReceive".to_string(),
                vec![Type::named("int32")],
            ))),
        ),
        (
            Type::Named("Queue".to_string(), vec![Type::named("int32")]),
            "__get_in_task_group",
            Some(DirectType::Opaque(Type::Named(
                "QueueReceive".to_string(),
                vec![Type::named("int32")],
            ))),
        ),
        (
            Type::Named("Queue".to_string(), vec![Type::named("int32")]),
            "__get_with_registered_producers",
            Some(DirectType::Opaque(Type::Named(
                "QueueReceive".to_string(),
                vec![Type::named("int32")],
            ))),
        ),
        (
            Type::Named("Queue".to_string(), vec![Type::named("int32")]),
            "close",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("Task".to_string(), vec![Type::named("int32")]),
            "result",
            Some(DirectType::Opaque(Type::Named(
                "TaskResult".to_string(),
                vec![Type::named("int32")],
            ))),
        ),
        (
            Type::Named("TaskGroup".to_string(), vec![]),
            "cancel",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (
            Type::Named("TaskGroup".to_string(), vec![]),
            "close",
            Some(DirectType::Scalar(ScalarKind::Unit)),
        ),
        (Type::named("str"), "missing", None),
    ] {
        assert_eq!(
            builtin_opaque_member_return_type(&object_ty, field, &classes),
            expected,
            "unexpected direct member type for `{object_ty}.{field}`"
        );
    }

    let named_type = |name: &str| Type::Named(name.to_string(), Vec::new());
    let vec_type = |inner: Type| Type::Named("list".to_string(), vec![inner]);
    let option_type = |inner: Type| Type::Named("Option".to_string(), vec![inner]);
    let result_direct =
        |ok: Type, err: Type| DirectType::Opaque(Type::Named("Result".to_string(), vec![ok, err]));
    let direct_named = |name: &str| DirectType::Opaque(named_type(name));
    let direct_vec = |inner: Type| DirectType::Opaque(vec_type(inner));
    let io_error = named_type("io.Error");
    let process_error = named_type("process.Error");
    for (object_ty, field, expected) in [
        (
            named_type("fs.File"),
            "read_all",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("fs.File"),
            "read_bytes",
            result_direct(vec_type(Type::named("uint8")), io_error.clone()),
        ),
        (
            named_type("fs.File"),
            "write_all",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("fs.File"),
            "write_bytes",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("fs.File"),
            "flush",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("fs.File"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("process.Child"),
            "stdin",
            DirectType::Opaque(option_type(named_type("process.Pipe"))),
        ),
        (
            named_type("process.Child"),
            "stdout",
            DirectType::Opaque(option_type(named_type("process.Pipe"))),
        ),
        (
            named_type("process.Child"),
            "stderr",
            DirectType::Opaque(option_type(named_type("process.Pipe"))),
        ),
        (
            named_type("process.Child"),
            "wait",
            direct_named("process.Wait"),
        ),
        (
            named_type("process.Child"),
            "kill",
            result_direct(Type::Unit, process_error.clone()),
        ),
        (
            named_type("process.Child"),
            "terminate",
            result_direct(Type::Unit, process_error.clone()),
        ),
        (
            named_type("process.Child"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("process.Pipe"),
            "read_all",
            result_direct(Type::named("str"), process_error.clone()),
        ),
        (
            named_type("process.Pipe"),
            "read_line",
            result_direct(option_type(Type::named("str")), process_error.clone()),
        ),
        (
            named_type("process.Pipe"),
            "read_bytes",
            result_direct(
                option_type(vec_type(Type::named("uint8"))),
                process_error.clone(),
            ),
        ),
        (
            named_type("process.Pipe"),
            "write_all",
            result_direct(Type::Unit, process_error.clone()),
        ),
        (
            named_type("process.Pipe"),
            "write_bytes",
            result_direct(Type::Unit, process_error.clone()),
        ),
        (
            named_type("process.Pipe"),
            "flush",
            result_direct(Type::Unit, process_error.clone()),
        ),
        (
            named_type("process.Pipe"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("process.Completed"),
            "status",
            direct_named("process.ExitStatus"),
        ),
        (
            named_type("process.Completed"),
            "success",
            DirectType::Scalar(ScalarKind::Bool),
        ),
        (
            named_type("process.Completed"),
            "stdout",
            direct_named("str"),
        ),
        (
            named_type("process.Completed"),
            "stderr",
            direct_named("str"),
        ),
        (
            named_type("process.Completed"),
            "stdout_bytes",
            direct_vec(Type::named("uint8")),
        ),
        (
            named_type("process.Completed"),
            "stderr_bytes",
            direct_vec(Type::named("uint8")),
        ),
        (
            named_type("process.Supervisor"),
            "start",
            result_direct(Type::Unit, process_error.clone()),
        ),
        (
            named_type("process.Supervisor"),
            "stop",
            result_direct(Type::Unit, process_error.clone()),
        ),
        (
            named_type("process.Supervisor"),
            "wait",
            direct_named("process.SupervisorWait"),
        ),
        (
            named_type("process.Supervisor"),
            "wait_or_none",
            result_direct(
                option_type(named_type("process.SupervisorEvent")),
                process_error.clone(),
            ),
        ),
        (
            named_type("process.Supervisor"),
            "is_empty",
            DirectType::Scalar(ScalarKind::Bool),
        ),
        (
            named_type("process.Supervisor"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.TcpListener"),
            "accept",
            result_direct(named_type("net.TcpStream"), io_error.clone()),
        ),
        (
            named_type("net.TcpListener"),
            "local_addr",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.TcpListener"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.TcpStream"),
            "read_all",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.TcpStream"),
            "local_addr",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.TcpStream"),
            "peer_addr",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.TcpStream"),
            "read_line",
            result_direct(option_type(Type::named("str")), io_error.clone()),
        ),
        (
            named_type("net.TcpStream"),
            "read_bytes",
            result_direct(
                option_type(vec_type(Type::named("uint8"))),
                io_error.clone(),
            ),
        ),
        (
            named_type("net.TcpStream"),
            "read_exact",
            result_direct(vec_type(Type::named("uint8")), io_error.clone()),
        ),
        (
            named_type("net.TcpStream"),
            "write_all",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.TcpStream"),
            "write_bytes",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.TcpStream"),
            "flush",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.TcpStream"),
            "shutdown_read",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.TcpStream"),
            "shutdown_write",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.TcpStream"),
            "shutdown_both",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.TcpStream"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.UdpSocket"),
            "send_text",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.UdpSocket"),
            "send_bytes",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.UdpSocket"),
            "recv",
            result_direct(
                option_type(vec_type(Type::named("uint8"))),
                io_error.clone(),
            ),
        ),
        (
            named_type("net.UdpSocket"),
            "recv_from",
            result_direct(option_type(named_type("net.UdpDatagram")), io_error.clone()),
        ),
        (
            named_type("net.UdpSocket"),
            "local_addr",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.UdpSocket"),
            "peer_addr",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.UdpSocket"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.UdpDatagram"),
            "address",
            direct_named("str"),
        ),
        (
            named_type("net.UdpDatagram"),
            "bytes",
            direct_vec(Type::named("uint8")),
        ),
        (
            named_type("net.UdpDatagram"),
            "text",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.HttpListener"),
            "accept",
            result_direct(named_type("net.HttpExchange"), io_error.clone()),
        ),
        (
            named_type("net.HttpListener"),
            "local_addr",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.HttpListener"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.HttpExchange"),
            "method",
            direct_named("str"),
        ),
        (named_type("net.HttpExchange"), "path", direct_named("str")),
        (
            named_type("net.HttpExchange"),
            "headers",
            DirectType::Opaque(Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("str")],
            )),
        ),
        (
            named_type("net.HttpResponse"),
            "headers",
            DirectType::Opaque(Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("str")],
            )),
        ),
        (
            named_type("net.HttpExchange"),
            "body_text",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.HttpResponse"),
            "text",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.HttpExchange"),
            "body_bytes",
            direct_vec(Type::named("uint8")),
        ),
        (
            named_type("net.HttpResponse"),
            "bytes",
            direct_vec(Type::named("uint8")),
        ),
        (
            named_type("net.HttpExchange"),
            "respond_text",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.HttpExchange"),
            "respond_bytes",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.HttpExchange"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.HttpResponse"),
            "status",
            DirectType::Scalar(ScalarKind::Int32),
        ),
        (
            named_type("net.HttpResponse"),
            "reason",
            direct_named("str"),
        ),
        (
            named_type("net.HttpResponse"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.WebSocketListener"),
            "accept",
            result_direct(named_type("net.WebSocket"), io_error.clone()),
        ),
        (
            named_type("net.WebSocketListener"),
            "local_addr",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.WebSocketListener"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.WebSocket"),
            "send_text",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.WebSocket"),
            "send_bytes",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.WebSocket"),
            "recv_text",
            result_direct(option_type(Type::named("str")), io_error.clone()),
        ),
        (
            named_type("net.WebSocket"),
            "recv_bytes",
            result_direct(
                option_type(vec_type(Type::named("uint8"))),
                io_error.clone(),
            ),
        ),
        (
            named_type("net.WebSocket"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.UnixListener"),
            "accept",
            result_direct(named_type("net.UnixStream"), io_error.clone()),
        ),
        (
            named_type("net.UnixListener"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.UnixStream"),
            "read_line",
            result_direct(option_type(Type::named("str")), io_error.clone()),
        ),
        (
            named_type("net.UnixStream"),
            "read_exact",
            result_direct(vec_type(Type::named("uint8")), io_error.clone()),
        ),
        (
            named_type("net.UnixStream"),
            "write_all",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.UnixStream"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.TlsListener"),
            "accept",
            result_direct(named_type("net.TlsStream"), io_error.clone()),
        ),
        (
            named_type("net.TlsListener"),
            "local_addr",
            result_direct(Type::named("str"), io_error.clone()),
        ),
        (
            named_type("net.TlsListener"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
        (
            named_type("net.TlsStream"),
            "read_line",
            result_direct(option_type(Type::named("str")), io_error.clone()),
        ),
        (
            named_type("net.TlsStream"),
            "read_exact",
            result_direct(vec_type(Type::named("uint8")), io_error.clone()),
        ),
        (
            named_type("net.TlsStream"),
            "write_all",
            result_direct(Type::Unit, io_error.clone()),
        ),
        (
            named_type("net.TlsStream"),
            "close",
            DirectType::Scalar(ScalarKind::Unit),
        ),
    ] {
        assert_eq!(
            builtin_opaque_member_return_type(&object_ty, field, &classes),
            Some(expected),
            "unexpected direct runtime member type for `{object_ty}.{field}`",
        );
    }

    assert_eq!(
        direct_type(&Type::named("Node"), &classes),
        Some(DirectType::Opaque(Type::named("Node"))),
        "recursive plain classes should fall back to opaque values"
    );
    assert_eq!(
        direct_type(
            &Type::Named("Box".to_string(), vec![Type::named("int32")]),
            &classes
        ),
        Some(DirectType::Opaque(Type::Named(
            "Box".to_string(),
            vec![Type::named("int32")],
        ))),
        "generic classes should stay opaque in direct type inference"
    );
}

#[test]
fn native_codegen_type_helpers_cover_nested_type_params_and_opaque_fallbacks() {
    let classes = HashMap::from([
        (
            "Pair".to_string(),
            crate::mir::MirClass {
                name: "Pair".to_string(),
                type_params: Vec::new(),
                fields: vec![
                    crate::mir::MirClassField {
                        name: "left".to_string(),
                        ty: Type::named("int32"),
                    },
                    crate::mir::MirClassField {
                        name: "right".to_string(),
                        ty: Type::named("bool"),
                    },
                ],
                methods: Vec::new(),
            },
        ),
        (
            "Wrapper".to_string(),
            crate::mir::MirClass {
                name: "Wrapper".to_string(),
                type_params: Vec::new(),
                fields: vec![crate::mir::MirClassField {
                    name: "value".to_string(),
                    ty: Type::named("str"),
                }],
                methods: Vec::new(),
            },
        ),
    ]);

    let mut collected = BTreeSet::new();
    collect_type_params_from_type(
        &Type::Named(
            "dict".to_string(),
            vec![
                Type::TypeParam("K".to_string()),
                Type::Named("list".to_string(), vec![Type::TypeParam("V".to_string())]),
            ],
        ),
        &mut collected,
    );
    assert_eq!(
        collected,
        BTreeSet::from(["K".to_string(), "V".to_string()])
    );

    assert_eq!(
        direct_type(&Type::TypeParam("T".to_string()), &classes),
        Some(DirectType::Opaque(Type::TypeParam("T".to_string())))
    );
    assert_eq!(
        direct_type(&Type::Module("pkg.tools".to_string()), &classes),
        Some(DirectType::Opaque(Type::Module("pkg.tools".to_string())))
    );
    assert_eq!(
        direct_type(&Type::named("float32"), &classes),
        Some(DirectType::Scalar(ScalarKind::Float32))
    );
    assert_eq!(
        direct_type(&Type::named("External"), &classes),
        Some(DirectType::Opaque(Type::named("External")))
    );

    let pair = direct_type(&Type::named("Pair"), &classes).expect("Pair should stay plain");
    assert_eq!(render_direct_type(&pair), "Pair");
    assert_eq!(pair.value_count(), 2);

    assert_eq!(
        direct_type(&Type::named("Wrapper"), &classes),
        Some(DirectType::Opaque(Type::named("Wrapper"))),
        "classes with opaque fields should fall back to opaque direct values"
    );
}

#[test]
fn validate_function_rejects_unreachable_terminators_for_direct_backend() {
    let function = MirFunction {
        name: "main".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: vec![MirParam {
            name: "value".to_string(),
            passing: MirReceiverKind::Value,
            ty: Type::named("int32"),
            default_function: None,
        }],
        local_types: vec![MirLocalType {
            name: "value".to_string(),
            ty: Type::named("int32"),
        }],
        return_type: Type::named("int32"),
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: vec![
                Instruction::Assign {
                    target: "%t0".to_string(),
                    value: Rvalue::FormatString {
                        parts: vec![
                            crate::mir::MirFormatPart::Literal("value=".to_string()),
                            crate::mir::MirFormatPart::Value(Operand::Place("value".to_string())),
                        ],
                    },
                },
                Instruction::Assign {
                    target: "%t1".to_string(),
                    value: Rvalue::VecLiteral {
                        elements: vec![Operand::Int(1)],
                        element_type: Type::named("int32"),
                    },
                },
                Instruction::Assign {
                    target: "%t2".to_string(),
                    value: Rvalue::MapLiteral {
                        entries: vec![MirMapEntry {
                            key: Operand::String("a".to_string()),
                            value: Operand::Int(1),
                        }],
                        key_type: Type::named("str"),
                        value_type: Type::named("int32"),
                    },
                },
            ],
            terminator: Terminator::Unreachable,
        }],
    };

    let error = validate_function(&function, &HashMap::new()).expect_err("unreachable should fail");
    assert!(
        error.contains("does not yet support MIR terminator"),
        "unexpected error: {error}"
    );
}

#[test]
fn native_codegen_constructor_initializes_runtime_function_surface() {
    let module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    let codegen = super::NativeCodegen::new(
        &module,
        "/tmp/direct_constructor.au",
        "def main() -> int32:\n    return 0\n",
    )
    .expect("direct codegen constructor should initialize runtime symbols");

    assert_eq!(codegen.program_path, "/tmp/direct_constructor.au");
    assert!(codegen.program_source.contains("return 0"));
    assert!(codegen.classes.is_empty());
    assert!(codegen.trait_impls.is_empty());
    assert!(codegen.string_data.is_empty());
    assert!(codegen.functions.contains_key("main"));
    assert!(codegen.function_return_types.contains_key("main"));
    assert!(codegen.function_param_types.contains_key("main"));
    assert!(codegen.function_writeback_types.contains_key("main"));
}

#[test]
fn native_codegen_constructor_tracks_receiver_and_writeback_types_for_methods_and_top_level() {
    let source = r#"
class Counter:
    value: int32

    def sync_into(mut self, other: mut Counter, amount: int32):
        self.value += amount
        other.value = self.value

mut left: Counter = Counter(value=1)
mut right: Counter = Counter(value=0)
left.sync_into(other=right, amount=2)
"#;
    let mir = lower_source_to_mir(source).expect("source should lower to MIR");
    let method = mir
        .functions
        .iter()
        .find(|function| function.receiver == Some(MirReceiverKind::BorrowMut))
        .expect("borrow-mut method should lower into a function");
    let top_level = mir
        .top_level
        .as_ref()
        .expect("top-level script should lower into a top-level entry function");

    let codegen = super::NativeCodegen::new(&mir, "/tmp/direct_constructor_writebacks.au", source)
        .expect("direct codegen constructor should initialize runtime symbols");

    let method_params = codegen
        .function_param_types
        .get(&method.name)
        .expect("method param metadata should be registered");
    assert_eq!(method_params.len(), 3);
    assert!(matches!(method_params[0], DirectType::PlainClass(_)));
    assert!(matches!(method_params[1], DirectType::PlainClass(_)));
    assert_eq!(method_params[2], DirectType::Scalar(ScalarKind::Int32));

    let method_writebacks = codegen
        .function_writeback_types
        .get(&method.name)
        .expect("method writeback metadata should be registered");
    assert_eq!(method_writebacks.len(), 2);
    assert!(matches!(method_writebacks[0], DirectType::PlainClass(_)));
    assert!(matches!(method_writebacks[1], DirectType::PlainClass(_)));

    assert!(codegen.functions.contains_key(&top_level.name));
    assert!(codegen.function_thunks.contains_key(&top_level.name));
}

#[test]
fn native_codegen_replace_nested_field_rejects_empty_paths_without_panicking() {
    let error = super::split_field_path_segments(&[])
        .expect_err("empty field paths should surface an internal diagnostic");
    assert!(error.contains("empty field path"));
}

fn cleanup_test_function(local_name: &str, ty: Type, place: &str) -> MirFunction {
    MirFunction {
        name: "main".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: Vec::new(),
        local_types: vec![MirLocalType {
            name: local_name.to_string(),
            ty,
        }],
        return_type: Type::named("int32"),
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: vec![Instruction::PushCleanup {
                place: place.to_string(),
            }],
            terminator: Terminator::Return(Operand::Int(0)),
        }],
    }
}

fn cleanup_test_module(functions: Vec<MirFunction>) -> crate::mir::MirModule {
    crate::mir::MirModule {
        constants: Vec::new(),
        functions,
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    }
}

fn class_field(name: &str, ty: Type) -> crate::mir::MirClassField {
    crate::mir::MirClassField {
        name: name.to_string(),
        ty,
    }
}

fn close_method(function_name: &str) -> crate::mir::MirMethod {
    crate::mir::MirMethod {
        name: "close".to_string(),
        function_name: function_name.to_string(),
        receiver: Some(MirReceiverKind::BorrowMut),
    }
}

fn close_function(class_name: &str) -> MirFunction {
    MirFunction {
        name: format!("{class_name}.close"),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: Some(MirReceiverKind::BorrowMut),
        params: Vec::new(),
        local_types: vec![MirLocalType {
            name: "self".to_string(),
            ty: Type::named(class_name),
        }],
        return_type: Type::Unit,
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: Vec::new(),
            terminator: Terminator::Return(Operand::Unit),
        }],
    }
}

#[test]
fn native_codegen_cleanup_thunks_cover_scalar_plain_opaque_and_metadata_errors() {
    let scalar_function = cleanup_test_function("count", Type::named("int32"), "count");
    let scalar_module = cleanup_test_module(vec![scalar_function.clone()]);
    let mut scalar_codegen =
        super::NativeCodegen::new(&scalar_module, "/tmp/direct_scalar_cleanup.au", "")
            .expect("scalar cleanup test codegen should initialize");
    scalar_codegen
        .define_cleanup_thunk(&scalar_function, "count")
        .expect("scalar cleanup thunks should return unit");

    let mut missing_thunk_codegen =
        super::NativeCodegen::new(&scalar_module, "/tmp/direct_missing_cleanup.au", "")
            .expect("missing cleanup test codegen should initialize");
    missing_thunk_codegen
        .cleanup_thunks
        .remove(&("main".to_string(), "count".to_string()));
    let missing_thunk_error = missing_thunk_codegen
        .define_cleanup_thunk(&scalar_function, "count")
        .expect_err("missing cleanup thunk metadata should fail");
    assert!(missing_thunk_error.contains("could not find cleanup thunk for `count` in `main`"));

    let unknown_field_id = *missing_thunk_codegen
        .cleanup_thunks
        .entry(("main".to_string(), "count.missing".to_string()))
        .or_insert_with(|| {
            scalar_codegen.cleanup_thunks[&("main".to_string(), "count".to_string())]
        });
    missing_thunk_codegen.cleanup_thunks.insert(
        ("main".to_string(), "count.missing".to_string()),
        unknown_field_id,
    );
    let unknown_field_error = missing_thunk_codegen
        .define_cleanup_thunk(&scalar_function, "count.missing")
        .expect_err("unknown cleanup fields should fail before code emission");
    assert!(unknown_field_error.contains("does not know cleanup field `missing`"));

    let plain_function = cleanup_test_function("resource", Type::named("Plain"), "resource");
    let plain_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![plain_function.clone()],
        classes: vec![crate::mir::MirClass {
            name: "Plain".to_string(),
            type_params: Vec::new(),
            fields: vec![class_field("value", Type::named("int32"))],
            methods: Vec::new(),
        }],
        trait_impls: Vec::new(),
        top_level: None,
    };
    let mut plain_codegen =
        super::NativeCodegen::new(&plain_module, "/tmp/direct_plain_cleanup.au", "")
            .expect("plain cleanup test codegen should initialize");
    plain_codegen
        .define_cleanup_thunk(&plain_function, "resource")
        .expect("plain-class cleanup without close should return unit");

    let opaque_function = cleanup_test_function("text", Type::named("str"), "text");
    let opaque_module = cleanup_test_module(vec![opaque_function.clone()]);
    let mut opaque_codegen =
        super::NativeCodegen::new(&opaque_module, "/tmp/direct_opaque_cleanup.au", "")
            .expect("opaque cleanup test codegen should initialize");
    opaque_codegen
        .define_cleanup_thunk(&opaque_function, "text")
        .expect("opaque cleanup without custom close should call runtime close");
}

#[test]
fn native_codegen_cleanup_thunks_cover_class_close_success_and_missing_targets() {
    let plain_close_function =
        cleanup_test_function("resource", Type::named("Resource"), "resource");
    let resource_close = close_function("Resource");
    let plain_close_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![plain_close_function.clone(), resource_close],
        classes: vec![crate::mir::MirClass {
            name: "Resource".to_string(),
            type_params: Vec::new(),
            fields: vec![class_field("closed", Type::named("bool"))],
            methods: vec![close_method("Resource.close")],
        }],
        trait_impls: Vec::new(),
        top_level: None,
    };
    let mut plain_close_codegen = super::NativeCodegen::new(
        &plain_close_module,
        "/tmp/direct_plain_close_cleanup.au",
        "",
    )
    .expect("plain close cleanup codegen should initialize");
    plain_close_codegen
        .define_cleanup_thunk(&plain_close_function, "resource")
        .expect("plain-class cleanup should call a close method when available");

    let mut missing_target_codegen = super::NativeCodegen::new(
        &plain_close_module,
        "/tmp/direct_missing_close_cleanup.au",
        "",
    )
    .expect("missing close target codegen should initialize");
    missing_target_codegen.functions.remove("Resource.close");
    let missing_target_error = missing_target_codegen
        .define_cleanup_thunk(&plain_close_function, "resource")
        .expect_err("missing plain-class close targets should be reported");
    assert!(missing_target_error.contains("could not find cleanup close method `Resource.close`"));

    let opaque_close_function = cleanup_test_function("managed", Type::named("Managed"), "managed");
    let managed_close = close_function("Managed");
    let opaque_close_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![opaque_close_function.clone(), managed_close],
        classes: vec![crate::mir::MirClass {
            name: "Managed".to_string(),
            type_params: Vec::new(),
            fields: vec![class_field("handle", Type::named("str"))],
            methods: vec![close_method("Managed.close")],
        }],
        trait_impls: Vec::new(),
        top_level: None,
    };
    let mut opaque_close_codegen = super::NativeCodegen::new(
        &opaque_close_module,
        "/tmp/direct_opaque_close_cleanup.au",
        "",
    )
    .expect("opaque close cleanup codegen should initialize");
    opaque_close_codegen
        .define_cleanup_thunk(&opaque_close_function, "managed")
        .expect("opaque cleanup should call a custom close method when available");

    let mut missing_opaque_target_codegen = super::NativeCodegen::new(
        &opaque_close_module,
        "/tmp/direct_missing_opaque_close.au",
        "",
    )
    .expect("missing opaque close target codegen should initialize");
    missing_opaque_target_codegen
        .functions
        .remove("Managed.close");
    let missing_opaque_target_error = missing_opaque_target_codegen
        .define_cleanup_thunk(&opaque_close_function, "managed")
        .expect_err("missing opaque close targets should be reported");
    assert!(
        missing_opaque_target_error.contains("could not find cleanup close method `Managed.close`")
    );
}

#[test]
fn native_codegen_thunks_cover_float_bool_plain_class_params_and_unit_main_wrapper() {
    let thunk_source = r#"
class Pair:
    left: int32
    right: bool

def helper(value: float64, flag: bool, pair: Pair) -> float64:
    if flag:
        return value
    return 0.0

def main() -> int32:
    return 0
"#;
    let thunk_mir = lower_source_to_mir(thunk_source).expect("source should lower to MIR");
    let helper = thunk_mir
        .functions
        .iter()
        .find(|function| function.name == "helper")
        .expect("helper function should be lowered");
    let mut thunk_codegen = super::NativeCodegen::new(
        &thunk_mir,
        "/tmp/direct_thunk_float_bool_pair.au",
        thunk_source,
    )
    .expect("codegen should initialize");
    thunk_codegen
        .define_function_thunk(helper)
        .expect("thunk generation should support float, bool, and plain-class parameters");

    let wrapper_source = "def main():\n    pass\n";
    let wrapper_mir = lower_source_to_mir(wrapper_source).expect("unit main should lower");
    let mut wrapper_codegen = super::NativeCodegen::new(
        &wrapper_mir,
        "/tmp/direct_unit_main_wrapper.au",
        wrapper_source,
    )
    .expect("codegen should initialize");
    wrapper_codegen
        .define_main_wrapper()
        .expect("main wrapper should support unit-return entrypoints");
}

#[test]
fn direct_assertions_reference_the_dedicated_failure_helper() {
    let source = r#"def main() -> int32:
    assert false, "direct assertion"
    return 0
"#;
    let mir = lower_source_to_mir(source).expect("assertion source should lower");
    let object = emit_host_object_with_metadata(&mir, "/tmp/direct_assert.au", source)
        .expect("assertion source should compile directly");
    let referenced = object_referenced_symbols(&object);
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_assert_fail")),
        "assert failures must use the diagnostic-preserving runtime helper: {referenced:?}"
    );
}

#[test]
fn direct_introspected_assertions_reference_the_detailed_failure_helper() {
    let source = r#"def main() -> int32:
    left = 41
    right = 42
    assert left == right, "direct assertion"
    return 0
"#;
    let mir = lower_source_to_mir(source).expect("assertion source should lower");
    let object = emit_host_object_with_metadata(&mir, "/tmp/direct_detailed_assert.au", source)
        .expect("introspected assertion source should compile directly");
    let referenced = object_referenced_symbols(&object);
    assert!(
        referenced
            .iter()
            .any(|symbol| symbol.contains("aura_direct_assert_fail_detailed")),
        "introspected assertion failures must use the detailed runtime helper: {referenced:?}"
    );
}

#[test]
fn direct_validation_accepts_assert_fail_operands_and_rejects_unknown_places() {
    let make_module = |message, captures| crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::AssertFail {
                    message,
                    captures,
                    span: Span::new(2, 5),
                },
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    emit_host_object(&make_module(
        Some(Operand::String("known".to_string())),
        Vec::new(),
    ))
    .expect("literal assertion messages should validate");
    let error = emit_host_object(&make_module(
        Some(Operand::Place("missing".to_string())),
        Vec::new(),
    ))
    .expect_err("unknown assertion message places should be rejected");
    assert!(
        error.contains("does not know local `missing`"),
        "unexpected validation error: {error}"
    );

    let captures = vec![
        AssertionCapture {
            label: "left".to_string(),
            ty: Type::named("int64"),
            value: Operand::String("41".to_string()),
        },
        AssertionCapture {
            label: "right".to_string(),
            ty: Type::named("int64"),
            value: Operand::Place("missing_capture".to_string()),
        },
    ];
    let error = emit_host_object(&make_module(None, captures))
        .expect_err("unknown assertion capture places should be rejected");
    assert!(
        error.contains("does not know local `missing_capture`"),
        "unexpected capture validation error: {error}"
    );

    let error = emit_host_object(&make_module(
        None,
        vec![AssertionCapture {
            label: "only".to_string(),
            ty: Type::named("int64"),
            value: Operand::String("1".to_string()),
        }],
    ))
    .expect_err("assertion captures must be absent or form a pair");
    assert!(
        error.contains("exactly two assertion captures"),
        "unexpected capture cardinality error: {error}"
    );

    let error = emit_host_object(&make_module(
        None,
        vec![
            AssertionCapture {
                label: "left".to_string(),
                ty: Type::named("int64"),
                value: Operand::Int(1),
            },
            AssertionCapture {
                label: "right".to_string(),
                ty: Type::named("int64"),
                value: Operand::String("2".to_string()),
            },
        ],
    ))
    .expect_err("assertion capture values must already be rendered strings");
    assert!(
        error.contains("rendered assertion capture to be `str`, found `int64`"),
        "unexpected capture type error: {error}"
    );
}

#[test]
fn adr0038_direct_return_projection_selection_and_tuple_errors_are_codegen_checked() {
    let pair_type = Type::named("Pair");
    let returned_module = crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![
            MirFunction {
                name: "dynamic_return".to_string(),
                module_name: "<test>".to_string(),
                source_path: None,
                span: Span::new(1, 1),
                receiver: None,
                params: vec![MirParam {
                    name: "origin".to_string(),
                    passing: MirReceiverKind::BorrowMut,
                    ty: pair_type.clone(),
                    default_function: None,
                }],
                local_types: vec![MirLocalType {
                    name: "selected".to_string(),
                    ty: Type::named("int64"),
                }],
                return_type: Type::named("int32"),
                entry: "entry".to_string(),
                blocks: vec![BasicBlock {
                    label: "entry".to_string(),
                    instructions: vec![
                        Instruction::BeginReturnedLoan {
                            loan: "selected".to_string(),
                            origin: "origin".to_string(),
                            projections: vec!["left".to_string(), "left".to_string()],
                            mutable: true,
                        },
                        Instruction::ReturnLoan {
                            loan: "selected".to_string(),
                            origin: "selected".to_string(),
                        },
                    ],
                    terminator: Terminator::Return(Operand::Int(0)),
                }],
            },
            MirFunction {
                name: "main".to_string(),
                module_name: "<test>".to_string(),
                source_path: None,
                span: Span::new(1, 1),
                receiver: None,
                params: Vec::new(),
                local_types: Vec::new(),
                return_type: Type::named("int32"),
                entry: "entry".to_string(),
                blocks: vec![BasicBlock {
                    label: "entry".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(0)),
                }],
            },
        ],
        classes: vec![crate::mir::MirClass {
            name: "Pair".to_string(),
            type_params: Vec::new(),
            fields: vec![
                crate::mir::MirClassField {
                    name: "left".to_string(),
                    ty: Type::named("int64"),
                },
                crate::mir::MirClassField {
                    name: "right".to_string(),
                    ty: Type::named("int64"),
                },
            ],
            methods: Vec::new(),
        }],
        trait_impls: Vec::new(),
        top_level: None,
    };
    emit_host_object(&returned_module)
        .expect("dynamic returned-view projection selection should compile");

    let malformed_tuple_module = |projection: &str| crate::mir::MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: format!("tuple_projection_{projection}"),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: vec![MirParam {
                name: "origin".to_string(),
                passing: MirReceiverKind::Borrow,
                ty: Type::Tuple(vec![Type::named("int64"), Type::named("int64")]),
                default_function: None,
            }],
            local_types: vec![MirLocalType {
                name: "target".to_string(),
                ty: Type::named("int64"),
            }],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::BeginLoan {
                        loan: "selected".to_string(),
                        source: format!("origin.{projection}"),
                        mutable: false,
                    },
                    Instruction::ReadLoan {
                        target: "target".to_string(),
                        loan: "selected".to_string(),
                    },
                ],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    let invalid = emit_host_object(&malformed_tuple_module("invalid"))
        .expect_err("tuple view projections must be fixed positions");
    assert!(invalid.contains("is not a fixed position"), "{invalid}");
    let out_of_bounds = emit_host_object(&malformed_tuple_module("4"))
        .expect_err("tuple view projections must remain in bounds");
    assert!(
        out_of_bounds.contains("has no element at index 4"),
        "{out_of_bounds}"
    );
}
