use super::*;
use crate::ast::{
    BreakStmt, ContinueStmt, FieldDecl, FormatPart, MapEntryExpr, PassStmt, ReturnStmt,
};
use crate::diag::Span;
use crate::integer::IntegerValue;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[test]
fn s1_frontend_module_constant_failures_distinguish_collisions_order_unknowns_and_types() {
    let collision = crate::check_source(
        "value: int64 = 1\n\ndef value() -> int64:\n    return 2\n\ndef main():\n    pass\n",
    )
    .expect_err("a constant cannot collide with a function");
    assert_eq!(collision.code, "AU2999");
    assert!(collision
        .message
        .contains("module constant `value` collides with an existing function"));
    assert_eq!(collision.secondary_spans.len(), 1);

    let order = crate::check_source(
        "first: int64 = second\nsecond: int64 = 2\n\ndef main():\n    print(first)\n",
    )
    .expect_err("a constant cannot read a later constant");
    assert_eq!(order.code, "AU2001");
    assert_eq!(
        order.message,
        "module constant `second` is used before initialization"
    );
    assert_eq!(order.secondary_spans.len(), 1);

    let unknown = crate::check_source("value: int64 = missing\n\ndef main():\n    print(value)\n")
        .expect_err("an unrelated unknown name must retain the ordinary diagnostic");
    assert_eq!(unknown.code, "AU2001");
    assert_eq!(unknown.message, "unknown name `missing`");

    let mismatch =
        crate::check_source("value: int64 = \"text\"\n\ndef main():\n    print(value)\n")
            .expect_err("a constant annotation must constrain its initializer");
    assert_eq!(mismatch.code, "AU2002");
    assert_eq!(
        mismatch.message,
        "initializer for module constant `value` has type `str`, expected `int64`"
    );
}

#[test]
fn s1_frontend_collection_capacity_calls_report_missing_types_arity_members_and_values() {
    for (source, code, expected) in [
        (
            "def main():\n    values = list.with_capacity(4)\n",
            "AU2005",
            "`list.with_capacity` requires explicit type arguments",
        ),
        (
            "def main():\n    values = list[int64, str].with_capacity(4)\n",
            "AU2002",
            "`list` expects exactly 1 type argument, found 2",
        ),
        (
            "def main():\n    values = dict[str].with_capacity(4)\n",
            "AU2002",
            "`dict` expects exactly 2 type arguments, found 1",
        ),
        (
            "def main():\n    values = set[str].missing(4)\n",
            "AU2001",
            "type `set` has no associated function `missing`",
        ),
        (
            "def main():\n    values = set[str].with_capacity(\"large\")\n",
            "AU2002",
            "`set.with_capacity` expects `int64`, found `str`",
        ),
    ] {
        let error = crate::check_source(source).expect_err("invalid capacity call must fail");
        assert_eq!(error.code, code, "{source}");
        assert_eq!(error.message, expected, "{source}");
    }
}

#[test]
fn s1_frontend_match_guard_and_sort_reverse_errors_keep_specific_diagnostics() {
    let guard = crate::check_source(
        "def main():\n    match 1:\n        case _ if missing:\n            pass\n",
    )
    .expect_err("an invalid guard expression must retain its own diagnostic");
    assert_eq!(guard.code, "AU2001");
    assert_eq!(guard.message, "unknown name `missing`");

    let reverse =
        crate::check_source("def main():\n    mut values = [2, 1]\n    values.sort(reverse=1)\n")
            .expect_err("sort reverse must be exactly bool");
    assert_eq!(reverse.code, "AU2002");
    assert_eq!(
        reverse.message,
        "`sort` expects `bool` for `reverse`, found `int64`"
    );
}

#[test]
fn s1_sema_collection_algorithms_pin_types_equality_and_lambda_key_orderability() {
    crate::check_source(
        "def main():\n    values: list[int64] = list[int64].with_capacity(4)\n    names: set[str] = set[str].with_capacity(2)\n    counts: dict[str, int64] = dict[str, int64].with_capacity(8)\n",
    )
    .expect("capacity constructors must retain their specialized collection types");

    let equality = crate::check_source(
        "def main():\n    left = [Array[int32].zeros(shape=[1])]\n    right = [Array[int32].zeros(shape=[1])]\n    print(left == right)\n",
    )
    .expect_err("collection equality must reject a recursively contained Array");
    assert_eq!(equality.code, "AU2003");
    assert_eq!(
        equality.message,
        "cannot compare `list[Array[int32]]` because it contains `Array[int32]`, whose equality is unavailable"
    );

    let orderability = crate::check_source(
        "def main():\n    mut values = [2, 1]\n    values.sort(key=lambda value: [value])\n",
    )
    .expect_err("a list-valued lambda key is not naturally ordered");
    assert_eq!(orderability.code, "AU2002");
    assert_eq!(
        orderability.message,
        "`list.sort` cannot order key type `list[int64]`"
    );

    for source in [
        "def main():\n    values = list[int64].with_capacity(missing)\n",
        "def main():\n    mut values = [2, 1]\n    values.sort(reverse=missing)\n",
    ] {
        let propagated = crate::check_source(source)
            .expect_err("collection argument inference must preserve its source diagnostic");
        assert_eq!(propagated.code, "AU2001", "{source}");
        assert_eq!(propagated.message, "unknown name `missing`", "{source}");
    }
}

#[test]
fn s1_sema_typed_power_and_shift_diagnostics_preserve_exact_operand_contracts() {
    let negative = crate::check_source("def main():\n    value = 2 ** -1\n")
        .expect_err("integer power must reject a negative exponent");
    assert_eq!(negative.code, "AU2003");
    assert_eq!(
        negative.message,
        "integer power does not accept a negative exponent"
    );

    let non_numeric = crate::check_source(
        "def main():\n    left: str = \"a\"\n    right: str = \"b\"\n    value = left ** right\n",
    )
    .expect_err("power requires numeric operands");
    assert_eq!(non_numeric.code, "AU2003");
    assert_eq!(
        non_numeric.message,
        "power requires numeric operands, found `str` and `str`"
    );

    for operator in ["**", "<<", ">>", "&", "|", "^"] {
        let source = format!(
            "def main():\n    left: int8 = 2\n    right: int16 = 1\n    value = left {operator} right\n"
        );
        let mismatch = crate::check_source(&source)
            .expect_err("typed integer operators require an exact shared width");
        assert_eq!(mismatch.code, "AU2002", "{operator}");
        assert_eq!(
            mismatch.message,
            "binary operator operands must match exactly, found `int8` and `int16`",
            "{operator}"
        );
    }

    crate::check_source(
        "def main():\n    left: int8 = 2\n    right: int8 = 1\n    power: int8 = left ** right\n    shifted: int8 = left << right\n",
    )
    .expect("same-width typed integer power and shifts must retain that width");
}

#[test]
fn s1_sema_fourth_collection_methods_pin_element_key_collection_and_capacity_types() {
    for (source, code, expected) in [
        (
            "def main():\n    mut values = [1]\n    values.append(\"bad\")\n",
            "AU2999",
            "`append` expects `int64`, found `str`",
        ),
        (
            "def main():\n    mut values = [1]\n    values.set(0, \"bad\")\n",
            "AU2999",
            "`set` expects `int64`, found `str`",
        ),
        (
            "def main():\n    mut values = [1]\n    values.remove(\"bad\")\n",
            "AU2002",
            "`remove` expects `int64`, found `str`",
        ),
        (
            "def main():\n    values = [1]\n    print(values.contains(\"bad\"))\n",
            "AU2999",
            "`contains` expects `int64`, found `str`",
        ),
        (
            "def main():\n    mut values = [1]\n    other = [\"bad\"]\n    values.extend(other)\n",
            "AU2999",
            "`extend` expects `list[int64]`, found `list[str]`",
        ),
        (
            "def main():\n    mut values = [1]\n    values.insert(0, \"bad\")\n",
            "AU2999",
            "`insert` expects `int64`, found `str`",
        ),
        (
            "def main():\n    mut values = [1]\n    values.reserve(\"bad\")\n",
            "AU2002",
            "`reserve` expects `int64`, found `str`",
        ),
        (
            "def main():\n    values = {\"one\": 1}\n    print(values.get(1))\n",
            "AU2999",
            "`get` expects `str`, found `int64`",
        ),
        (
            "def main():\n    mut values = {\"one\": 1}\n    values.remove(1)\n",
            "AU2999",
            "`remove` expects `str`, found `int64`",
        ),
        (
            "def main():\n    mut values = {\"one\": 1}\n    other = {1: 2}\n    values.update(other)\n",
            "AU2999",
            "`update` expects `dict[str, int64]`, found `dict[int64, int64]`",
        ),
        (
            "def main():\n    mut values = {\"one\": 1}\n    values.reserve(\"bad\")\n",
            "AU2002",
            "`reserve` expects `int64`, found `str`",
        ),
        (
            "def main():\n    mut values = {1}\n    values.add(\"bad\")\n",
            "AU2999",
            "`add` expects `int64`, found `str`",
        ),
        (
            "def main():\n    mut values = {1}\n    values.reserve(\"bad\")\n",
            "AU2002",
            "`reserve` expects `int64`, found `str`",
        ),
    ] {
        let error = crate::check_source(source)
            .expect_err("a collection method must reject the wrong static argument type");
        assert_eq!(error.code, code, "{source}: {error:?}");
        assert_eq!(error.message, expected, "{source}");
    }
}

#[test]
fn s1_sema_fourth_duplicate_ffi_items_report_the_prior_declaration_kind() {
    for (source, expected) in [
        (
            "extern \"C\" opaque class Handle\nextern \"C\" opaque class Handle\n",
            "duplicate item `Handle` (previously declared as opaque class at 1:1)",
        ),
        (
            "extern \"C\" def read(value: int32) -> int32\nextern \"C\" def read(value: int32) -> int32\n",
            "duplicate item `read` (previously declared as extern function at 1:1)",
        ),
    ] {
        let error = check_ffi_source_for_test(source)
            .expect_err("duplicate extern declarations must be rejected");
        assert_eq!(error.code, "AU2999", "{source}: {error:?}");
        assert_eq!(error.message, expected, "{source}");
        assert_eq!(error.secondary_spans.len(), 0, "{source}");
    }
}

#[test]
fn s1_sema_fourth_expected_builtin_enum_arity_and_len_receiver_stay_specific() {
    let arity = crate::check_source(
        "def make() -> Option[int32]:\n    return Option.Some()\n\ndef main():\n    pass\n",
    )
    .expect_err("the expected Option type must not hide a missing Some payload");
    assert_eq!(arity.code, "AU2004");
    assert_eq!(
        arity.message,
        "variant `Some` of enum `Option` expects 1 payload argument, found 0"
    );

    let len = crate::check_source("def main():\n    print(len(1))\n")
        .expect_err("len must reject scalar values through its public builtin contract");
    assert_eq!(len.code, "AU2002");
    assert_eq!(
        len.message,
        "`len(...)` expects a value with a `len()` member, found `int64`"
    );

    let field_default = crate::check_source(
        "def initial() -> int32:\n    return 1\n\nclass Box:\n    value: int32 = initial()\n\ndef main():\n    pass\n",
    )
    .expect_err("class field defaults cannot call module functions");
    assert_eq!(field_default.code, "AU2999");
    assert_eq!(field_default.message, "unsupported call target");
}

#[test]
fn s1_sema_fourth_match_statements_and_expressions_pin_shape_diagnostics() {
    for (source, expected) in [
        (
            "enum Pair:\n    Both(int32, int32)\n\ndef main():\n    value = Pair.Both(1, 2)\n    match value:\n        case Pair.Both(left):\n            print(left)\n        case _:\n            pass\n",
            "variant `Pair.Both` expects 2 pattern payloads, found 1",
        ),
        (
            "enum Pair:\n    Both(int32, int32)\n\ndef main() -> int32:\n    value = Pair.Both(1, 2)\n    return match value:\n        case Pair.Both(left): left\n        case _: 0\n",
            "variant `Pair.Both` expects 2 pattern payloads, found 1",
        ),
        (
            "def main():\n    match [1]:\n        case _:\n            pass\n",
            "`match` currently requires a tuple, enum, bool, integer, float, or str scrutinee, found `list[int64]`",
        ),
        (
            "def main() -> int32:\n    return match [1]:\n        case _: 0\n",
            "`match` currently requires a tuple, enum, bool, integer, float, or str scrutinee, found `list[int64]`",
        ),
    ] {
        let error = crate::check_source(source)
            .expect_err("unsupported match shapes must report a source-level diagnostic");
        assert_eq!(error.code, "AU2999", "{source}: {error:?}");
        assert_eq!(error.message, expected, "{source}");
    }
}

#[test]
fn s1_sema_fifth_entry_module_rules_and_constant_plan_are_observable() {
    let mutable_module = crate::check_source("mut state = 1\n\ndef main():\n    pass\n")
        .expect_err("module-level mutable state must be rejected before entrypoint checking");
    assert_eq!(mutable_module.code, "AU3003");
    assert_eq!(
        mutable_module.message,
        "module bindings are immutable; `mut` module state is not supported"
    );
    assert_eq!(
        mutable_module.help,
        vec!["put mutable state in a local value owned by `main` or another explicit owner"]
    );

    let return_type = crate::check_source("def main() -> str:\n    return \"bad\"\n")
        .expect_err("main must retain its exact runtime return contract");
    assert_eq!(return_type.code, "AU2999");
    assert_eq!(
        return_type.message,
        "`main` must return `int32` or `None` in the bootstrap runtime"
    );

    let program = crate::check_source(
        "first: int64 = 1\nsecond: int64 = first + 1\n\ndef main():\n    print(second)\n",
    )
    .expect("valid module constants must produce a deterministic source-order plan");
    let names = program
        .constant_init_plan
        .iter()
        .map(|constant| constant.decl.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["first", "second"]);
}

#[test]
fn module_constant_diagnostic_names_the_later_top_level_script_local() {
    let error = crate::check_source(
        "class C:\n    v: int64\n\n    def take(own self) -> int64:\n        return self.v\n\nmut c = C(v=1)\nx = c.take()\nprint(x)\n",
    )
    .expect_err("a module constant cannot read an entry-script local");

    assert_eq!(error.code, "AU2001");
    assert_eq!(
        error.message,
        "module constant `x` cannot read top-level script local `c`"
    );
    assert_eq!(error.span, Some(crate::diag::Span::new(8, 5)));
    assert_eq!(
        error.help,
        vec![
            "declare `x` with `mut` to make it a top-level script local, or move this work into `main`"
        ]
    );
    assert_eq!(error.secondary_spans.len(), 1);
    assert_eq!(error.secondary_spans[0].span, crate::diag::Span::new(7, 5));
    assert_eq!(
        error.secondary_spans[0].label,
        "`c` is initialized when top-level entry statements run"
    );
}

#[test]
fn s1_sema_fifth_contextual_literals_lambdas_and_variants_keep_exact_errors() {
    let float =
        crate::check_source("def main():\n    value: float32 = -16777217\n    print(value)\n")
            .expect_err("an inexact negative integer must not silently round in float32 context");
    assert_eq!(float.code, "AU2002");
    assert_eq!(
        float.message,
        "integer literal `-16777217` cannot be represented exactly as `float32`; write an explicit float spelling such as `-16777217.0` or use `.to_float()` when rounding is intended"
    );

    let lambda = crate::check_source(
        "def main():\n    callback: def(int64) -> str = lambda value: value\n    print(callback)\n",
    )
    .expect_err("a lambda body must match its contextual callable return type");
    assert_eq!(lambda.code, "AU2002");
    assert_eq!(
        lambda.message,
        "lambda body has type `int64`, expected `str`"
    );

    let variant = crate::check_source(
        "def make() -> Option[int32]:\n    return Option.Some(\"bad\")\n\ndef main():\n    pass\n",
    )
    .expect_err("an expected builtin enum type must constrain its payload");
    assert_eq!(variant.code, "AU2999");
    assert_eq!(
        variant.message,
        "variant `Some` of enum `Option` expects `int32`, found `str`"
    );
}

#[test]
fn s1_sema_fifth_member_values_methods_and_missing_fields_stay_specific() {
    let associated = crate::check_source(
        "class Box:\n    value: int32\n\n    def make(value: int32) -> Box:\n        return Box(value=value)\n\ndef main():\n    callback = Box.make\n    print(callback)\n",
    )
    .expect_err("associated methods are callable only through direct syntax");
    assert_eq!(associated.code, "AU2005");
    assert_eq!(
        associated.message,
        "associated method values are not supported in this language version; call `Box.make(...)` directly or wrap it in a named function"
    );

    let integer = crate::check_source(
        "def main():\n    left: int8 = 1\n    right: int16 = 2\n    value = left.wrapping_add(right)\n    print(value)\n",
    )
    .expect_err("fixed-width arithmetic methods require one exact integer type");
    assert_eq!(integer.code, "AU2002");
    assert_eq!(
        integer.message,
        "`wrapping_add` expects `int8`, found `int16`"
    );

    for (source, code, expected) in [
        (
            "def main():\n    value = 1\n    print(value.missing)\n",
            "AU2002",
            "type `int64` has no field `missing`",
        ),
        (
            "class Box:\n    value: int32\n\ndef main():\n    box = Box(value=1)\n    print(box.missing)\n",
            "AU2999",
            "class `Box` has no field `missing`",
        ),
    ] {
        let error = crate::check_source(source).expect_err("unknown fields must name their owner");
        assert_eq!(error.code, code, "{source}: {error:?}");
        assert_eq!(error.message, expected, "{source}");
    }
}

#[test]
fn s1_sema_fifth_custom_compound_operator_retains_the_target_access() {
    let error = crate::check_source(
        r#"
trait Add[Rhs, Out]:
    def add(own self, rhs: own Rhs) -> Out

class Box:
    value: int32

impl Add[int32, Box] for Box:
    def add(own self, rhs: own int32) -> Box:
        return Box(value=self.value + rhs)

def main():
    mut box = Box(value=1)
    box += box.value
"#,
    )
    .expect_err("a consuming compound operator target must conflict with a projected RHS read");
    assert_eq!(error.code, "AU3002");
    assert_eq!(
        error.message,
        "cannot borrow `box.value` while `box` remains reserved for consumption by the compound assignment target"
    );
    assert_eq!(error.secondary_spans.len(), 1);
    assert_eq!(
        error.secondary_spans[0].label,
        "consumption by the compound assignment target begins here"
    );
    assert_eq!(
        error.help,
        vec![
            "call `.clone()` before the expression when an independent value is intended, or perform the read in a separate statement first"
        ]
    );
}

#[test]
fn s1_sema_sixth_bytes_conversion_pins_shared_input_and_result_type() {
    crate::check_source(
        "import bytes\n\ndef main():\n    payload: list[uint8] = [65]\n    decoded: Result[str, bytes.Error] = str.from_bytes(payload)\n    print(payload.len())\n    print(decoded)\n",
    )
    .expect("str.from_bytes must return its typed result without consuming the byte list");

    let error = crate::check_source(
        "import bytes\n\ndef main():\n    payload: list[int64] = [65]\n    decoded = str.from_bytes(payload)\n    print(decoded)\n",
    )
    .expect_err("str.from_bytes must require an exact byte-list input");
    assert_eq!(error.code, "AU2999");
    assert_eq!(
        error.message,
        "`str.from_bytes` expects `list[uint8]`, found `list[int64]`"
    );
}

#[test]
fn s1_sema_sixth_or_pattern_subsumption_remains_observable() {
    let prior_or = crate::check_source(
        "def main():\n    match 3:\n        case 1 | 2:\n            pass\n        case 1:\n            pass\n        case _:\n            pass\n",
    )
    .expect_err("a prior or-pattern must make its repeated alternative unreachable");
    assert_eq!(prior_or.code, "AU2999");
    assert_eq!(prior_or.message, "unreachable match arm");

    let current_or = crate::check_source(
        "enum Choice:\n    Value(int64)\n    Empty\n\ndef main():\n    value = Choice.Value(1)\n    match value:\n        case Value(_):\n            pass\n        case Value(1) | Value(2):\n            pass\n        case Empty:\n            pass\n",
    )
    .expect_err("one prior variant pattern must subsume every current alternative");
    assert_eq!(current_or.code, "AU2999");
    assert_eq!(current_or.message, "unreachable match arm");

    let bindings = crate::check_source(
        "enum Choice:\n    Left(int64)\n    Right(int64)\n\ndef main():\n    value = Choice.Left(1)\n    match value:\n        case Left(left) | Right(right):\n            print(left)\n",
    )
    .expect_err("or-pattern alternatives must expose one identical binding set");
    assert_eq!(bindings.code, "AU2999");
    assert_eq!(
        bindings.message,
        "every alternative in an or-pattern must bind the same names with identical types and capabilities"
    );
}

#[test]
fn s1_sema_sixth_shared_collection_field_move_offers_the_exact_copy_fix() {
    let error = crate::check_source(
        "class Holder:\n    values: list[int64]\n\ndef take(holder: Holder) -> list[int64]:\n    return holder.values\n\ndef main():\n    pass\n",
    )
    .expect_err("a collection field cannot move through a shared parameter");
    assert_eq!(error.code, "AU3002");
    assert_eq!(
        error.message,
        "cannot move non-copy field `values` out of borrowed value `holder`"
    );
    assert_eq!(
        error.help,
        ["take `holder` as `own Holder` when the field should be moved, or call `.copy()` on the field to return an independent value"]
    );
    assert_eq!(error.edits.len(), 1);
    assert_eq!(error.edits[0].replacement, ".copy()");
}

#[test]
fn s1_sema_final_nested_or_patterns_keep_exact_errors() {
    let nested_or = crate::check_source(
        "enum Choice:\n    Value((bool, bool))\n    Empty\n\ndef main():\n    value = Choice.Value((true, false))\n    match value:\n        case Value((true, _)):\n            pass\n        case Value((true, true) | (true, false)):\n            pass\n        case Empty:\n            pass\n        case _:\n            pass\n",
    )
    .expect_err("a prior tuple payload must subsume every nested or-pattern alternative");
    assert_eq!(nested_or.code, "AU2999");
    assert_eq!(nested_or.message, "unreachable match arm");

    let tuple_union = crate::check_source(
        "def main():\n    value: (int64, bool) = (1, true)\n    match value:\n        case (1, true):\n            pass\n        case (1, false):\n            pass\n        case (1, _):\n            pass\n        case _:\n            pass\n",
    )
    .expect_err("two prior tuple rows must subsume their literal-prefix wildcard");
    assert_eq!(tuple_union.code, "AU2999");
    assert_eq!(tuple_union.message, "unreachable match arm");
}

#[test]
fn s1_sema_final_match_expression_or_patterns_preserve_result_types() {
    crate::check_source(
        "def classify(value: int64) -> str:\n    return match value:\n        case 1 | 2: \"small\"\n        case _: \"other\"\n\ndef main():\n    print(classify(1))\n",
    )
    .expect("match-expression or-patterns must preserve the annotated result type");

    let class_pattern = crate::check_source(
        "class Point:\n    x: int64\n\ndef describe(point: Point) -> str:\n    return match point:\n        case Point(value): \"point\"\n        case _: \"other\"\n\ndef main():\n    print(describe(Point(1)))\n",
    )
    .expect_err("class values cannot use enum-shaped match patterns");
    assert_eq!(class_pattern.code, "AU2999");
    assert_eq!(
        class_pattern.message,
        "class patterns are not supported; match an explicit enum/tag representation or use a wildcard and ordinary code"
    );
}

#[test]
fn s1_sema_final_task_specializations_preserve_matching_type_arguments() {
    crate::check_source(
        "class Factory[T]:\n    def make[T]() -> None:\n        pass\n\ndef main():\n    with group = TaskGroup():\n        group.start(Factory[int64].make[int64])\n",
    )
    .expect("matching class and associated-method arguments must remain a valid task target");
}

#[test]
fn s1_sema_type_pattern_unification_rejects_callable_kind_and_capture_mismatches() {
    let function = Type::Function {
        params: vec![FunctionParamContract {
            name: String::new(),
            ty: Type::named("int64"),
            passing: ReceiverKind::Borrow,
            has_default: false,
            default_erased: false,
        }],
        return_type: Box::new(Type::named("int64")),
    };
    let not_callable = unify_type_pattern(&function, &Type::named("int64"), &mut HashMap::new())
        .expect_err("a function pattern cannot unify with a scalar");
    assert_eq!(
        not_callable.message,
        "expected `def(int64) -> int64`, found `int64`"
    );

    let closure = Type::Closure {
        params: Box::new(Vec::new()),
        return_type: Box::new(Type::named("int64")),
        captures: Box::new(vec![ClosureCapture {
            name: "value".to_string(),
            ty: Type::named("int64"),
            mode: ClosureCaptureMode::SharedView,
            span: Span::new(1, 1),
        }]),
        call_kind: ClosureCallKind::Repeatable,
    };
    let not_closure = unify_type_pattern(&closure, &function, &mut HashMap::new())
        .expect_err("a closure pattern cannot unify with a written function type");
    assert!(not_closure.message.starts_with("expected `closure def()"));

    let mut different_capture = closure.clone();
    let Type::Closure { captures, .. } = &mut different_capture else {
        unreachable!()
    };
    captures[0].name = "other".to_string();
    let capture_mismatch = unify_type_pattern(&closure, &different_capture, &mut HashMap::new())
        .expect_err("closure capture identity participates in type-pattern unification");
    assert!(capture_mismatch
        .message
        .starts_with("expected `closure def()"));
}

#[test]
fn s1_sema_module_constants_reject_self_reentry_rebinding_mutation_and_moves() {
    let reentry = crate::check_source("VALUE: int64 = VALUE\n\ndef main():\n    pass\n")
        .expect_err("a module constant cannot re-enter its own initializer");
    assert_eq!(reentry.code, "AU2001");
    assert_eq!(
        reentry.message,
        "module constant `VALUE` is used before initialization"
    );

    let rebound = crate::check_source(
        "NAMES: list[str] = [\"Aura\"]\n\ndef main():\n    NAMES = [\"changed\"]\n",
    )
    .expect_err("module constants cannot be rebound");
    assert_eq!(rebound.code, "AU3003");
    assert_eq!(rebound.message, "module constant `NAMES` is immutable");

    let mutated = crate::check_source(
        "NAMES: list[str] = [\"Aura\"]\n\ndef main():\n    NAMES.append(\"changed\")\n",
    )
    .expect_err("module constants cannot provide mutable receivers");
    assert_eq!(mutated.code, "AU3003");
    assert_eq!(
        mutated.message,
        "module constant `NAMES` cannot provide a mutable receiver for method `append`"
    );

    let moved = crate::check_source(
        "import random\n\nSTATE: random.Rng = random.Rng(seed=7)\n\ndef consume(value: own random.Rng):\n    pass\n\ndef main():\n    consume(STATE)\n",
    )
    .expect_err("a non-cloneable module constant cannot move out of immutable storage");
    assert_eq!(moved.code, "AU3001");
    assert_eq!(
        moved.message,
        "cannot move `STATE` out of immutable module storage"
    );
    assert_eq!(
        moved.help,
        ["keep shared access or construct an independent owned value"]
    );

    let remote = crate::check_source("STATE: list[str] = [\"ready\"]\n\ndef main():\n    pass\n")
        .expect("the imported constant fixture must be a valid checked module");
    let state = remote.constants["STATE"].clone();
    let mut settings = namespace("settings");
    settings
        .constants
        .insert("STATE".to_string(), state.clone());
    settings.all_constants.insert("STATE".to_string(), state);
    let importing = crate::parser::parse(
        "def consume(value: own list[str]):\n    pass\n\ndef main():\n    consume(settings.STATE)\n",
    )
    .expect("qualified constant consumption source must parse");
    let qualified = check_with_context(
        importing,
        ModuleContext {
            module_name: "<main>".to_string(),
            imported_bindings: BTreeMap::from([(
                "settings".to_string(),
                ImportedBinding::Module(settings.clone()),
            )]),
            module_registry: BTreeMap::from([("settings".to_string(), settings)]),
            is_entry_module: true,
        },
    )
    .expect_err("qualified imported constants cannot move from module storage");
    assert_eq!(qualified.code, "AU3001");
    assert_eq!(
        qualified.message,
        "cannot move `settings.STATE` out of immutable module storage"
    );
    assert_eq!(
        qualified.help,
        ["keep shared access or construct an independent owned value"]
    );
}

#[test]
fn s1_sema_match_guards_and_or_patterns_pin_commit_binding_and_coverage_errors() {
    for (source, code, expected) in [
        (
            "def main():\n    value: int32 = 1\n    match value:\n        case 1 if 7:\n            pass\n        case _:\n            pass\n",
            "AU2002",
            "match guard expects exactly `bool`, found `int64`",
        ),
        (
            "enum Pair:\n    Left(int32)\n    Right(int32)\n\ndef main():\n    value = Pair.Left(1)\n    match value:\n        case Left(item) | Right(_):\n            print(item)\n",
            "AU2999",
            "every alternative in an or-pattern must bind the same names with identical types and capabilities",
        ),
        (
            "def main():\n    value: int32 = 1\n    match value:\n        case 1 | 1:\n            pass\n        case _:\n            pass\n",
            "AU2999",
            "duplicate or subsumed alternative in or-pattern",
        ),
        (
            "def main():\n    value: (bool, bool) = (true, false)\n    match value:\n        case (true, _) | (true, false):\n            pass\n        case _:\n            pass\n",
            "AU2999",
            "duplicate or subsumed alternative in or-pattern",
        ),
    ] {
        let error = crate::check_source(source).expect_err("invalid guarded match must fail");
        assert_eq!(error.code, code, "{source}");
        assert_eq!(error.message, expected, "{source}");
    }

    let candidate_move = crate::check_source(
        "enum Text:\n    Value(str)\n    Empty\n\ndef consume(value: own str) -> bool:\n    return value == \"yes\"\n\ndef main():\n    text = Text.Value(\"yes\")\n    match own text:\n        case Value(value) if consume(value):\n            pass\n        case Value(_):\n            pass\n        case Empty:\n            pass\n",
    )
    .expect_err("an own-match guard cannot consume a candidate before commit");
    assert_eq!(candidate_move.code, "AU3001");
    assert_eq!(
        candidate_move.message,
        "cannot move an owned match candidate before its guard commits the arm"
    );

    let nested = crate::check_source(
        "enum Flag:\n    On\n    Off\n\nenum Wrap:\n    Value(Flag)\n    Empty\n\ndef main():\n    value = Wrap.Value(Flag.On)\n    match value:\n        case Value(On):\n            pass\n        case Empty:\n            pass\n",
    )
    .expect_err("nested enum matches must report the uncovered payload shape");
    assert_eq!(nested.code, "AU2999");
    assert_eq!(
        nested.message,
        "non-exhaustive match over `Wrap`: missing `Value(Off)`"
    );

    let primitive = crate::check_source(
        "def main():\n    value: int64 = 1\n    match value:\n        case 1:\n            pass\n",
    )
    .expect_err("an open primitive match needs a fallback");
    assert_eq!(primitive.code, "AU2999");
    assert_eq!(
        primitive.message,
        "`match` over `int64` with literal patterns requires a final `case _:` arm"
    );

    let primitive_payload = crate::check_source(
        "enum Number:\n    Value(int64)\n    Empty\n\ndef main():\n    value = Number.Value(1)\n    match value:\n        case Value(1):\n            pass\n        case Empty:\n            pass\n",
    )
    .expect_err("an enum payload with an open primitive domain needs a fallback shape");
    assert_eq!(primitive_payload.code, "AU2999");
    assert_eq!(
        primitive_payload.message,
        "non-exhaustive match over `Number`: missing `Value(_)`"
    );

    let pair_payload = crate::check_source(
        "enum PairNumber:\n    Value(int64, int64)\n    Empty\n\ndef main():\n    value = PairNumber.Value(1, 1)\n    match value:\n        case Value(1, 1):\n            pass\n        case Empty:\n            pass\n",
    )
    .expect_err("an incompletely covered multi-payload variant needs its full fallback shape");
    assert_eq!(pair_payload.code, "AU2999");
    assert_eq!(
        pair_payload.message,
        "non-exhaustive match over `PairNumber`: missing `Value(_, _)`"
    );

    let bool_union = crate::check_source(
        "def main():\n    value: bool = true\n    match value:\n        case true | false:\n            pass\n        case _:\n            pass\n",
    )
    .expect_err("a complete bool or-pattern must subsume a later wildcard");
    assert_eq!(bool_union.code, "AU2999");
    assert_eq!(bool_union.message, "unreachable match arm");
}

#[test]
fn root_match_bindings_are_catch_alls_in_statements_and_expressions() {
    crate::check_source(
        r#"
class Box:
    value: int64

enum State:
    Ready
    Done

def statement(value: int64) -> int64:
    match value:
        case n if n > 0:
            return n
        case n:
            return 0 - n

def expression(value: int64) -> int64:
    return match value:
        case n if n >= 0: n
        case n: 0 - n

def enum_expression(state: State) -> int64:
    return match state:
        case whole if whole == State.Ready: 1
        case whole: 2

def consume(box: own Box) -> int64:
    match own box:
        case owned:
            return owned.value

def mutate(box: mut Box):
    match mut box:
        case current:
            current.value += 1

def whole_enum(state: State) -> State:
    match state:
        case whole:
            return whole
"#,
    )
    .expect("top-level bindings should be guarded or unguarded catch-all patterns");

    for (source, expected) in [
        (
            "def main():\n    match 1:\n        case n:\n            pass\n        case _:\n            pass\n",
            "catch-all match arm must be the final `case`",
        ),
        (
            "def choose_value() -> int64:\n    return match 1:\n        case n: n\n        case _: 0\n\ndef main():\n    pass\n",
            "catch-all match arm must be the final `case`",
        ),
        (
            "enum State:\n    Ready\n    Done\n\ndef main():\n    state = State.Ready\n    match state:\n        case whole:\n            pass\n        case Ready:\n            pass\n",
            "catch-all match arm must be the final `case`",
        ),
        (
            "enum State:\n    Ready\n    Done\n\ndef choose(state: State) -> int64:\n    return match state:\n        case whole: 1\n        case Ready: 2\n\ndef main():\n    pass\n",
            "catch-all match arm must be the final `case`",
        ),
        (
            "def main():\n    match 1:\n        case n if n > 0:\n            pass\n",
            "requires a final `case _:` arm",
        ),
        (
            "class Box:\n    value: int64\n\ndef choose(box: Box) -> int64:\n    return match box:\n        case Ready: 1\n        case whole: whole.value\n\ndef main():\n    pass\n",
            "class patterns are not supported",
        ),
        (
            "def choose(value: bool) -> int64:\n    return match value:\n        case _: 1\n        case true: 2\n\ndef main():\n    pass\n",
            "wildcard match arm must be the final `case`",
        ),
        (
            "def choose(value: bool) -> int64:\n    return match value:\n        case whole if whole: 1\n\ndef main():\n    pass\n",
            "non-exhaustive bool match: missing `false`, `true`",
        ),
    ] {
        let diagnostic = crate::check_source(source)
            .expect_err("root binding coverage and ordering must stay explicit");
        assert!(
            diagnostic.message.contains(expected),
            "expected `{expected}`, got {diagnostic:?}"
        );
    }
}

#[test]
fn s1_sema_fstring_specs_report_syntax_and_interpolated_type_failures() {
    let syntax = crate::check_source("def main():\n    print(f\"{1:q}\")\n")
        .expect_err("unsupported format codes must fail during checking");
    assert_eq!(syntax.code, "AU1101");
    assert_eq!(
        syntax.message,
        "unsupported format type `q`; supported codes are d, f, e, x, X, b, o, %, and s"
    );

    let typed = crate::check_source("def main():\n    print(f\"{true:d}\")\n")
        .expect_err("integer format codes require an integer interpolation");
    assert_eq!(typed.code, "AU2002");
    assert_eq!(
        typed.message,
        "integer format code requires an integer value, found value"
    );
    assert_eq!(
        typed.help,
        ["supported codes are d, f, e, x, X, b, o, %, and s; omit the code for ordinary rendering"]
    );
}
use std::sync::OnceLock;

fn empty_canonical_type_names() -> &'static BTreeMap<String, String> {
    static NAMES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    NAMES.get_or_init(BTreeMap::new)
}

fn empty_constants() -> &'static BTreeMap<String, ConstantInfo> {
    static EMPTY: std::sync::OnceLock<BTreeMap<String, ConstantInfo>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(BTreeMap::new)
}

#[test]
fn assertion_introspection_dispatch_requires_two_shared_custom_operands() {
    assert!(assertion_dispatch_is_non_consuming(None));
    assert!(assertion_dispatch_is_non_consuming(Some((
        ReceiverKind::Borrow,
        ReceiverKind::Borrow,
    ))));
    for dispatch in [
        (ReceiverKind::Value, ReceiverKind::Borrow),
        (ReceiverKind::Borrow, ReceiverKind::Value),
        (ReceiverKind::Value, ReceiverKind::Value),
        (ReceiverKind::BorrowMut, ReceiverKind::Borrow),
        (ReceiverKind::Borrow, ReceiverKind::BorrowMut),
    ] {
        assert!(!assertion_dispatch_is_non_consuming(Some(dispatch)));
    }
}

fn check_ffi_source_for_test(source: &str) -> Result<Program> {
    let module = crate::parse_source(source)?;
    crate::check_module_with_builtin_imports(module)
}

#[test]
fn round_and_divmod_builtin_overloads_preserve_exact_static_types() {
    crate::check_source(
        r#"
def main():
    tiny: int8 = -7
    other: int8 = 3
    same: int8 = round(tiny)
    half: float32 = 2.5
    rounded: int64 = round(value=half)
    integer_pair: (int8, int8) = divmod(left=tiny, right=other)
    float_pair: (float32, float32) = divmod(half, 1.5)
    i16: int16 = round(7 as int16)
    i32: int32 = round(7 as int32)
    i64: int64 = round(7 as int64)
    i128: int128 = round(7 as int128)
    isize: intsize = round(7 as intsize)
    u8: uint8 = round(7 as uint8)
    u16: uint16 = round(7 as uint16)
    u32: uint32 = round(7 as uint32)
    u64: uint64 = round(7 as uint64)
    u128: uint128 = round(7 as uint128)
    usize: uintsize = round(7 as uintsize)
    pair_i128: (int128, int128) = divmod(7 as int128, 3 as int128)
    pair_u128: (uint128, uint128) = divmod(7 as uint128, 3 as uint128)
"#,
    )
    .expect("round and divmod overloads should preserve their ratified result types");

    let round_domain = crate::check_source(
        r#"
def main():
    value = round("1")
"#,
    )
    .expect_err("round must reject non-numeric values");
    assert_eq!(round_domain.code, "AU2003");
    assert!(round_domain.message.contains("expects an integer"));

    let divmod_domain = crate::check_source(
        r#"
def main():
    value = divmod("1", "2")
"#,
    )
    .expect_err("divmod must reject non-numeric values");
    assert_eq!(divmod_domain.code, "AU2003");

    let divmod_mismatch = crate::check_source(
        r#"
def main():
    value = divmod(1, 2.0)
"#,
    )
    .expect_err("divmod operands must have one exact type");
    assert_eq!(divmod_mismatch.code, "AU2002");
    assert!(divmod_mismatch.message.contains("one exact type"));

    let arity = crate::check_source(
        r#"
def main():
    value = round(1, 2)
"#,
    )
    .expect_err("round has no digit-count overload");
    assert_eq!(arity.code, "AU2004");
}

#[test]
fn math_module_requires_and_returns_exact_float64_types() {
    check_ffi_source_for_test(
        r#"
import math

def main():
    floored: int64 = math.floor(1.5)
    ceiled: int64 = math.ceil(1.5)
    truncated: int64 = math.trunc(-1.5)
    powered: float64 = math.pow(2.0, 3.0)
    exponential: float64 = math.exp(1.0)
    natural: float64 = math.log(2.0)
    binary: float64 = math.log2(8.0)
    decimal: float64 = math.log10(1000.0)
    sine: float64 = math.sin(0.0)
    cosine: float64 = math.cos(0.0)
    tangent: float64 = math.tan(0.0)
"#,
    )
    .expect("the exact math module signatures should type check");

    for source in [
        "import math\n\ndef main():\n    value = math.sin(1 as float32)\n",
        "import math\n\ndef main():\n    value = math.pow(2.0, 3 as int64)\n",
    ] {
        let error = check_ffi_source_for_test(source)
            .expect_err("math must not introduce implicit numeric conversions");
        assert_eq!(error.code, "AU2002");
        assert!(error.message.contains("float64"));
    }
}

fn public_ffi_handle_namespace(module_name: &str) -> ModuleNamespace {
    let remote = check_ffi_source_for_test("public extern \"C\" opaque class Handle\n")
        .expect("public remote opaque handle should check");
    let mut handle = remote.opaque_handles["Handle"].clone();
    handle.module_name = module_name.to_string();
    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: module_name.to_string(),
        path: module_name.to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: BTreeMap::new(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::from([("Handle".to_string(), handle.clone())]),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::new(),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::from([("Handle".to_string(), handle)]),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    }
}

fn public_ffi_function_namespace(module_name: &str) -> ModuleNamespace {
    let remote =
        check_ffi_source_for_test("public extern \"C\" def scalar(value: int32) -> int64\n")
            .expect("public remote extern function should check");
    let mut scalar = remote.extern_functions["scalar"].clone();
    scalar.module_name = module_name.to_string();
    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: module_name.to_string(),
        path: module_name.to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: BTreeMap::new(),
        extern_functions: BTreeMap::from([("scalar".to_string(), scalar.clone())]),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::new(),
        all_extern_functions: BTreeMap::from([("scalar".to_string(), scalar)]),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    }
}

#[test]
fn array_surface_type_checks_constructors_members_operators_and_transfer() {
    crate::check_source(
        r#"
def main():
    mut values = Array[int32].from_list(values=[1, 2, 3, 4], shape=[2, 2])
    zeros: Array[float64] = Array[float64].zeros(shape=[2, 2])
    full = Array[int64].full(shape=[2, 2], value=7)

    shape: list[int64] = values.shape()
    count: int64 = values.len()
    copied: Array[int32] = values.clone()
    maybe: Option[int32] = values.get(index=[0, 1])
    old: Option[int32] = values.set(index=[0, 1], value=9)
    values.fill(value=0)
    item: int32 = values[0, 1]
    coordinates: list[int64] = [0]
    borrowed_item: int32 = values[coordinates[0]]
    values[0, 1] = item
    rows: Array[int32] = values[:1]

    arrays: Array[int32] = values + copied
    right_scalar: Array[int32] = values * 2
    left_scalar: Array[int32] = 2 - values
    quotient: Array[float64] = zeros / 2.0
    mapped: Array[float64] = values.map[float64](f=lambda value: value.to_float())
    total: int32 = values.sum()
    minimum: int32 = values.min()
    maximum: int32 = values.max()
    average: float64 = values.mean()

    wrapped: Array[int32] = values.wrapping_add(rhs=1)
    saturated: Array[int32] = values.saturating_mul(rhs=copied)
    scalar: int32 = 7
    wrapped_scalar: int32 = scalar.wrapping_sub(rhs=9)
    saturated_scalar: int32 = scalar.saturating_add(rhs=9)

    queue = Queue[Array[int32]]()
    print(shape)
    print(count)
    print(full)
    print(borrowed_item)
    print(arrays)
    print(right_scalar)
    print(left_scalar)
    print(quotient)
    print(mapped)
    print(total)
    print(minimum)
    print(maximum)
    print(average)
    print(wrapped)
    print(saturated)
    print(wrapped_scalar)
    print(saturated_scalar)
    print(queue)
"#,
    )
    .expect("the complete compiler-facing Array surface should type-check");
}

#[test]
fn array_surface_rejects_invalid_dtypes_mixed_arithmetic_and_invalid_members() {
    for (source, code, message) in [
        (
            "def main():\n    value: Array[str] = Array[str].zeros(shape=[1])\n",
            "AU2002",
            "Array dtype must be one of",
        ),
        (
            "def main():\n    left = Array[int32].zeros(shape=[1])\n    right = Array[int64].zeros(shape=[1])\n    print(left + right)\n",
            "AU2002",
            "Array arithmetic requires matching dtypes",
        ),
        (
            "def main():\n    values = Array[int32].zeros(shape=[1])\n    print(values / 2)\n",
            "AU2003",
            "integer Array `/` is not supported",
        ),
        (
            "def main():\n    left = Array[float64].zeros(shape=[1])\n    right = Array[float64].zeros(shape=[1])\n    print(left == right)\n",
            "AU2003",
            "Array equality is not supported",
        ),
        (
            "def main():\n    left = Array[float64].zeros(shape=[1])\n    right = Array[float64].zeros(shape=[1])\n    print(left != right)\n",
            "AU2003",
            "Array equality is not supported",
        ),
        (
            "def main():\n    values = Array[int32].zeros(shape=[1])\n    mapped = values.map(lambda value: value > 0)\n    print(mapped)\n",
            "AU2002",
            "Array.map callback must return",
        ),
        (
            "def main():\n    values = Array[int32].zeros(shape=[1])\n    mapped = values.map[int64](f=lambda value: value.to_float())\n    print(mapped)\n",
            "AU2002",
            "Array.map type argument `int64` does not match callback result `float64`",
        ),
        (
            "def main():\n    values = Array[int32].zeros(shape=[1])\n    index: uint64 = 0\n    print(values[index])\n",
            "AU2002",
            "Array indices must have type `int64` or a losslessly narrower integer type",
        ),
        (
            "def main():\n    values = Array[int32].zeros(shape=[1])\n    values.fill(value=1)\n",
            "AU3003",
            "mutable receiver",
        ),
        (
            "def main():\n    values = Array[int32].zeros(shape=[1])\n    other = Array[int64].zeros(shape=[1])\n    print(values.wrapping_add(rhs=other))\n",
            "AU2002",
            "expects `Array[int32]` or `int32`",
        ),
    ] {
        let diagnostic = crate::check_source(source).expect_err(message);
        assert_eq!(diagnostic.code, code, "{source}\n{diagnostic:?}");
        assert!(
            diagnostic.message.contains(message),
            "{source}\nexpected `{message}` in `{}`",
            diagnostic.message
        );
    }
}

#[test]
fn array_surface_reports_reachable_constructor_member_and_operator_contracts() {
    let cases = [
        (
            "missing constructor dtype",
            "def main():\n    values = Array.zeros([1])\n    print(values)\n",
            "AU2005",
            "Array associated functions require an explicit dtype such as `Array[int32]`",
        ),
        (
            "too many constructor dtype arguments",
            "def main():\n    values = Array[int32, int64].zeros([1])\n    print(values)\n",
            "AU2002",
            "`Array` expects exactly one type argument, found 2",
        ),
        (
            "invalid constructor dtype",
            "def main():\n    values = Array[str].zeros([1])\n    print(values)\n",
            "AU2002",
            "Array dtype must be one of `int32`, `int64`, `float32`, or `float64`, found `str`",
        ),
        (
            "unknown constructor",
            "def main():\n    values = Array[int32].unknown([1])\n    print(values)\n",
            "AU2001",
            "type `Array` has no associated function `unknown`",
        ),
        (
            "constructor method type arguments",
            "def main():\n    values = Array[int32].zeros[int64]([1])\n    print(values)\n",
            "AU2005",
            "`Array.zeros` does not take explicit type arguments",
        ),
        (
            "builtin member type arguments",
            "def main():\n    values = Array[int32].zeros([1])\n    print(values.len[int64]())\n",
            "AU2005",
            "builtin method `Array.len` does not take explicit type arguments",
        ),
        (
            "zeros shape",
            "def main():\n    shape: list[int32] = [1]\n    values = Array[int32].zeros(shape)\n    print(values)\n",
            "AU2002",
            "`Array.zeros` expects `list[int64]` for `shape`, found `list[int32]`",
        ),
        (
            "full value",
            "def main():\n    values = Array[int32].full([1], \"bad\")\n    print(values)\n",
            "AU2002",
            "`Array.full` expects `int32` for `value`, found `str`",
        ),
        (
            "from_list values",
            "def main():\n    values: list[int64] = [1]\n    array = Array[int32].from_list(values, [1])\n    print(array)\n",
            "AU2002",
            "`Array.from_list` expects `list[int32]` for `values`, found `list[int64]`",
        ),
        (
            "from_list shape",
            "def main():\n    shape: list[int32] = [1]\n    array = Array[int32].from_list([1], shape)\n    print(array)\n",
            "AU2002",
            "`Array.from_list` expects `list[int64]` for `shape`, found `list[int32]`",
        ),
        (
            "get index",
            "def main():\n    values = Array[int32].zeros([1])\n    index: list[int32] = [0]\n    print(values.get(index))\n",
            "AU2002",
            "`Array.get` expects `list[int64]`, found `list[int32]`",
        ),
        (
            "set index",
            "def main():\n    mut values = Array[int32].zeros([1])\n    index: list[int32] = [0]\n    print(values.set(index, 1))\n",
            "AU2002",
            "`Array.set` expects `list[int64]`, found `list[int32]`",
        ),
        (
            "set value",
            "def main():\n    mut values = Array[int32].zeros([1])\n    print(values.set([0], \"bad\"))\n",
            "AU2002",
            "`Array.set` expects `int32`, found `str`",
        ),
        (
            "fill value",
            "def main():\n    mut values = Array[int32].zeros([1])\n    values.fill(\"bad\")\n",
            "AU2002",
            "`Array.fill` expects `int32`, found `str`",
        ),
        (
            "map type argument arity",
            "def widen(value: int32) -> float64:\n    return value.to_float()\n\ndef main():\n    values = Array[int32].zeros([1])\n    print(values.map[int64, float64](widen))\n",
            "AU2002",
            "`Array.map` expects exactly one type argument, found 2",
        ),
        (
            "map output dtype",
            "def widen(value: int32) -> float64:\n    return value.to_float()\n\ndef main():\n    values = Array[int32].zeros([1])\n    print(values.map[str](widen))\n",
            "AU2002",
            "Array.map output dtype must be one of `int32`, `int64`, `float32`, or `float64`, found `str`",
        ),
        (
            "integer-only method",
            "def main():\n    values = Array[float64].zeros([1])\n    print(values.wrapping_add(1.0))\n",
            "AU2003",
            "`Array.wrapping_add` is available only for integer Arrays",
        ),
        (
            "array floor division",
            "def main():\n    values = Array[int32].zeros([1])\n    print(values // values)\n",
            "AU2003",
            "operator `//` is not supported for Array values",
        ),
        (
            "array remainder",
            "def main():\n    values = Array[int32].zeros([1])\n    print(values % values)\n",
            "AU2003",
            "operator `%` is not supported for Array values",
        ),
        (
            "array less-than",
            "def main():\n    values = Array[int32].zeros([1])\n    print(values < values)\n",
            "AU2003",
            "operator `<` is not supported for Array values",
        ),
        (
            "array less-than-or-equal",
            "def main():\n    values = Array[int32].zeros([1])\n    print(values <= values)\n",
            "AU2003",
            "operator `<=` is not supported for Array values",
        ),
        (
            "array greater-than",
            "def main():\n    values = Array[int32].zeros([1])\n    print(values > values)\n",
            "AU2003",
            "operator `>` is not supported for Array values",
        ),
        (
            "array greater-than-or-equal",
            "def main():\n    values = Array[int32].zeros([1])\n    print(values >= values)\n",
            "AU2003",
            "operator `>=` is not supported for Array values",
        ),
        (
            "array logical and",
            "def main():\n    values = Array[int32].zeros([1])\n    print(values and values)\n",
            "AU2003",
            "operator `logical operator` is not supported for Array values",
        ),
        (
            "array logical or",
            "def main():\n    values = Array[int32].zeros([1])\n    print(values or values)\n",
            "AU2003",
            "operator `logical operator` is not supported for Array values",
        ),
        (
            "right scalar dtype",
            "def main():\n    values = Array[int32].zeros([1])\n    print(values + 1.0)\n",
            "AU2002",
            "Array arithmetic requires scalar dtype `int32`, found `float64`",
        ),
        (
            "left scalar dtype",
            "def main():\n    values = Array[int32].zeros([1])\n    print(1.0 + values)\n",
            "AU2002",
            "Array arithmetic requires scalar dtype `int32`, found `float64`",
        ),
    ];

    for (shape, source, code, message) in cases {
        let diagnostic = crate::check_source(source).expect_err(shape);
        assert_eq!(diagnostic.code, code, "{shape}: {diagnostic:?}");
        assert_eq!(diagnostic.message, message, "{shape}");
    }
}

#[test]
fn array_containment_rejects_contextual_empty_sets_and_map_indexing() {
    let cases = [
        (
            "empty contextual set",
            r#"
def main():
    values: set[Array[int32]] = {}
    print(values)
"#,
            "cannot use `Array[int32]` as a set element because it contains `Array[int32]`, whose equality is unavailable",
        ),
        (
            "dict indexed read",
            r#"
def read(values: dict[Array[int32], int64], key: Array[int32]) -> int64:
    return values[key]
"#,
            "cannot use dict indexing with `Array[int32]` because it contains `Array[int32]`, whose equality is unavailable",
        ),
        (
            "dict indexed write",
            r#"
def write(values: mut dict[Array[int32], int64], key: Array[int32]):
    values[key] = 1
"#,
            "cannot use dict indexing with `Array[int32]` because it contains `Array[int32]`, whose equality is unavailable",
        ),
        (
            "mismatched nested equality",
            r#"
def main():
    values: list[Array[int32]] = [Array[int32].zeros([1])]
    print(1 == values)
"#,
            "cannot compare `list[Array[int32]]` because it contains `Array[int32]`, whose equality is unavailable",
        ),
    ];

    for (shape, source, message) in cases {
        let diagnostic = crate::check_source(source).expect_err(shape);
        assert_eq!(diagnostic.code, "AU2003", "{shape}: {diagnostic:?}");
        assert_eq!(diagnostic.message, message, "{shape}");
    }
}

#[test]
fn recursive_array_free_enums_remain_equality_eligible() {
    crate::check_source(
        r#"
enum Chain:
    End
    Next(indirect Chain)

def equal(left: Chain, right: Chain) -> bool:
    return left == right
"#,
    )
    .expect("recursive Array-free enums should terminate equality containment analysis");
}

#[test]
fn trait_equality_contracts_reject_concrete_arrays_and_strengthening_impls() {
    let specialized = crate::check_source(
        r#"
trait Equaler[Item]:
    def equal(self, left: Item, right: Item) -> bool:
        return left == right

class Fixed:
    value: int64

impl Equaler[Array[int32]] for Fixed:
    pass
"#,
    )
    .expect_err("a concrete Array specialization cannot satisfy an equality-bearing default");
    assert_eq!(specialized.code, "AU2003");
    assert_eq!(
        specialized.message,
        "impl method `equal` cannot satisfy the trait's equality contract because `Array[int32]` contains `Array[int32]`, whose equality is unavailable"
    );

    let strengthened = crate::check_source(
        r#"
trait Equaler[T]:
    def equal(self, left: T, right: T) -> bool

class Matcher[T]:
    value: T

impl[T] Equaler[T] for Matcher[T]:
    def equal(self, left: T, right: T) -> bool:
        return left == right
"#,
    )
    .expect_err("an impl may not strengthen an abstract trait equality contract");
    assert_eq!(strengthened.code, "AU2003");
    assert_eq!(
        strengthened.message,
        "impl method `equal` would strengthen its trait's equality contract for type parameter `T`; put the equality-bearing behavior in the trait default method so callers can enforce it"
    );
}

#[test]
fn trait_method_generics_defer_array_equality_until_call_inference() {
    let surface = r#"
trait Equaler:
    def equal[T](self, left: T, right: T) -> bool:
        return left == right

class Marker:
    value: int64

impl Equaler for Marker:
    pass
"#;

    crate::check_source(&format!(
        "{surface}\ndef main():\n    marker = Marker(0)\n    print(marker.equal(1, 1))\n"
    ))
    .expect("eligible inferred method arguments should satisfy the equality contract");

    let direct = crate::check_source(&format!(
        "{surface}\ndef main():\n    marker = Marker(0)\n    left = Array[int32].zeros([1])\n    right = Array[int32].zeros([1])\n    print(marker.equal(left, right))\n"
    ))
    .expect_err("direct trait dispatch must reject an inferred Array method argument");
    assert_eq!(direct.code, "AU2003", "{direct:?}");
    assert_eq!(
        direct.message,
        "cannot use method `equal` with `Array[int32]` because it contains `Array[int32]`, whose equality is unavailable"
    );

    let bound = crate::check_source(&format!(
        "{surface}\ndef forward[T, C: Equaler](marker: C, left: T, right: T) -> bool:\n    return marker.equal(left, right)\n\ndef main():\n    marker = Marker(0)\n    left = Array[int32].zeros([1])\n    right = Array[int32].zeros([1])\n    print(forward[Array[int32], Marker](marker, left, right))\n"
    ))
    .expect_err("bound dispatch must propagate the inferred Array equality obligation");
    assert_eq!(bound.code, "AU2003");
    assert_eq!(
        bound.message,
        "cannot use function `forward` with `Array[int32]` because it contains `Array[int32]`, whose equality is unavailable"
    );
}

#[test]
fn operator_dispatch_propagates_array_equality_contracts() {
    let binary_surface = r#"
trait Add[Rhs]:
    def add[T](self, rhs: T) -> bool:
        return rhs == rhs

class Marker:
    value: int64

class Payload[T]:
    value: T

class BoundMarker:
    value: int64

impl[Rhs] Add[Rhs] for Marker:
    pass

impl Add[int64] for BoundMarker:
    pass
"#;

    crate::check_source(&format!(
        "{binary_surface}\ndef main():\n    marker = Marker(0)\n    print(marker + 1)\n"
    ))
    .expect("eligible inferred operator arguments should satisfy equality");

    let direct = crate::check_source(&format!(
        "{binary_surface}\ndef main():\n    marker = Marker(0)\n    payload = Payload(Array[int32].zeros([1]))\n    print(marker + payload)\n"
    ))
    .expect_err("concrete operator dispatch must reject an inferred Array argument");
    assert_eq!(direct.code, "AU2003", "{direct:?}");
    assert_eq!(
        direct.message,
        "cannot use operator trait `Add.add` with `Payload[Array[int32]]` because it contains `Array[int32]`, whose equality is unavailable"
    );

    crate::check_source(&format!(
        "{binary_surface}\ndef combine[C: Add[int64]](marker: C, rhs: int64) -> bool:\n    return marker + rhs\n\ndef main():\n    marker = BoundMarker(0)\n    print(combine[BoundMarker](marker, 1))\n"
    ))
    .expect("bound operator dispatch should preserve eligible method-generic equality");

    let unary_surface = r#"
trait Neg[Out]:
    def neg(self):
        print(self == self)

class Wrapper[T]:
    value: T

class BoundWrapper:
    value: int64

impl[T] Neg[None] for Wrapper[T]:
    pass

impl Neg[None] for BoundWrapper:
    pass
"#;

    crate::check_source(&format!(
        "{unary_surface}\ndef main():\n    wrapper = Wrapper(1)\n    print(-wrapper)\n"
    ))
    .expect("eligible unary operator receivers should satisfy equality");

    let unary = crate::check_source(&format!(
        "{unary_surface}\ndef main():\n    wrapper = Wrapper(Array[int32].zeros([1]))\n    print(-wrapper)\n"
    ))
    .expect_err("unary operator dispatch must reject an Array-containing receiver");
    assert_eq!(unary.code, "AU2003");
    assert_eq!(
        unary.message,
        "cannot use operator trait `Neg.neg` with `Array[int32]` because it contains `Array[int32]`, whose equality is unavailable"
    );

    crate::check_source(&format!(
        "{unary_surface}\ndef negate[C: Neg[None]](value: C):\n    -value\n\ndef main():\n    wrapper = BoundWrapper(1)\n    negate[BoundWrapper](wrapper)\n"
    ))
    .expect("bound unary dispatch should preserve eligible receiver equality");
}

#[test]
fn array_constructor_remains_a_global_type_inside_binary_expressions() {
    crate::check_source(
        r#"
def main():
    ints = Array[int32].zeros([1])
    floats = Array[float64].full([2, 2], 8.0) / 2.0
    print(ints)
    print(floats)
"#,
    )
    .expect("Array constructors of another dtype must stay visible inside binary expressions");
}

#[test]
fn array_containment_recursively_disables_structural_equality() {
    let cases = [
        (
            "list value",
            "list[Array[int32]]",
            r#"
def main():
    left: list[Array[int32]] = [Array[int32].zeros([1])]
    right: list[Array[int32]] = [Array[int32].zeros([1])]
    print(left == right)
"#,
        ),
        (
            "dict value",
            "dict[str, Array[int32]]",
            r#"
def main():
    left: dict[str, Array[int32]] = {"values": Array[int32].zeros([1])}
    right: dict[str, Array[int32]] = {"values": Array[int32].zeros([1])}
    print(left != right)
"#,
        ),
        (
            "tuple element",
            "(Array[int32], int64)",
            r#"
def main():
    left = (Array[int32].zeros([1]), 1)
    right = (Array[int32].zeros([1]), 1)
    print(left == right)
"#,
        ),
        (
            "Option payload",
            "Option[Array[int32]]",
            r#"
def main():
    left: Option[Array[int32]] = Option.Some(Array[int32].zeros([1]))
    right: Option[Array[int32]] = Option.Some(Array[int32].zeros([1]))
    print(left == right)
"#,
        ),
        (
            "Result payload",
            "Result[Array[int32], str]",
            r#"
def main():
    left: Result[Array[int32], str] = Result.Ok(Array[int32].zeros([1]))
    right: Result[Array[int32], str] = Result.Ok(Array[int32].zeros([1]))
    print(left != right)
"#,
        ),
        (
            "class field",
            "Batch",
            r#"
class Batch:
    values: Array[int32]

def main():
    left = Batch(values=Array[int32].zeros([1]))
    right = Batch(values=Array[int32].zeros([1]))
    print(left == right)
"#,
        ),
        (
            "enum payload",
            "Payload",
            r#"
enum Payload:
    Batch(Array[int32])

def main():
    left: Payload = Payload.Batch(Array[int32].zeros([1]))
    right: Payload = Payload.Batch(Array[int32].zeros([1]))
    print(left != right)
"#,
        ),
    ];

    for (shape, compared_type, source) in cases {
        let diagnostic =
            crate::check_source(source).expect_err("nested Arrays must disable equality");
        assert_eq!(diagnostic.code, "AU2003", "{shape}: {diagnostic:?}");
        assert_eq!(
            diagnostic.message,
            format!(
                "cannot compare `{compared_type}` because it contains `Array[int32]`, whose equality is unavailable"
            ),
            "{shape}"
        );
        assert_eq!(
            diagnostic.help,
            vec![
                "compare Array elements explicitly, or compare a chosen scalar summary such as shape, length, or a reduction result"
                    .to_string()
            ],
            "{shape}"
        );
    }
}

#[test]
fn array_containment_disables_membership_and_key_deduplication() {
    let cases = [
        (
            "membership operator",
            r#"
def main():
    needle = Array[int32].zeros([1])
    values: list[Array[int32]] = [needle.clone()]
    print(needle in values)
"#,
            "cannot test membership for `Array[int32]` because it contains `Array[int32]`, whose equality is unavailable",
        ),
        (
            "list.contains",
            r#"
def main():
    needle = Array[int32].zeros([1])
    values: list[Array[int32]] = [needle.clone()]
    print(values.contains(needle))
"#,
            "cannot use `list.contains` with `Array[int32]` because it contains `Array[int32]`, whose equality is unavailable",
        ),
        (
            "set constructor",
            r#"
def main():
    values = set[Array[int32]]()
    print(values)
"#,
            "cannot use `Array[int32]` as a set element because it contains `Array[int32]`, whose equality is unavailable",
        ),
        (
            "set literal",
            r#"
def main():
    values = {Array[int32].zeros([1])}
    print(values)
"#,
            "cannot use `Array[int32]` as a set element because it contains `Array[int32]`, whose equality is unavailable",
        ),
        (
            "dict constructor",
            r#"
def main():
    values = dict[Array[int32], int64]()
    print(values)
"#,
            "cannot use `Array[int32]` as a dict key because it contains `Array[int32]`, whose equality is unavailable",
        ),
        (
            "dict literal",
            r#"
def main():
    values = {Array[int32].zeros([1]): 1}
    print(values)
"#,
            "cannot use `Array[int32]` as a dict key because it contains `Array[int32]`, whose equality is unavailable",
        ),
        (
            "set comprehension",
            r#"
def main():
    source: list[Array[int32]] = [Array[int32].zeros([1])]
    values = {value.clone() for value in source}
    print(values)
"#,
            "cannot use `Array[int32]` as a set element because it contains `Array[int32]`, whose equality is unavailable",
        ),
        (
            "dict comprehension",
            r#"
def main():
    source: list[Array[int32]] = [Array[int32].zeros([1])]
    values = {value.clone(): 1 for value in source}
    print(values)
"#,
            "cannot use `Array[int32]` as a dict key because it contains `Array[int32]`, whose equality is unavailable",
        ),
    ];

    for (shape, source, expected_message) in cases {
        let diagnostic =
            crate::check_source(source).expect_err("Array comparison must not hide in collections");
        assert_eq!(diagnostic.code, "AU2003", "{shape}: {diagnostic:?}");
        assert_eq!(diagnostic.message, expected_message, "{shape}");
        assert_eq!(
            diagnostic.help,
            vec![
                "compare Array elements explicitly, or compare a chosen scalar summary such as shape, length, or a reduction result"
                    .to_string()
            ],
            "{shape}"
        );
    }
}

#[test]
fn equality_and_membership_remain_available_for_array_free_values() {
    crate::check_source(
        r#"
class Pair:
    values: list[int32]

enum MaybePair:
    Missing
    Present(Pair)

def main():
    left = MaybePair.Present(Pair(values=[1, 2]))
    right = MaybePair.Present(Pair(values=[1, 2]))
    equal: bool = left == right
    numbers: list[(int32, str)] = [(1, "one")]
    present: bool = (1, "one") in numbers
    keys: set[(int32, str)] = {(1, "one")}
    lookup: dict[(int32, str), bool] = {(1, "one"): true}
    print(equal)
    print(present)
    print(keys)
    print(lookup)
"#,
    )
    .expect("recursive eligibility checks must preserve equality for Array-free values");
}

#[test]
fn generic_equality_rejects_array_substitutions_without_rejecting_eligible_ones() {
    let diagnostic = crate::check_source(
        r#"
def equal[T](left: T, right: T) -> bool:
    return left == right

def main():
    left = Array[int32].zeros([1])
    right = Array[int32].zeros([1])
    print(equal[Array[int32]](left, right))
"#,
    )
    .expect_err("a generic equality operation must retain its equality-eligibility obligation");
    assert_eq!(diagnostic.code, "AU2003", "{diagnostic:?}");
    assert_eq!(
        diagnostic.message,
        "cannot use function `equal` with `Array[int32]` because it contains `Array[int32]`, whose equality is unavailable"
    );

    crate::check_source(
        r#"
def equal[T](left: T, right: T) -> bool:
    return left == right

def main():
    print(equal[float64](1.0, 1.0))
"#,
    )
    .expect("generic equality must remain available for eligible substitutions");
}

#[test]
fn generic_method_and_trait_dispatch_propagate_array_equality_obligations() {
    let class_method = crate::check_source(
        r#"
class Box[T]:
    value: T

    def equal(self, other: T) -> bool:
        return self.value == other

def main():
    value = Array[int32].zeros([1])
    box: Box[Array[int32]] = Box(value=value.clone())
    print(box.equal(value))
"#,
    )
    .expect_err("class-specialized method equality must reject Array");
    assert_eq!(class_method.code, "AU2003", "{class_method:?}");
    assert_eq!(
        class_method.message,
        "cannot use method `equal` with `Array[int32]` because it contains `Array[int32]`, whose equality is unavailable"
    );

    let trait_dispatch = crate::check_source(
        r#"
trait Equaler[T]:
    def equal(self, left: T, right: T) -> bool:
        return left == right

class Matcher[T]:
    pass

impl[T] Equaler[T] for Matcher[T]:
    pass

def compare[T](matcher: Matcher[T], left: T, right: T) -> bool:
    return matcher.equal(left, right)

def main():
    matcher = Matcher[Array[int32]]()
    left = Array[int32].zeros([1])
    right = Array[int32].zeros([1])
    print(compare[Array[int32]](matcher, left, right))
"#,
    )
    .expect_err("trait-dispatched generic equality must propagate to its caller");
    assert_eq!(trait_dispatch.code, "AU2003", "{trait_dispatch:?}");
    assert_eq!(
        trait_dispatch.message,
        "cannot use function `compare` with `Array[int32]` because it contains `Array[int32]`, whose equality is unavailable"
    );
}

#[test]
fn ffi_v0_accepts_fixed_width_scalar_signatures_and_the_int64_alias() {
    let source = r#"
extern "C" def scalars(a: bool, b: int8, c: int16, d: int32, e: int64, f: int, g: uint8, h: uint16, i: uint32, j: uint64, k: float32, l: float64) -> None
"#;
    let program =
        check_ffi_source_for_test(source).expect("the complete FFI v0 scalar set should check");
    let function = &program.extern_functions["scalars"];
    assert_eq!(function.decl.abi, "C");
    assert_eq!(function.signature.params[5], Type::named("int64"));
    assert_eq!(function.signature.return_type, Type::Unit);
}

#[test]
fn ffi_v0_validates_views_and_opaque_handle_capabilities() {
    check_ffi_source_for_test(
        r#"
extern "C" opaque class Handle
extern "C" def inspect(name: str, bytes: list[uint8], output: mut list[uint8], handle: Handle) -> int32
extern "C" def close(handle: own Handle) -> None
extern "C" def acquire() -> Handle
"#,
    )
    .expect("FFI byte views and opaque handle ownership should check");

    let rejected = [
        (
            r#"extern "C" def bad(value: own int32) -> None
"#,
            "fixed-width scalar parameter `value` must use the bare capability",
        ),
        (
            r#"extern "C" def bad(value: mut int32) -> None
"#,
            "fixed-width scalar parameter `value` must use the bare capability",
        ),
        (
            r#"extern "C" def bad(value: own str) -> None
"#,
            "str view parameter `value` must use the bare capability",
        ),
        (
            r#"extern "C" def bad(value: mut str) -> None
"#,
            "mutable str views are reserved",
        ),
        (
            r#"extern "C" def bad(value: own list[uint8]) -> None
"#,
            "owned byte views are reserved",
        ),
        (
            r#"extern "C" opaque class Handle
extern "C" def bad(value: mut Handle) -> None
"#,
            "mutable opaque-handle parameters are reserved",
        ),
    ];
    for (source, message) in rejected {
        let diagnostic = check_ffi_source_for_test(source)
            .expect_err("the unsupported FFI capability must fail");
        assert_eq!(diagnostic.code, "AU3004", "{source}");
        assert!(
            diagnostic.message.contains(message),
            "{}",
            diagnostic.message
        );
    }
}

#[test]
fn ffi_v0_rejects_unsupported_abi_types_and_returned_views() {
    let rejected = [
        (
            r#"extern "C" def bad(value: int128) -> None
"#,
            "FFI v0 does not support parameter type `int128`",
            "AU2002",
        ),
        (
            r#"extern "C" def bad(value: intsize) -> None
"#,
            "FFI v0 does not support parameter type `intsize`",
            "AU2002",
        ),
        (
            r#"extern "C" def bad(value: list[int32]) -> None
"#,
            "only `list[uint8]` is supported as an FFI byte view",
            "AU2002",
        ),
        (
            r#"extern "C" def bad(value: (int32, int32)) -> None
"#,
            "FFI v0 does not support parameter type `(int32, int32)`",
            "AU2002",
        ),
        (
            r#"extern "C" def bad(value: def(int32) -> int32) -> None
"#,
            "FFI v0 does not support callback parameters or returns",
            "AU1101",
        ),
        (
            r#"extern "C" def bad(value: Ptr[uint8]) -> None
"#,
            "raw pointers are reserved",
            "AU2005",
        ),
        (
            r#"extern "C" def bad() -> str
"#,
            "FFI v0 cannot return a str view",
            "AU2002",
        ),
        (
            r#"extern "C" def bad() -> list[uint8]
"#,
            "FFI v0 cannot return a list[uint8] view",
            "AU2002",
        ),
        (
            r#"extern "C" def bad() -> (int32, int32)
"#,
            "FFI v0 does not support return type `(int32, int32)`",
            "AU2002",
        ),
    ];
    for (source, message, code) in rejected {
        let diagnostic =
            check_ffi_source_for_test(source).expect_err("the unsupported FFI type must fail");
        assert_eq!(diagnostic.code, code, "{source}");
        assert!(
            diagnostic.message.contains(message),
            "{}",
            diagnostic.message
        );
    }
}

#[test]
fn ffi_calls_preserve_the_declared_result_type_in_aura_contexts() {
    let diagnostic = check_ffi_source_for_test(
        r#"
extern "C" def current_value() -> int64

def main():
    narrowed: int32 = current_value()
"#,
    )
    .expect_err("an extern result must retain its declared fixed-width type");
    assert_eq!(diagnostic.code, "AU2002");
    assert_eq!(
        diagnostic.message,
        "result type mismatch for extern function `current_value`: expected `int64`, found `int32`"
    );
}

#[test]
fn ffi_v0_rejects_duplicate_names_and_unsupported_returns() {
    let rejected = [
        (
            "duplicate parameter",
            "extern \"C\" def bad(value: int32, value: int64) -> None\n",
            "AU2999",
            "duplicate parameter `value` on extern function `bad`",
        ),
        (
            "builtin function name",
            "extern \"C\" def len(value: int32) -> int32\n",
            "AU2007",
            "`len` is a builtin function name and cannot be redefined",
        ),
        (
            "duplicate opaque item",
            "extern \"C\" opaque class Handle\nclass Handle:\n    value: int32\n",
            "AU2999",
            "duplicate item `Handle`",
        ),
        (
            "raw-pointer return",
            "extern \"C\" def bad() -> Ptr[uint8]\n",
            "AU2005",
            "FFI raw-pointer returns are reserved",
        ),
        (
            "unsupported scalar return",
            "extern \"C\" def bad() -> int128\n",
            "AU2002",
            "FFI v0 does not support return type `int128`",
        ),
    ];

    for (case, source, code, message) in rejected {
        let diagnostic =
            check_ffi_source_for_test(source).expect_err("invalid FFI declarations must fail");
        assert_eq!(diagnostic.code, code, "{case}: {diagnostic:?}");
        assert!(
            diagnostic.message.contains(message),
            "{case}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn ffi_direct_call_only_contract_covers_specialization_values_and_tasks() {
    let specialized = check_ffi_source_for_test(
        r#"
extern "C" def scalar(value: int32) -> int64

def main():
    value = scalar[int32](7)
"#,
    )
    .expect_err("extern functions do not accept Aura type arguments");
    assert_eq!(specialized.code, "AU2005");
    assert!(
        specialized
            .message
            .contains("extern function `scalar` does not take type arguments"),
        "{}",
        specialized.message
    );

    let opaque_value = check_ffi_source_for_test(
        r#"
extern "C" opaque class Handle

def main():
    handle_type = Handle
"#,
    )
    .expect_err("an opaque handle type must not become a runtime value");
    assert_eq!(opaque_value.code, "AU2005");
    assert!(
        opaque_value
            .message
            .contains("opaque FFI handle type `Handle` is not a value"),
        "{}",
        opaque_value.message
    );

    let task_target = check_ffi_source_for_test(
        r#"
extern "C" def scalar(value: int32) -> int64

def main():
    with group = TaskGroup():
        group.start(scalar, 7)
"#,
    )
    .expect_err("extern calls are synchronous and cannot be task targets");
    assert_eq!(task_target.code, "AU2999");
    assert!(
        task_target.message.contains(
            "extern function `scalar` is direct-call-only and cannot be handed to a task"
        ),
        "{}",
        task_target.message
    );
}

#[test]
fn ffi_qualified_externs_remain_direct_call_only_as_values_and_task_targets() {
    let namespace = public_ffi_function_namespace("ffi_api");
    let imported_bindings = BTreeMap::from([(
        "ffi_api".to_string(),
        ImportedBinding::Module(namespace.clone()),
    )]);
    let module_registry = BTreeMap::from([("ffi_api".to_string(), namespace)]);

    for (case, body, expected) in [
        (
            "function value",
            "def main():\n    callback = ffi_api.scalar\n",
            "extern function `ffi_api.scalar` is direct-call-only and cannot be used as a function value",
        ),
        (
            "task target",
            "def main():\n    with group = TaskGroup():\n        group.start(ffi_api.scalar, 7)\n",
            "extern function `ffi_api.scalar` is direct-call-only and cannot be handed to a task",
        ),
    ] {
        let source = crate::parse_source(&format!("import ffi_api\n\n{body}"))
            .expect("qualified extern rejection probe should parse");
        let diagnostic = check_with_context(
            source,
            ModuleContext {
                module_name: "app".to_string(),
                imported_bindings: imported_bindings.clone(),
                module_registry: module_registry.clone(),
                is_entry_module: true,
            },
        )
        .expect_err("qualified externs must remain direct-call-only");
        assert_eq!(diagnostic.code, "AU2999", "{case}: {diagnostic:?}");
        assert!(
            diagnostic.message.contains(expected),
            "{case}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn ffi_from_imported_opaque_handles_preserve_canonical_type_identity() {
    let namespace = public_ffi_handle_namespace("ffi_types");
    let handle = namespace.opaque_handles["Handle"].clone();
    let module = crate::parse_source(
        "from ffi_types import Handle\n\nextern \"C\" def inspect(handle: Handle) -> Handle\n",
    )
    .expect("from-imported opaque-handle signature should parse");
    let program = check_with_context(
        module,
        ModuleContext {
            module_name: "app".to_string(),
            imported_bindings: BTreeMap::from([(
                "Handle".to_string(),
                ImportedBinding::OpaqueHandle(handle),
            )]),
            module_registry: BTreeMap::from([("ffi_types".to_string(), namespace)]),
            is_entry_module: true,
        },
    )
    .expect("from-imported opaque handles should be valid FFI parameter and return types");

    assert_eq!(
        program
            .canonical_type_names
            .get("Handle")
            .map(String::as_str),
        Some("ffi_types.Handle")
    );
    let signature = &program.extern_functions["inspect"].signature;
    assert_eq!(signature.params, vec![Type::named("ffi_types.Handle")]);
    assert_eq!(signature.return_type, Type::named("ffi_types.Handle"));
}

#[test]
fn ffi_opaque_signature_validation_uses_canonical_nominal_identity() {
    let namespace = public_ffi_handle_namespace("ffi_types");
    let imported_bindings = BTreeMap::from([(
        "ffi_types".to_string(),
        ImportedBinding::Module(namespace.clone()),
    )]);
    let module_registry = BTreeMap::from([("ffi_types".to_string(), namespace)]);

    for (case, declaration) in [
        (
            "parameter",
            "extern \"C\" def inspect(handle: Handle) -> None",
        ),
        ("return", "extern \"C\" def acquire() -> Handle"),
    ] {
        let module = crate::parse_source(&format!(
            r#"
import ffi_types

class Handle:
    value: int32

{declaration}
"#
        ))
        .expect("local-class/remote-handle collision probe should parse");
        let diagnostic = check_with_context(
            module,
            ModuleContext {
                module_name: "app".to_string(),
                imported_bindings: imported_bindings.clone(),
                module_registry: module_registry.clone(),
                is_entry_module: true,
            },
        )
        .expect_err("a same-basename ordinary class must not become an opaque FFI handle");
        assert_eq!(diagnostic.code, "AU2002", "{case}: {diagnostic:?}");
        assert!(
            diagnostic.message.contains(if case == "parameter" {
                "does not support parameter type `Handle`"
            } else {
                "does not support return type `Handle`"
            }),
            "{case}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn ffi_externs_are_direct_call_only_and_opaque_handles_are_not_transferable() {
    check_ffi_source_for_test(
        r#"
extern "C" opaque class Handle
extern "C" def get_number() -> int32
extern "C" def acquire() -> Handle
extern "C" def close(handle: own Handle) -> None

def main():
    number: int32 = get_number()
    handle = acquire()
    close(handle)
"#,
    )
    .expect("direct FFI calls and explicit handle consumption should check");

    let as_value = check_ffi_source_for_test(
        r#"
extern "C" def get_number() -> int32

def main():
    callback = get_number
"#,
    )
    .expect_err("an extern declaration must not become a function value");
    assert_eq!(as_value.code, "AU2999");
    assert!(as_value.message.contains("direct-call-only"));

    let construction = check_ffi_source_for_test(
        r#"
extern "C" opaque class Handle

def main():
    handle = Handle()
"#,
    )
    .expect_err("opaque handles cannot be constructed by Aura code");
    assert_eq!(construction.code, "AU2005");
    assert!(
        construction.message.contains("cannot be constructed"),
        "{}",
        construction.message
    );

    let consumed = check_ffi_source_for_test(
        r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle
extern "C" def close(handle: own Handle) -> None

def main():
    handle = acquire()
    close(handle)
    print(handle)
"#,
    )
    .expect_err("an own opaque-handle parameter must consume the handle");
    assert_eq!(consumed.code, "AU3001");
    assert!(consumed.message.contains("use of moved value `handle`"));

    let transfer = check_ffi_source_for_test(
        r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def use(handle: own Handle):
    pass

def main():
    handle = acquire()
    with group = TaskGroup():
        group.start(use, handle)
"#,
    )
    .expect_err("opaque handles are never Transfer");
    assert_eq!(transfer.code, "AU3008");
    assert!(transfer.message.contains("opaque FFI handle"));
}

#[test]
fn ffi_opaque_handles_and_containing_values_do_not_have_equality() {
    let cases = [
        (
            "direct handle",
            "Handle",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    left = acquire()
    right = acquire()
    equal = left == right
"#,
        ),
        (
            "tuple containing a handle",
            "(Handle, int64)",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    left = (acquire(), 1)
    right = (acquire(), 1)
    different = left != right
"#,
        ),
        (
            "class containing a handle",
            "Wrapper",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

class Wrapper:
    handle: Handle

def main():
    left = Wrapper(handle=acquire())
    right = Wrapper(handle=acquire())
    equal = left == right
"#,
        ),
        (
            "generic collection containing a handle",
            "list[Handle]",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    left = [acquire()]
    right = [acquire()]
    different = left != right
"#,
        ),
    ];

    for (shape, compared_type, source) in cases {
        let diagnostic = check_ffi_source_for_test(source)
            .expect_err("opaque handle identity is intentionally not observable");
        assert_eq!(diagnostic.code, "AU2008", "{shape}: {diagnostic:?}");
        assert_eq!(
            diagnostic.message,
            format!(
                "cannot compare `{compared_type}` because opaque FFI handle `Handle` does not define equality"
            ),
            "{shape}"
        );
        assert_eq!(
            diagnostic.help,
            vec![
                "compare a stable scalar or str identifier exposed by the binding instead of foreign identity"
                    .to_string()
            ],
            "{shape}"
        );
    }
}

#[test]
fn ffi_opaque_handle_equality_is_rejected_when_the_handle_is_the_right_operand() {
    for operator in ["==", "!="] {
        let source = format!(
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    handle = acquire()
    result = 0 {operator} handle
"#
        );
        let diagnostic = check_ffi_source_for_test(&source)
            .expect_err("opaque identity must remain hidden in either operand position");
        assert_eq!(diagnostic.code, "AU2008", "{operator}: {diagnostic:?}");
        assert_eq!(
            diagnostic.message,
            "cannot compare `Handle` because opaque FFI handle `Handle` does not define equality",
            "{operator}"
        );
        assert_eq!(
            diagnostic.help,
            vec![
                "compare a stable scalar or str identifier exposed by the binding instead of foreign identity"
                    .to_string()
            ],
            "{operator}"
        );
    }
}

#[test]
fn ffi_closures_that_capture_opaque_handles_use_the_callable_equality_diagnostic() {
    let diagnostic = check_ffi_source_for_test(
        r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    left_handle = acquire()
    right_handle = acquire()
    left: def() -> Handle = lambda: left_handle
    right: def() -> Handle = lambda: right_handle
    result = left == right
"#,
    )
    .expect_err("all callable equality must use the dedicated diagnostic");
    assert_eq!(diagnostic.code, "AU2008", "{diagnostic:?}");
    assert_eq!(
        diagnostic.message,
        "callable equality is not supported; compare results or use an explicit discriminant"
    );
    assert!(diagnostic.help.is_empty(), "{diagnostic:?}");
}

#[test]
fn ffi_opaque_handles_reject_pointer_arithmetic_and_address_ordering() {
    let unary = check_ffi_source_for_test(
        r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    handle = acquire()
    result = -handle
"#,
    )
    .expect_err("FFI v0 must reject unary raw pointer arithmetic");
    assert_eq!(unary.code, "AU2003", "{unary:?}");
    assert_eq!(
        unary.message,
        "opaque FFI handle `Handle` does not support unary operator `-`; FFI v0 does not expose raw pointer arithmetic"
    );
    assert_eq!(
        unary.help,
        vec![
            "declare a reviewed extern function for the native handle operation instead of manipulating its address"
                .to_string()
        ]
    );

    for operator in ["+", "-", "*", "/", "//", "%"] {
        let source = format!(
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    left = acquire()
    right = acquire()
    result = left {operator} right
"#
        );
        let diagnostic = check_ffi_source_for_test(&source)
            .expect_err("FFI v0 must reject raw pointer arithmetic");
        assert_eq!(diagnostic.code, "AU2003", "{operator}: {diagnostic:?}");
        assert_eq!(
            diagnostic.message,
            format!(
                "opaque FFI handle `Handle` does not support operator `{operator}`; FFI v0 does not expose raw pointer arithmetic"
            ),
            "{operator}"
        );
        assert_eq!(
            diagnostic.help,
            vec![
                "declare a reviewed extern function for the native handle operation instead of manipulating its address"
                    .to_string()
            ],
            "{operator}"
        );
    }

    let rhs_handle = check_ffi_source_for_test(
        r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    handle = acquire()
    result = 1 + handle
"#,
    )
    .expect_err("an opaque right operand must also receive the pointer-arithmetic diagnostic");
    assert_eq!(rhs_handle.code, "AU2003", "{rhs_handle:?}");
    assert_eq!(
        rhs_handle.message,
        "opaque FFI handle `Handle` does not support operator `+`; FFI v0 does not expose raw pointer arithmetic"
    );

    for (expression, operator) in [
        ("left < right", "<"),
        ("left <= right", "<="),
        ("left > right", ">"),
        ("left >= right", ">="),
        ("left < middle <= right", "<"),
    ] {
        let middle = expression
            .contains("middle")
            .then_some("    middle = acquire()\n")
            .unwrap_or("");
        let source = format!(
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    left = acquire()
{middle}    right = acquire()
    result = {expression}
"#
        );
        let diagnostic = check_ffi_source_for_test(&source)
            .expect_err("FFI v0 must reject ordering by a foreign address");
        assert_eq!(diagnostic.code, "AU2003", "{expression}: {diagnostic:?}");
        assert_eq!(
            diagnostic.message,
            format!(
                "opaque FFI handle `Handle` does not support operator `{operator}`; FFI v0 does not define ordering for foreign addresses"
            ),
            "{expression}"
        );
        assert_eq!(
            diagnostic.help,
            vec![
                "compare a stable scalar or str ordering key exposed by the binding instead of a foreign address"
                    .to_string()
            ],
            "{expression}"
        );
    }
}

#[test]
fn ffi_opaque_handles_are_rejected_by_reachable_clone_producing_collection_observers() {
    let cases = [
        (
            "list.copy",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    handles = [acquire()]
    copied = handles.copy()
"#,
        ),
        (
            "list.get",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    handles = [acquire()]
    copied = handles.get(0)
"#,
        ),
        (
            "list.filter",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def keep(handle: Handle) -> bool:
    return true

def main():
    handles = [acquire()]
    copied = handles.filter(keep)
"#,
        ),
        (
            "dict.copy",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    handles: dict[str, Handle] = {"one": acquire()}
    copied = handles.copy()
"#,
        ),
        (
            "dict.get",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    handles: dict[str, Handle] = {"one": acquire()}
    copied = handles.get("one")
"#,
        ),
        (
            "dict.values",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    handles: dict[str, Handle] = {"one": acquire()}
    copied = handles.values()
"#,
        ),
        (
            "dict.items",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    handles: dict[str, Handle] = {"one": acquire()}
    copied = handles.items()
"#,
        ),
    ];

    for (operation, source) in cases {
        let diagnostic = check_ffi_source_for_test(source)
            .expect_err("clone-producing collection observers must reject opaque handles");
        assert_eq!(diagnostic.code, "AU3007", "{operation}");
        assert!(
            diagnostic.message.contains(operation),
            "{operation}: {}",
            diagnostic.message
        );
        assert!(
            diagnostic.message.contains("opaque FFI handle `Handle`"),
            "{operation}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn ffi_opaque_handle_duplication_is_rejected_through_structural_and_generic_shapes() {
    let cases = [
        (
            "tuple",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    values: list[(Handle, int32)] = [(acquire(), 1)]
    copied = values.copy()
"#,
        ),
        (
            "class",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

class Wrapper:
    handle: Handle

def main():
    values = [Wrapper(handle=acquire())]
    copied = values.get(0)
"#,
        ),
        (
            "enum",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

enum Wrapper:
    Present(Handle)

def main():
    values = [Wrapper.Present(acquire())]
    copied = values.filter(lambda value: true)
"#,
        ),
        (
            "generic specialization",
            r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def duplicate[T](values: list[T]) -> list[T]:
    return values.copy()

def main():
    values = [acquire()]
    copied = duplicate(values)
"#,
        ),
    ];

    for (shape, source) in cases {
        let diagnostic = check_ffi_source_for_test(source)
            .expect_err("opaque handles must remain non-cloneable through structural shapes");
        assert_eq!(diagnostic.code, "AU3007", "{shape}");
        assert!(
            diagnostic.message.contains("opaque FFI handle `Handle`"),
            "{shape}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn ffi_opaque_handles_remain_movable_out_of_collections_without_duplication() {
    check_ffi_source_for_test(
        r#"
extern "C" opaque class Handle
extern "C" def acquire() -> Handle

def main():
    mut popped_values = [acquire()]
    popped: Handle = popped_values.pop()

    mut removed_values = [acquire()]
    removed: Handle = removed_values.pop(0)

    mut replaced_values = [acquire()]
    replaced: Handle = replaced_values.set(0, acquire())

    mut handles: dict[str, Handle] = {"one": acquire()}
    removed_from_map: Option[Handle] = handles.remove("one")
"#,
    )
    .expect("move-producing collection operations must preserve one opaque-handle owner");
}

#[test]
fn ffi_extern_metadata_supports_from_and_qualified_import_calls() {
    let remote =
        check_ffi_source_for_test("public extern \"C\" def scalar(value: int32) -> int64\n")
            .expect("remote extern declaration");
    let mut scalar = remote.extern_functions["scalar"].clone();
    scalar.module_name = "ffi_api".to_string();
    let namespace = ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "ffi_api".to_string(),
        path: "ffi_api".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: BTreeMap::new(),
        extern_functions: BTreeMap::from([("scalar".to_string(), scalar.clone())]),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::new(),
        all_extern_functions: BTreeMap::from([("scalar".to_string(), scalar.clone())]),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    };

    let qualified = crate::parse_source(
        "import ffi_api\n\ndef main():\n    value: int64 = ffi_api.scalar(7)\n",
    )
    .expect("qualified source parses");
    check_with_context(
        qualified,
        ModuleContext {
            module_name: "app".to_string(),
            imported_bindings: BTreeMap::from([(
                "ffi_api".to_string(),
                ImportedBinding::Module(namespace.clone()),
            )]),
            module_registry: BTreeMap::from([("ffi_api".to_string(), namespace.clone())]),
            is_entry_module: true,
        },
    )
    .expect("qualified extern calls should check");

    let from_import = crate::parse_source(
        "from ffi_api import scalar\n\ndef main():\n    value: int64 = scalar(7)\n",
    )
    .expect("from-import source parses");
    check_with_context(
        from_import,
        ModuleContext {
            module_name: "app".to_string(),
            imported_bindings: BTreeMap::from([(
                "scalar".to_string(),
                ImportedBinding::ExternFunction(scalar),
            )]),
            module_registry: BTreeMap::from([("ffi_api".to_string(), namespace)]),
            is_entry_module: true,
        },
    )
    .expect("from-imported extern calls should check");
}

#[test]
fn ffi_qualified_imports_do_not_expose_private_extern_declarations() {
    let remote = check_ffi_source_for_test("extern \"C\" def hidden(value: int32) -> int64\n")
        .expect("private remote extern declaration");
    let mut hidden = remote.extern_functions["hidden"].clone();
    hidden.module_name = "ffi_api".to_string();
    let namespace = ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "ffi_api".to_string(),
        path: "ffi_api".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: BTreeMap::new(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::new(),
        all_extern_functions: BTreeMap::from([("hidden".to_string(), hidden)]),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    };
    let source = crate::parse_source(
        "import ffi_api\n\ndef main():\n    value: int64 = ffi_api.hidden(7)\n",
    )
    .expect("qualified private-extern probe parses");
    let diagnostic = check_with_context(
        source,
        ModuleContext {
            module_name: "app".to_string(),
            imported_bindings: BTreeMap::from([(
                "ffi_api".to_string(),
                ImportedBinding::Module(namespace.clone()),
            )]),
            module_registry: BTreeMap::from([("ffi_api".to_string(), namespace)]),
            is_entry_module: true,
        },
    )
    .expect_err("qualified imports must not expose private extern declarations");
    assert_eq!(diagnostic.code, "AU2001");
    assert_eq!(
        diagnostic.message,
        "module `ffi_api` has no callable member `hidden`"
    );
}

#[test]
fn ffi_qualified_imports_do_not_expose_private_opaque_handles() {
    let remote = check_ffi_source_for_test("extern \"C\" opaque class Hidden\n")
        .expect("private remote opaque declaration");
    let mut hidden = remote.opaque_handles["Hidden"].clone();
    hidden.module_name = "ffi_api".to_string();
    let namespace = ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "ffi_api".to_string(),
        path: "ffi_api".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: BTreeMap::new(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::new(),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::from([("Hidden".to_string(), hidden)]),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    };
    let source =
        crate::parse_source("import ffi_api\n\ndef inspect(value: ffi_api.Hidden):\n    pass\n")
            .expect("qualified private-handle probe parses");
    let diagnostic = check_with_context(
        source,
        ModuleContext {
            module_name: "app".to_string(),
            imported_bindings: BTreeMap::from([(
                "ffi_api".to_string(),
                ImportedBinding::Module(namespace.clone()),
            )]),
            module_registry: BTreeMap::from([("ffi_api".to_string(), namespace)]),
            is_entry_module: true,
        },
    )
    .expect_err("qualified imports must not expose private opaque handles");
    assert!(
        diagnostic.message.contains("unknown type `ffi_api.Hidden`"),
        "{}",
        diagnostic.message
    );
}

#[test]
fn ffi_qualified_public_opaque_handles_are_valid_in_signatures_and_non_cloneable() {
    let namespace = public_ffi_handle_namespace("ffi_types");
    let imported_bindings = BTreeMap::from([(
        "ffi_types".to_string(),
        ImportedBinding::Module(namespace.clone()),
    )]);
    let module_registry = BTreeMap::from([("ffi_types".to_string(), namespace)]);

    let signature = crate::parse_source(
        "import ffi_types\n\nextern \"C\" def inspect(handle: ffi_types.Handle) -> int32\n",
    )
    .expect("qualified opaque-handle signature parses");
    check_with_context(
        signature,
        ModuleContext {
            module_name: "app".to_string(),
            imported_bindings: imported_bindings.clone(),
            module_registry: module_registry.clone(),
            is_entry_module: true,
        },
    )
    .expect("a public qualified opaque handle should be valid in an extern signature");

    let duplication = crate::parse_source(
        "import ffi_types\n\ndef duplicate(values: list[ffi_types.Handle]):\n    copied = values.copy()\n",
    )
    .expect("qualified opaque-handle duplication probe parses");
    let diagnostic = check_with_context(
        duplication,
        ModuleContext {
            module_name: "app".to_string(),
            imported_bindings,
            module_registry,
            is_entry_module: true,
        },
    )
    .expect_err("qualified public opaque handles must remain non-cloneable");
    assert_eq!(diagnostic.code, "AU3007");
    assert!(
        diagnostic
            .message
            .contains("opaque FFI handle `ffi_types.Handle`"),
        "{}",
        diagnostic.message
    );
}

fn type_ref(name: &str) -> TypeRef {
    TypeRef::named(name, Vec::new(), false, Span::new(1, 1))
}

#[test]
fn tuples_type_contextual_none_indexing_and_recursive_copyability() {
    crate::check_source(
        "\
def choose() -> (Option[int32], int32):
    return (None, 7)

def main():
    pair: (Option[int32], int32) = choose()
    value: int32 = pair[1]
    grouped: Option[int32] = pair[(0)]
    print(value)
",
    )
    .expect("tuple literals use element-wise expected types and copy indexing");

    assert!(Type::Tuple(vec![Type::named("int32"), Type::Unit]).is_copy());
    assert!(!Type::Tuple(vec![Type::named("str"), Type::Unit]).is_copy());
    assert_eq!(
        Type::Tuple(vec![Type::named("int32")]).to_string(),
        "(int32,)",
        "singleton tuple types must render with the comma that distinguishes their arity"
    );

    let dynamic = crate::check_source(
        "def main():\n    pair = (1, 2)\n    index = 0\n    print(pair[index])\n",
    )
    .expect_err("tuple indices must be compile-time literals");
    assert_eq!(dynamic.code, "AU2003");
    assert!(dynamic.message.contains("tuple indices must be"));

    let non_copy =
        crate::check_source("def main():\n    pair = (\"left\", 2)\n    print(pair[0])\n")
            .expect_err("non-copy tuple elements cannot be consumed by indexing");
    assert!(non_copy.message.contains("unpack the tuple"));

    let grouped_dynamic = crate::check_source(
        "def main():\n    pair = (1, 2)\n    index = 0\n    print(pair[(index)])\n",
    )
    .expect_err("a grouped non-literal is still a dynamic tuple index");
    assert_eq!(
        grouped_dynamic.message,
        "tuple indices must be non-negative integer literals"
    );
}

#[test]
fn tuple_equality_and_inequality_are_structural_and_non_consuming() {
    crate::check_source(
        r#"
def main():
    left = ("kept", ((1,), true))
    same = ("kept", ((1,), true))
    different = ("kept", ((2,), true))
    equal: bool = left == same
    unequal: bool = left != different
    still_equal: bool = left == same
"#,
    )
    .expect("tuple equality should recurse through nested elements without consuming operands");
}

#[test]
fn tuple_equality_requires_the_same_static_tuple_type() {
    for operator in ["==", "!="] {
        let source = format!(
            "def main():\n    left: (int32, str) = (1, \"same\")\n    right: (int64, str) = (1, \"same\")\n    compared = left {operator} right\n"
        );
        let error = crate::check_source(&source)
            .expect_err("bound tuples with different static element types must not be widened");
        assert_eq!(error.code, "AU2002");
        assert_eq!(
            error.message,
            "tuple equality operands must have the same type, found `(int32, str)` and `(int64, str)`"
        );
    }
}

#[test]
fn tuple_ordering_rejects_all_four_operators_with_the_teaching_diagnostic() {
    for operator in ["<", "<=", ">", ">="] {
        let source = format!(
            "def main():\n    left = (1, 2)\n    right = (1, 2)\n    compared = left {operator} right\n"
        );
        let error =
            crate::check_source(&source).expect_err("tuple ordering is outside the language");
        assert_eq!(error.code, "AU2003");
        assert_eq!(
            error.message,
            "tuple ordering is not supported; use `==` or `!=`, or compare tuple elements explicitly"
        );
    }
}

#[test]
fn tuple_type_helpers_preserve_generic_matching_and_recursive_storage_semantics() {
    let tuple_ref = TypeRef::tuple(
        vec![type_ref("T"), nested_type_ref("list", vec![type_ref("U")])],
        false,
        Span::new(1, 1),
    );
    let mut collected_ref_params = BTreeSet::new();
    collect_type_ref_type_params(
        &tuple_ref,
        &BTreeMap::new(),
        &mut collected_ref_params,
        false,
    );
    assert_eq!(
        collected_ref_params,
        BTreeSet::from(["T".to_string(), "U".to_string()])
    );

    let tuple_pattern = Type::Tuple(vec![Type::TypeParam("T".to_string()), Type::named("int32")]);
    let mut collected_type_params = BTreeSet::new();
    collect_type_params_from_type(&tuple_pattern, &mut collected_type_params);
    assert_eq!(collected_type_params, BTreeSet::from(["T".to_string()]));
    assert!(has_unresolved_type_params(&tuple_pattern));
    assert_eq!(type_pattern_specificity(&tuple_pattern), 2);

    let type_params = BTreeSet::from(["T".to_string()]);
    let actual = Type::Tuple(vec![Type::named("str"), Type::named("int32")]);
    let mut substitutions = HashMap::new();
    assert!(type_pattern_matches(
        &tuple_pattern,
        &actual,
        &type_params,
        &mut substitutions,
    ));
    assert_eq!(substitutions.get("T"), Some(&Type::named("str")));
    assert!(!type_pattern_matches(
        &tuple_pattern,
        &Type::named("str"),
        &type_params,
        &mut HashMap::new(),
    ));

    let not_tuple = unify_type_pattern(&tuple_pattern, &Type::named("str"), &mut HashMap::new())
        .expect_err("tuple generic patterns must reject non-tuples");
    assert_eq!(not_tuple.message, "expected `(T, int32)`, found `str`");
    let wrong_arity = unify_type_pattern(
        &tuple_pattern,
        &Type::Tuple(vec![Type::named("str")]),
        &mut HashMap::new(),
    )
    .expect_err("tuple generic patterns must reject the wrong arity");
    assert_eq!(wrong_arity.message, "expected `(T, int32)`, found `(str,)`");

    let classes = BTreeMap::from([(
        "Wrapper".to_string(),
        class_info(
            "Wrapper",
            false,
            vec![(
                "payload",
                Type::Tuple(vec![Type::named("Node"), Type::named("int32")]),
                false,
            )],
        ),
    )]);
    assert!(type_reaches_class_through_non_indirect_fields(
        &Type::named("Wrapper"),
        "Node",
        &classes,
        &mut BTreeSet::new(),
    ));
}

#[test]
fn tuples_destructure_recursively_and_consume_the_whole_non_copy_source() {
    crate::check_source(
        "\
def main():
    pair = (1, (2, 3))
    (first, (second, third)) = pair
    print(first)
    print(second)
    print(third)
",
    )
    .expect("recursive copy tuple destructuring should check");

    let moved = crate::check_source(
        "\
def main():
    pair = (\"left\", \"right\")
    (left, right) = pair
    print(pair)
",
    )
    .expect_err("unpacking a non-copy tuple consumes the whole source");
    assert!(moved.message.contains("use of moved value `pair`"));

    let wrong_shape = crate::check_source("def main():\n    (left, right, extra) = (1, 2)\n")
        .expect_err("tuple target arity must match");
    assert!(wrong_shape.message.contains("has 3 elements"));
}

#[test]
fn tuple_loop_targets_and_match_patterns_are_structural() {
    crate::check_source(
        "\
def main():
    values = [(1, 2), (3, 4)]
    for (left, right) in values:
        print(left + right)

    pair = (1, \"ready\")
    match pair:
        case (number, text):
            print(number)
            print(text)
",
    )
    .expect("tuple loop targets and shared-borrow tuple patterns should check");

    crate::check_source(
        "\
def main():
    pair = ((1, 2), 3)
    match pair:
        case ((left, right), tail):
            print(left + right + tail)
",
    )
    .expect("nested tuple patterns recursively bind every leaf");

    let mutable = crate::check_source(
        "\
def main():
    mut values = [(1, 2)]
    for (left, right) in mut values:
        pass
",
    )
    .expect_err("borrow-mut tuple targets are intentionally rejected");
    assert!(
        mutable
            .message
            .contains("`mut` tuple targets are not supported"),
        "{}",
        mutable.message
    );

    let match_mut = crate::check_source(
        "\
def main():
    mut pair = (1, 2)
    match mut pair:
        case (left, right):
            pass
",
    )
    .expect_err("match mut tuple patterns are intentionally rejected");
    assert!(match_mut
        .message
        .contains("does not support tuple patterns"));
}

#[test]
fn tuple_patterns_report_scrutinee_kind_and_expression_exhaustiveness() {
    let enum_statement = crate::check_source(
        "\
enum State:
    Ready

def main():
    state = State.Ready
    match state:
        case (_,):
            pass
",
    )
    .expect_err("an enum statement match must reject a tuple pattern");
    assert_eq!(
        enum_statement.message,
        "match over `State` expects enum variant patterns, not a tuple pattern"
    );

    let enum_expression = crate::check_source(
        "\
enum State:
    Ready

def main() -> int32:
    state = State.Ready
    return match state:
        case (_,): 1
",
    )
    .expect_err("an enum expression match must reject a tuple pattern");
    assert_eq!(
        enum_expression.message,
        "match over `State` expects enum variant patterns, not a tuple pattern"
    );

    let scalar_statement = crate::check_source(
        "\
def main():
    match true:
        case (_,):
            pass
",
    )
    .expect_err("a scalar statement match must reject a tuple pattern");
    assert_eq!(
        scalar_statement.message,
        "tuple pattern requires a tuple scrutinee, found `bool`"
    );

    let scalar_expression = crate::check_source(
        "\
def main() -> int32:
    return match true:
        case (_,): 1
",
    )
    .expect_err("a scalar expression match must reject a tuple pattern");
    assert_eq!(
        scalar_expression.message,
        "tuple pattern requires a tuple scrutinee, found `bool`"
    );

    let nested_scalar = crate::check_source(
        "\
def main():
    match (true, false):
        case ((_, _), _):
            pass
",
    )
    .expect_err("a nested tuple pattern must validate its corresponding element type");
    assert_eq!(
        nested_scalar.message,
        "tuple pattern requires a tuple scrutinee, found `bool`"
    );

    crate::check_source(
        "\
def classify(pair: (bool, bool)) -> bool:
    return match pair:
        case (true, value): value
        case _: false
",
    )
    .expect("tuple match expressions should bind element types recursively");

    let partial_expression = crate::check_source(
        "\
def classify(first: bool, second: bool) -> int32:
    return match (first, second):
        case (true, _): 1
",
    )
    .expect_err("a tuple match expression must cover its finite product");
    assert_eq!(
        partial_expression.message,
        "non-exhaustive match over `(bool, bool)`: add a covering tuple pattern or final `case _:`"
    );
}

#[test]
fn tuple_pattern_unions_preserve_nested_bool_and_enum_exhaustiveness() {
    crate::check_source(
        "\
enum Maybe:
    Some(bool)
    None

def main():
    match (true, 1):
        case (true, _):
            pass
        case (false, _):
            pass

    match ((true, false),):
        case ((true, _),):
            pass
        case ((false, _),):
            pass

    match (Maybe.Some(true), \"value\"):
        case (Maybe.Some(true), _):
            pass
        case (Maybe.Some(false), _):
            pass
        case (Maybe.None, _):
            pass
",
    )
    .expect("complementary finite tuple patterns should be exhaustive");

    let partial = crate::check_source(
        "\
def main():
    match (true, 1):
        case (true, _):
            pass
",
    )
    .expect_err("a partial tuple-pattern union must remain non-exhaustive");
    assert_eq!(
        partial.message,
        "non-exhaustive match over `(bool, int64)`: add a covering tuple pattern or final `case _:`"
    );

    let unreachable = crate::check_source(
        "\
def main():
    match (true, 1):
        case (true, _):
            pass
        case (false, _):
            pass
        case _:
            pass
",
    )
    .expect_err("a wildcard after a complete tuple-pattern union must remain unreachable");
    assert_eq!(unreachable.message, "unreachable match arm");

    let union_covered_subset = crate::check_source(
        "\
def classify(first: bool, second: bool):
    match (first, second):
        case (true, _):
            pass
        case (false, true):
            pass
        case (_, true):
            print(\"unreachable\")
        case (false, false):
            pass
",
    )
    .expect_err("a tuple arm covered by the union of earlier rows must be unreachable");
    assert_eq!(union_covered_subset.message, "unreachable match arm");

    let nested_union_covered_subset = crate::check_source(
        "\
enum Maybe:
    Some(bool)
    None

def classify(value: Maybe, flag: bool):
    match (value, flag):
        case (Maybe.Some(true), _):
            pass
        case (Maybe.Some(false), true):
            pass
        case (Maybe.Some(_), true):
            print(\"unreachable\")
        case (Maybe.Some(false), false):
            pass
        case (Maybe.None, _):
            pass
",
    )
    .expect_err(
        "correlated earlier rows must cover a current tuple arm through nested enum patterns",
    );
    assert_eq!(nested_union_covered_subset.message, "unreachable match arm");

    let nested_wildcard_union = crate::check_source(
        "\
def classify(left: bool, right: bool, flag: bool):
    match ((left, right), flag):
        case (_, true):
            pass
        case (_, false):
            pass
        case ((true, _), _):
            print(\"unreachable\")
",
    )
    .expect_err("wildcard rows over a nested tuple domain must participate in union reachability");
    assert_eq!(nested_wildcard_union.message, "unreachable match arm");

    let variant_wildcard_union = crate::check_source(
        "\
enum Flags:
    Pair(bool, bool)

def classify(flags: Flags):
    match flags:
        case Flags.Pair(_, true):
            pass
        case Flags.Pair(_, false):
            pass
        case Flags.Pair(true, _):
            print(\"unreachable\")
",
    )
    .expect_err("wildcard rows over multi-payload variants must participate in union reachability");
    assert_eq!(variant_wildcard_union.message, "unreachable match arm");

    crate::check_source(
        "\
def classify(first: bool, second: bool):
    match (first, second):
        case (true, true):
            pass
        case (false, false):
            pass
        case (_, true):
            print(\"reachable for false, true\")
        case _:
            pass
",
    )
    .expect("disjoint correlated rows must not make a partially overlapping tuple arm unreachable");

    crate::check_source(
        "\
enum PairState:
    Pair(bool, bool)
    Empty

def classify(value: PairState):
    match value:
        case PairState.Pair(true, _):
            pass
        case PairState.Pair(false, _):
            pass
        case PairState.Empty:
            pass
",
    )
    .expect("complementary rows must exhaust every product position of a multi-payload variant");

    let multi_payload_union_covered_subset = crate::check_source(
        "\
enum PairState:
    Pair(bool, bool)
    Empty

def classify(value: PairState):
    match value:
        case PairState.Pair(true, _):
            pass
        case PairState.Pair(false, true):
            pass
        case PairState.Pair(_, true):
            print(\"unreachable\")
        case PairState.Pair(false, false):
            pass
        case PairState.Empty:
            pass
",
    )
    .expect_err(
        "correlated earlier rows must make a covered multi-payload variant arm unreachable",
    );
    assert_eq!(
        multi_payload_union_covered_subset.message,
        "unreachable match arm"
    );
}

#[test]
fn d4_string_indexing_remains_rejected() {
    let error = crate::check_source(
        "def index_text() -> str:\n    text = 'Aura'\n    return text[0]\n\ndef main() -> int32:\n    return 0\n",
    )
    .expect_err("Aura does not define integer str indexing");
    assert_eq!(
        error.message,
        "cannot index non-Array, list, or dict value `str`"
    );
}

#[test]
fn conditional_expressions_require_bool_unify_arms_and_merge_branch_moves() {
    crate::check_source(
        r#"
def choose(flag: bool, count: int32) -> int32:
    return count if flag else 2

def ratio(flag: bool) -> float64:
    return 1 if flag else 2.5

def maybe(flag: bool) -> Option[int32]:
    return None if flag else Option.Some(7)
"#,
    )
    .expect("expected types should flow into both conditional arms");

    let condition_error = crate::check_source("def main():\n    print(\"yes\" if 1 else \"no\")\n")
        .expect_err("conditional conditions must be exactly bool");
    assert_eq!(condition_error.code, "AU2002");
    assert_eq!(
        condition_error.message,
        "conditional expression condition must have type `bool`, found `int64`"
    );
    assert_eq!(
        condition_error.help,
        ["Aura has no implicit truthiness; compare the value explicitly"]
    );

    let arm_error = crate::check_source("def main():\n    print(1 if true else \"no\")\n")
        .expect_err("conditional arms must have one static type");
    assert_eq!(arm_error.code, "AU2002");
    assert_eq!(
        arm_error.message,
        "conditional expression arms must have one type; expected `int64`, found `str`"
    );

    let move_error = crate::check_source(
        r#"
def take(value: own str) -> str:
    return value

def main():
    text = "aura"
    selected = take(text) if true else "fallback"
    print(selected)
    print(text)
"#,
    )
    .expect_err("a value moved on one conditional path is not definitely reusable");
    assert_eq!(move_error.code, "AU3001");
    assert!(move_error.message.contains("use of moved value `text`"));
}

#[test]
fn conditional_expression_inference_is_symmetric_for_contextual_values_and_literals() {
    crate::check_source(
        r#"
def choose(
    flag: bool,
    exact_integer: int32,
    reverse_integer: int32,
    exact_float: float32,
    reverse_float: float32,
    values: own list[int32],
    reverse_values: own list[int32],
    tuple_values: own list[int32]
):
    left_empty = [] if flag else values
    right_empty = reverse_values if flag else []
    nested_empty = ([], 1) if flag else (tuple_values, 2)
    left_none = None if flag else Option.Some(exact_integer)
    right_none = Option.Some(reverse_integer) if flag else None
    left_integer = (-1) if flag else exact_integer
    right_integer = reverse_integer if flag else (-2)
    promoted_integer = 1 if flag else exact_float
    reverse_promoted_integer = reverse_float if flag else 2
    left_float = (1.5) if flag else exact_float
    right_float = reverse_float if flag else (2.5)
    same_type = exact_integer if flag else reverse_integer
"#,
    )
    .expect("either conditional arm should provide the contextual result type");

    let both_unknown = crate::check_source("def main():\n    values = [] if true else []\n")
        .expect_err("two empty conditional list arms still lack an element type");
    assert_eq!(both_unknown.code, "AU2002");
    assert!(
        both_unknown
            .message
            .contains("empty list literals require an expected `list[T]` type"),
        "{}",
        both_unknown.message
    );

    let annotated_mismatch =
        crate::check_source("def choose(flag: bool) -> int32:\n    return None if flag else 1\n")
            .expect_err("an explicit result type should diagnose the first mismatching arm");
    assert_eq!(annotated_mismatch.code, "AU2002");
    assert_eq!(
        annotated_mismatch.message,
        "conditional expression arm expects `int32`, found `None`"
    );
}

#[test]
fn conditional_defaults_reject_references_from_every_operand() {
    for (source, operand) in [
        (
            "def choose(value: bool, result: int32 = 1 if value else 2):\n    pass\n",
            "condition",
        ),
        (
            "def choose(value: int32, result: int32 = value if true else 2):\n    pass\n",
            "then arm",
        ),
        (
            "def choose(value: int32, result: int32 = 1 if true else value):\n    pass\n",
            "else arm",
        ),
    ] {
        let diagnostic = crate::check_source(source)
            .expect_err("a default must not depend on another parameter");
        assert_eq!(diagnostic.code, "AU2004", "{operand}");
        assert!(
            diagnostic.message.contains("default argument")
                && diagnostic.message.contains("parameter `value`"),
            "{operand}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn consuming_a_conditional_moves_values_from_both_possible_arms() {
    for (source, moved_name) in [
        (
            r#"
def consume(value: own str):
    pass

def exercise(flag: bool):
    left = "left"
    right = "right"
    consume(left if flag else right)
    print(left)
"#,
            "left",
        ),
        (
            r#"
def consume(value: own str):
    pass

def exercise(flag: bool):
    left = "left"
    right = "right"
    consume(left if flag else right)
    print(right)
"#,
            "right",
        ),
    ] {
        let diagnostic = crate::check_source(source)
            .expect_err("each possible owned conditional result must be treated as moved");
        assert_eq!(diagnostic.code, "AU3001", "{moved_name}");
        assert!(
            diagnostic
                .message
                .contains(&format!("use of moved value `{moved_name}`")),
            "{moved_name}: {}",
            diagnostic.message
        );
    }
}

fn assert_conditional_single_consumption(expression: &str) {
    let source = format!(
        r#"
def take(value: own str) -> str:
    return value

def choose(flag: bool, text: own str) -> str:
    selected = {expression}
    return selected
"#
    );
    crate::check_source(&source).unwrap_or_else(|diagnostic| {
        panic!(
            "each conditional path consumes `text` exactly once for `{expression}`: {}",
            diagnostic.message
        )
    });
}

#[test]
fn conditional_consumption_keeps_direct_else_arm_branch_local() {
    assert_conditional_single_consumption("take(text) if flag else text");
}

#[test]
fn conditional_consumption_keeps_direct_then_arm_branch_local() {
    assert_conditional_single_consumption("text if flag else take(text)");
}

fn assert_match_single_consumption(then_value: &str, else_value: &str) {
    let source = format!(
        r#"
def take(value: own str) -> str:
    return value

def choose(flag: bool, text: own str) -> str:
    selected = match flag:
        case true: {then_value}
        case false: {else_value}
    return selected
"#
    );
    crate::check_source(&source).unwrap_or_else(|diagnostic| {
        panic!(
            "each match path consumes `text` exactly once for `{then_value}` / `{else_value}`: {}",
            diagnostic.message
        )
    });
}

#[test]
fn match_consumption_keeps_direct_false_arm_branch_local() {
    assert_match_single_consumption("take(text)", "text");
}

#[test]
fn match_consumption_keeps_direct_true_arm_branch_local() {
    assert_match_single_consumption("text", "take(text)");
}

#[test]
fn borrowed_conditional_result_preserves_internal_arm_consumption() {
    let diagnostic = crate::check_source(
        r#"
def consume_and_label(value: own str) -> str:
    return "used"

def observe(value: str):
    pass

def exercise(flag: bool, spent: own str):
    observe(consume_and_label(spent) if flag else "idle")
    print(spent)
"#,
    )
    .expect_err("borrowing the result must not erase consumption performed inside an arm");
    assert_eq!(diagnostic.code, "AU3001");
    assert!(diagnostic.message.contains("use of moved value `spent`"));
}

#[test]
fn borrowed_match_result_preserves_internal_arm_consumption() {
    let diagnostic = crate::check_source(
        r#"
def consume_and_label(value: own str) -> str:
    return "used"

def observe(value: str):
    pass

def exercise(flag: bool, spent: own str):
    observe(match flag:
        case true: consume_and_label(spent)
        case false: "idle"
    )
    print(spent)
"#,
    )
    .expect_err("borrowing a match result must retain consumption performed inside an arm");
    assert_eq!(diagnostic.code, "AU3001");
    assert!(diagnostic.message.contains("use of moved value `spent`"));
}

#[test]
fn composite_and_projected_arguments_conflict_with_a_retained_shared_argument() {
    for (source, consumed) in [
        (
            "def hold(shared: list[int32], taken: own (list[int32], int32)):\n    pass\n\ndef exercise(values: own list[int32]):\n    hold(values, (values, 1))\n",
            "values",
        ),
        (
            "def hold(shared: list[int32], taken: own list[list[int32]]):\n    pass\n\ndef exercise(values: own list[int32]):\n    hold(values, [values])\n",
            "values",
        ),
        (
            "def hold(shared: str, taken: own set[str]):\n    pass\n\ndef exercise(text: own str):\n    hold(text, {text})\n",
            "text",
        ),
        (
            "def hold(shared: str, taken: own dict[str, str]):\n    pass\n\ndef exercise(text: own str, other: own str):\n    hold(text, {other: text})\n",
            "text",
        ),
        (
            "class Holder:\n    text: str\n\ndef hold(shared: str, taken: own str):\n    pass\n\ndef exercise(flag: bool, left: own Holder, right: own Holder):\n    hold(left.text, (left if flag else right).text)\n",
            "left.text",
        ),
        (
            "class Holder:\n    text: str\n\ndef hold(shared: str, taken: own str):\n    pass\n\ndef exercise(flag: bool, left: own Holder, right: own Holder):\n    hold(left.text, (match flag:\n        case true: left\n        case false: right\n    ).text)\n",
            "left.text",
        ),
        (
            "class Holder:\n    text: str\n\nenum Choice:\n    First\n    Second\n\ndef hold(shared: str, taken: own str):\n    pass\n\ndef exercise(choice: own Choice, left: own Holder, right: own Holder):\n    hold(left.text, (match choice:\n        case Choice.First: left\n        case Choice.Second: right\n    ).text)\n",
            "left.text",
        ),
        (
            "def hold(shared: list[int32], taken: own list[list[int32]]):\n    pass\n\ndef exercise(values: own list[int32]):\n    hold(values, ([values]))\n",
            "values",
        ),
    ] {
        let diagnostic = crate::check_source(source).expect_err(
            "a place reached through a composite or branch argument must conflict with a retained shared argument",
        );
        assert_eq!(diagnostic.code, "AU3002", "{source}");
        assert!(
            diagnostic.message
                == format!(
                    "cannot consume `{consumed}` while `{consumed}` remains shared-borrowed by the parameter `shared`"
                ),
            "{source}: {}",
            diagnostic.message
        );
        assert_eq!(diagnostic.secondary_spans.len(), 1, "{source}");
        assert_eq!(
            diagnostic.secondary_spans[0].label,
            "shared access for the parameter `shared` begins here",
            "{source}",
        );
    }
}

#[test]
fn conditional_inference_exchanges_set_and_map_literal_shapes() {
    let output = crate::run_source(
        r#"
def main():
    flag = true
    numbers: set[int32] = {1, 2} if flag else {3}
    lookup: dict[str, int32] = {"a": 1} if flag else {"b": 2}
    print(numbers.len())
    print(lookup.len())
"#,
    )
    .expect("peer set and dict literal arms should infer one result type");
    assert_eq!(output.stdout, "2\n1\n");

    let grouped = crate::run_source(
        r#"
def main():
    flag = false
    value: float64 = 1 if flag else (2.0)
    print(value)
"#,
    )
    .expect("a grouped floating arm should still widen the integer arm");
    assert_eq!(grouped.stdout, "2.0\n");

    // Without an annotation the arms must agree through their own shapes.
    let unannotated = crate::run_source(
        r#"
def main():
    flag = true
    scale = 1.5
    pair = ([1], 7) if flag else ([2], 8)
    lists = [1, 2] if flag else [3, 4]
    numbers = {1, 2} if flag else {3, 4}
    lookup = {"a": 1} if flag else {"b": 2}
    widened = scale if flag else ((2.0))
    print(pair[1])
    print(lists.len())
    print(numbers.len())
    print(lookup.len())
    print(widened)
"#,
    )
    .expect("peer tuple, list, set, and dict arms should infer one result type");
    assert_eq!(unannotated.stdout, "7\n2\n2\n1\n1.5\n");
}

#[test]
fn a_projected_branch_result_field_is_consumed_from_every_arm() {
    let output = crate::run_source(
        r#"
class Holder:
    text: str

def take(value: own str) -> str:
    return value

def project(flag: bool, left: own Holder, right: own Holder) -> str:
    return take((left if flag else right).text)

def project_match(flag: bool, left: own Holder, right: own Holder) -> str:
    return take((match flag:
        case true: left
        case false: right
    ).text)

def main():
    print(project(true, Holder(text="a"), Holder(text="b")))
    print(project_match(false, Holder(text="c"), Holder(text="d")))
"#,
    )
    .expect("a field projected from a conditional or match result should be movable");
    assert_eq!(output.stdout, "a\nd\n");

    let pattern_arms = crate::run_source(
        r#"
class Holder:
    text: str

enum Choice:
    First
    Second

def take(value: own str) -> str:
    return value

def keep(holder: own Holder) -> str:
    return holder.text

def project_enum(choice: own Choice, left: own Holder, right: own Holder) -> str:
    return take((match choice:
        case Choice.First: left
        case Choice.Second: right
    ).text)

def choose_enum(choice: own Choice, left: own Holder, right: own Holder) -> str:
    return keep(match choice:
        case Choice.First: left
        case Choice.Second: right
    )

def main():
    print(project_enum(Choice.First, Holder(text="a"), Holder(text="b")))
    print(choose_enum(Choice.Second, Holder(text="c"), Holder(text="d")))
"#,
    )
    .expect("pattern match arms should transfer both projected fields and whole results");
    assert_eq!(pattern_arms.stdout, "a\nd\n");
}

#[test]
fn retained_access_help_names_the_conflicting_access_kind() {
    // `AU3002` fires for shared reads and pure consumption as well as for
    // mutation, so the recovery clause must name the access that actually
    // conflicts instead of always telling the caller to move "the mutation".
    for (source, access) in [
        (
            r#"
def id_s(s: own str) -> str:
    return s

def use_both(text: str, owned: own str) -> None:
    print(text)
    print(owned)

def main() -> None:
    value = "hello"
    use_both(value, id_s(value))
"#,
            "consumption",
        ),
        (
            // An operator whose receiver is consumed, with the right operand
            // reading a projection of that same place.
            r#"
trait Add[Rhs, Out]:
    def add(own self, rhs: own Rhs) -> Out

class Box:
    value: int32

impl Add[int32, Box] for Box:
    def add(own self, rhs: own int32) -> Box:
        return Box(value=self.value + rhs)

def main():
    box = Box(value=1)
    result = box + box.value
    print(result.value)
"#,
            "read",
        ),
        (
            r#"
def replace_and_return(value: mut str) -> str:
    value = "B"
    return "C"

def main():
    mut value: str = "A"
    print(value + replace_and_return(value))
"#,
            "mutation",
        ),
    ] {
        let rejected = crate::check_source(source).expect_err("the overlap must be rejected");
        assert_eq!(rejected.code, "AU3002", "{source}");
        assert_eq!(
            rejected.help,
            vec![format!(
                "call `.clone()` before the expression when an independent value is intended, or perform the {access} in a separate statement first"
            )],
            "{source}"
        );
    }
}

#[test]
fn indexed_read_guidance_follows_the_element_clone_safety() {
    // A clone-safe element keeps the explicit cloned-read guidance.
    for (source, message) in [
        (
            "def main():\n    values: list[str] = [\"one\"]\n    taken = values[0]\n    print(taken)\n",
            "cannot implicitly copy `str` out of a list index; use `get(index)` for an explicit cloned read instead",
        ),
        (
            "def main():\n    mut values = dict[str, str]()\n    values[\"a\"] = \"b\"\n    taken = values[\"a\"]\n    print(taken)\n",
            "cannot implicitly copy `str` out of a dict index; use `get(key)` for an explicit cloned optional read, or `remove(key)` to transfer ownership",
        ),
    ] {
        let rejected = crate::check_source(source)
            .expect_err("a non-copy indexed read must be rejected");
        assert_eq!(rejected.code, "AU3005", "{source}");
        assert_eq!(rejected.message, message, "{source}");
    }

    // A value that contains non-cloneable state must not be sent to `get`,
    // whose own clone would be rejected with AU3007. The guidance names the
    // transfer that actually works.
    for (source, message) in [
        (
            "import random\n\ndef main():\n    mut generators = list[random.Rng]()\n    generators.append(random.Rng(seed=1))\n    chosen = generators[0]\n    print(chosen.next_float())\n",
            "cannot implicitly copy `random.Rng` out of a list index; `get(index)` cannot clone it because `random.Rng` is directly non-cloneable, so use `pop(index)` to transfer ownership instead",
        ),
        (
            "import random\n\nclass Holder:\n    generator: random.Rng\n\ndef main():\n    mut holders = dict[str, Holder]()\n    holders[\"a\"] = Holder(generator=random.Rng(seed=1))\n    chosen = holders[\"a\"]\n    print(chosen.generator.next_float())\n",
            "cannot implicitly copy `Holder` out of a dict index; `get(key)` cannot clone it because `Holder` contains non-cloneable `random.Rng` state, so use `remove(key)` to transfer ownership instead",
        ),
    ] {
        let rejected = crate::check_source(source)
            .expect_err("an RNG-containing indexed read must be rejected");
        assert_eq!(rejected.code, "AU3005", "{source}");
        assert_eq!(rejected.message, message, "{source}");
    }

    // An unresolved element cannot be proven clone-safe, so the guidance says
    // what `get` would require and still offers the transfer.
    for (source, message) in [
        (
            "def first[T](values: list[T]) -> T:\n    return values[0]\n\ndef main():\n    print(\"ok\")\n",
            "cannot implicitly copy `T` out of a list index; `get(index)` requires a clone-safe `T`, or use `pop(index)` to transfer ownership",
        ),
        (
            "def lookup[V](values: dict[str, V], key: str) -> V:\n    return values[key]\n\ndef main():\n    print(\"ok\")\n",
            "cannot implicitly copy `V` out of a dict index; `get(key)` requires a clone-safe `V`, or use `remove(key)` to transfer ownership",
        ),
        (
            "def first(values: list[Task[str]]) -> Task[str]:\n    return values[0]\n",
            "cannot implicitly copy `Task[str]` out of a list index; `get(index)` cannot clone it because that would duplicate the single observation right for task result `str`, so use `pop(index)` to transfer ownership instead",
        ),
    ] {
        let rejected = crate::check_source(source)
            .expect_err("an unresolved indexed read must be rejected");
        assert_eq!(rejected.code, "AU3005", "{source}");
        assert_eq!(rejected.message, message, "{source}");
    }

    // The recommended transfer is the operation that actually succeeds.
    let vec_transfer = crate::run_source(
        r#"
import random

def main():
    mut generators = list[random.Rng]()
    generators.append(random.Rng(seed=7))
    chosen = generators.pop(0)
    mut taken = chosen
    print(taken.next_int(lo=1, hi=10))
    print(generators.len())
"#,
    )
    .expect("`pop` transfers a non-cloneable element out of a list");
    assert_eq!(vec_transfer.stdout, "4\n0\n");

    let map_transfer = crate::run_source(
        r#"
import random

class Holder:
    generator: random.Rng

def main():
    mut holders = dict[str, Holder]()
    holders["a"] = Holder(generator=random.Rng(seed=7))
    match own holders.remove("a"):
        case Option.Some(chosen):
            mut taken = chosen
            print(taken.generator.next_int(lo=1, hi=10))
        case Option.None:
            print("none")
    print(holders.len())
"#,
    )
    .expect("`remove` transfers a non-cloneable value out of a map");
    assert_eq!(map_transfer.stdout, "4\n0\n");
}

#[test]
fn indexed_compound_assignment_guidance_follows_the_element_clone_safety() {
    for (source, message) in [
        (
            "def main():\n    mut values: list[str] = [\"A\"]\n    values[0] += \"B\"\n",
            "cannot implicitly copy `str` out of a list index for compound assignment; use `get(index)` for an explicit cloned optional read, update it, then write the result back with `set(index, value)`",
        ),
        (
            "class Box:\n    value: int32\n\ndef main():\n    mut values: dict[str, Box] = {\"one\": Box(value=1)}\n    values[\"one\"] += Box(value=2)\n",
            "cannot implicitly copy `Box` out of a dict index for compound assignment; use `get(key)` for an explicit cloned optional read, or `remove(key)` to transfer ownership; update the selected value, then write it back with indexed assignment",
        ),
        (
            "import random\n\ndef main():\n    mut values: list[random.Rng] = [random.Rng(seed=1)]\n    values[0] += random.Rng(seed=2)\n",
            "cannot implicitly copy `random.Rng` out of a list index for compound assignment; `get(index)` cannot clone it because `random.Rng` is directly non-cloneable, so use `pop(index)` to transfer ownership; update the selected value, then write it back with `insert(index, value)`",
        ),
        (
            "import random\n\ndef main():\n    mut values: dict[str, random.Rng] = {\"one\": random.Rng(seed=1)}\n    values[\"one\"] += random.Rng(seed=2)\n",
            "cannot implicitly copy `random.Rng` out of a dict index for compound assignment; `get(key)` cannot clone it because `random.Rng` is directly non-cloneable, so use `remove(key)` to transfer ownership; update the selected value, then write it back with `indexed assignment`",
        ),
        (
            "def update[T](values: mut list[T], rhs: T):\n    values[0] += rhs\n",
            "cannot implicitly copy `T` out of a list index for compound assignment; `get(index)` requires a clone-safe `T`, or use `pop(index)` to transfer ownership; update the selected value, then write it back with `insert(index, value)`",
        ),
        (
            "def update[V](values: mut dict[str, V], rhs: V):\n    values[\"one\"] += rhs\n",
            "cannot implicitly copy `V` out of a dict index for compound assignment; `get(key)` requires a clone-safe `V`, or use `remove(key)` to transfer ownership; update the selected value, then write it back with `indexed assignment`",
        ),
        (
            "def update(values: mut list[Task[str]], rhs: Task[str]):\n    values[0] += rhs\n",
            "cannot implicitly copy `Task[str]` out of a list index for compound assignment; `get(index)` cannot clone it because that would duplicate the single observation right for task result `str`, so use `pop(index)` to transfer ownership; update the selected value, then write it back with `insert(index, value)`",
        ),
    ] {
        let rejected = crate::check_source(source)
            .expect_err("a non-copy indexed compound assignment must be rejected");
        assert_eq!(rejected.code, "AU3006", "{source}");
        assert_eq!(rejected.message, message, "{source}");
    }
}

#[test]
fn len_delegates_to_the_value_and_str_renders_it() {
    let output = crate::run_source(
        r#"
def main():
    mut values = list[int32]()
    values.append(1)
    values.append(2)
    mut ages = dict[str, int32]()
    ages["ada"] = 36
    mut tags = set[str]()
    tags.add("beta")
    text = "é🙂"

    print(len(values))
    print(len("hello"))
    print(len(ages))
    print(len(tags))
    print(len(text) == text.len())
    print(len(values) == values.len())
    print(len(ages) == ages.len())
    print(len(tags) == tags.len())
    print(text.len())
    print(text.byte_len())

    print(str(1))
    print(str(2.5))
    print(str(true))
    print(str(values))
    print(str(Option.Some(7)))

    rendered = str(42)
    print(len(rendered))
"#,
    )
    .expect("len should delegate and str should render");
    assert_eq!(
        output.stdout,
        "2\n5\n1\n1\ntrue\ntrue\ntrue\ntrue\n2\n6\n1\n2.5\ntrue\n[1, 2]\nOption.Some(7)\n2\n"
    );

    // `str` renders exactly what an f-string interpolation renders.
    let matches_interpolation = crate::run_source(
        r#"
def main():
    mut values = list[int32]()
    values.append(7)
    print(str(values) == f"{values}")
    print(str(2.5) == f"{2.5}")
"#,
    )
    .expect("str should match f-string rendering");
    assert_eq!(matches_interpolation.stdout, "true\ntrue\n");

    for (source, code, message) in [
        (
            "def main():\n    print(len(1))\n",
            "AU2002",
            "`len(...)` expects a value with a `len()` member, found `int64`",
        ),
        (
            "def main():\n    mut values = list[int32]()\n    print(len(values, values))\n",
            "AU2004",
            "`len` expects 1 argument, found 2",
        ),
    ] {
        let rejected =
            crate::check_source(source).expect_err("an invalid `len` call must be rejected");
        assert_eq!(rejected.code, code, "{source}");
        assert_eq!(rejected.message, message, "{source}");
    }

    // Both names are builtin function names and cannot be redefined, the same
    // way `abs` and `print` cannot.
    for name in ["len", "str"] {
        let source = format!(
            "def {name}(value: list[int32]) -> int32:\n    return 0\n\ndef main():\n    pass\n"
        );
        let rejected = crate::check_source(&source)
            .expect_err("a builtin function name must not be redefined");
        assert_eq!(rejected.code, "AU2007", "{name}");
        assert_eq!(
            rejected.message,
            format!("`{name}` is a builtin function name and cannot be redefined"),
            "{name}"
        );
    }
}

#[test]
fn enumerate_and_zip_iterate_in_lockstep_over_the_bare_loop_default() {
    let output = crate::run_source(
        r#"
def main():
    mut names = list[str]()
    names.append("ada")
    names.append("grace")
    for index, name in enumerate(names):
        print(f"{index}:{name}")

    mut tags = set[str]()
    tags.add("beta")
    for index, tag in enumerate(tags):
        print(f"set {index}:{tag}")

    mut numbers = list[int32]()
    numbers.append(1)
    numbers.append(2)
    numbers.append(3)
    mut words = list[str]()
    words.append("one")
    words.append("two")
    for number, word in zip(numbers, words):
        print(f"{number}={word}")

    for index, value in enumerate(numbers):
        if index == 0:
            continue
        if value == 3:
            break
        print(f"skip {index}->{value}")

    print(names.len())
"#,
    )
    .expect("enumerate and zip should iterate in lockstep");
    assert_eq!(
        output.stdout,
        "0:ada\n1:grace\nset 0:beta\n1=one\n2=two\nskip 1->2\n2\n"
    );

    for (source, code, message) in [
        (
            "def main():\n    for index, value in enumerate(range(3)):\n        print(index)\n",
            "AU2002",
            "`enumerate` requires a `list[T]` or `set[T]` iterable, found `Range`",
        ),
        (
            "def main():\n    mut values = list[int32]()\n    for index, value in own enumerate(values):\n        print(index)\n",
            "AU3002",
            "`enumerate` iterates over the bare-loop shared default; write `for ... in enumerate(...):` without an ownership modifier",
        ),
        (
            "def main():\n    mut values = list[int32]()\n    for index, value in enumerate(values, values):\n        print(index)\n",
            "AU2004",
            "`enumerate` takes 1 iterable, found 2",
        ),
        (
            "def main():\n    mut values = list[int32]()\n    for first, second in zip(values):\n        print(first)\n",
            "AU2004",
            "`zip` takes 2 iterables, found 1",
        ),
        (
            "def main():\n    mut values = list[int32]()\n    for index, value in enumerate(values=values):\n        print(index)\n",
            "AU2004",
            "`enumerate` takes positional iterables only",
        ),
        (
            "def main():\n    mut values = list[int32]()\n    pairs = enumerate(values)\n    print(pairs)\n",
            "AU2005",
            "`enumerate` is a `for` loop form, not a value; write `for ... in enumerate(...):`",
        ),
    ] {
        let rejected = crate::check_source(source)
            .expect_err("an invalid enumerate or zip form must be rejected");
        assert_eq!(rejected.code, code, "{source}");
        assert_eq!(rejected.message, message, "{source}");
    }

    // The iterables stay borrowed for the whole loop, and a non-copy element
    // binding is a shared borrow rather than a move.
    let frozen = crate::check_source(
        "def main():\n    mut values = list[str]()\n    values.append(\"a\")\n    for index, value in enumerate(values):\n        values.append(value)\n",
    )
    .expect_err("mutating an iterable during a lockstep loop must be rejected");
    assert_eq!(frozen.code, "AU3002");
    assert!(
        frozen
            .message
            .contains("cannot mutate `values` while `values` is borrowed for iteration"),
        "{}",
        frozen.message
    );

    let moved = crate::check_source(
        "def take(value: own str) -> str:\n    return value\n\ndef main():\n    mut values = list[str]()\n    values.append(\"a\")\n    for index, value in enumerate(values):\n        print(take(value))\n",
    )
    .expect_err("a borrowed lockstep element must not be moved out");
    assert_eq!(moved.code, "AU3002");
    assert!(
        moved.message.contains("cannot move borrowed value `value`"),
        "{}",
        moved.message
    );

    // A user definition shadows the loop form.
    let shadowed = crate::run_source(
        r#"
def zip(left: list[int32], right: list[int32]) -> int32:
    return left.len() as int32 + right.len() as int32

def main():
    mut values = list[int32]()
    values.append(1)
    print(zip(values, values))
"#,
    )
    .expect("a user function named `zip` should shadow the loop form");
    assert_eq!(shadowed.stdout, "2\n");
}

#[test]
fn membership_and_chain_operands_are_visible_to_defaults_and_argument_reads() {
    // A default argument may not reference another parameter, wherever the
    // reference hides inside the expression.
    for (source, name) in [
        (
            "def probe(ports: list[int32], present: bool = 1 in ports) -> bool:\n    return present\n\ndef main():\n    print(probe(list[int32]()))\n",
            "ports",
        ),
        (
            "def probe(low: int32, ok: bool = 1 < low < 3) -> bool:\n    return ok\n\ndef main():\n    print(probe(2))\n",
            "low",
        ),
    ] {
        let rejected = crate::check_source(source)
            .expect_err("a default argument must not reference another parameter");
        assert_eq!(rejected.code, "AU2001", "{source}");
        assert_eq!(rejected.message, format!("unknown name `{name}`"), "{source}");
    }

    // A place read inside a membership test or a chain still conflicts with a
    // consumed argument at the same call.
    for source in [
        "def hold(taken: own list[int32], flag: bool):\n    pass\n\ndef exercise(values: own list[int32]):\n    hold(values, 1 in values)\n",
        "def hold(taken: own list[int32], flag: bool):\n    pass\n\ndef exercise(values: own list[int32], n: int32):\n    hold(values, 1 < n < values.len() as int32)\n",
    ] {
        let overlap = crate::check_source(source)
            .expect_err("reading a consumed argument inside an operand must be rejected");
        assert_eq!(overlap.code, "AU3002", "{source}");
        assert!(
            overlap
                .message
                .contains("remains reserved for consumption by the parameter `taken`"),
            "{source}: {}",
            overlap.message
        );
    }

    crate::check_source(
        r#"
class Observer[T]:
    task: Task[T]

class Nested[U]:
    observer: Observer[Result[U, U]]

def duplicate(values: list[Nested[int32]]) -> list[Nested[int32]]:
    return values.copy()
"#,
    )
    .expect(
        "a repeated generic formal inside a copy result remains repeatably observable through a nested class",
    );

    let nested = crate::check_source(
        r#"
class Observer[T]:
    task: Task[T]

class Nested[U]:
    observer: Observer[Result[U, U]]

def duplicate(values: list[Nested[str]]) -> list[Nested[str]]:
    return values.copy()
"#,
    )
    .expect_err(
        "a nested generic specialization must preserve its single task-result observation right",
    );
    assert_eq!(nested.code, "AU3009");
    assert!(
        nested
            .message
            .contains("non-repeatable task result `Result[str, str]`"),
        "{nested:?}"
    );

    let tuple = crate::check_source(
        r#"
def duplicate(values: list[(Task[str], int32)]) -> list[(Task[str], int32)]:
    return values.copy()
"#,
    )
    .expect_err("a task right nested in a tuple must remain single-consumer");
    assert_eq!(tuple.code, "AU3009");
    assert!(
        tuple.message.contains("non-repeatable task result `str`"),
        "{tuple:?}"
    );
}

#[test]
fn membership_tests_read_supported_containers_and_reject_the_rest() {
    let output = crate::run_source(
        r#"
def main():
    mut values = list[int32]()
    values.append(1)
    values.append(2)
    print(1 in values)
    print(3 in values)
    print(3 not in values)

    mut names = set[str]()
    names.add("aura")
    print("aura" in names)
    print("other" not in names)

    mut ages = dict[str, int32]()
    ages["ada"] = 36
    print("ada" in ages)
    print("bob" in ages)

    text = "hello world"
    print("world" in text)
    print("zzz" in text)

    print(values.len())
    print(text)
"#,
    )
    .expect("membership tests should read list, set, dict keys, and str substrings");
    assert_eq!(
        output.stdout,
        "true\nfalse\ntrue\ntrue\ntrue\ntrue\nfalse\ntrue\nfalse\n2\nhello world\n"
    );

    for (source, code, message) in [
        (
            "def main():\n    print(1 in 5)\n",
            "AU2003",
            "`in` requires a `list[T]`, `set[T]`, `dict[K, V]`, or `str` container, found `int64`",
        ),
        (
            "def main():\n    mut values = list[int32]()\n    print(\"x\" in values)\n",
            "AU2002",
            "`in` expects a `int32` element, found `str`",
        ),
        (
            "def main():\n    mut ages = dict[str, int32]()\n    print(1 in ages)\n",
            "AU2002",
            "`in` expects a `str` key, found `int64`",
        ),
    ] {
        let diagnostic =
            crate::check_source(source).expect_err("an invalid membership test must be rejected");
        assert_eq!(diagnostic.code, code, "{source}");
        assert_eq!(diagnostic.message, message, "{source}");
    }
}

#[test]
fn comparison_chains_evaluate_each_operand_once_and_short_circuit() {
    let output = crate::run_source(
        r#"
def trace(label: str, value: int32) -> int32:
    print(label)
    return value

def main():
    print(trace("a", 1) < trace("b", 2) <= trace("c", 3))
    print(trace("a", 5) < trace("b", 2) <= trace("c", 3))
    x: int32 = 2
    print(1 < x < 3)
    print(x < 3 < 4)
    print(1 == 1 == 1)
    print(3 > 2 >= 2)
"#,
    )
    .expect("comparison chains should evaluate each operand once and short-circuit");
    assert_eq!(
        output.stdout,
        "a\nb\nc\ntrue\na\nb\nfalse\ntrue\ntrue\ntrue\ntrue\n"
    );

    // A chain link may itself be a membership test, and either form may appear
    // inside an f-string interpolation.
    let mixed = crate::run_source(
        r#"
def main():
    mut words = list[str]()
    words.append("abc")
    text = "abc"
    needle = "a"
    print(needle in text in words)
    print(needle in text not in words)

    mut ports = list[int32]()
    ports.append(80)
    port: int32 = 80
    low: int32 = 1
    high: int32 = 1024
    print(f"open={port in ports} bounded={low <= port < high}")

    limit: int64 = 8
    print(limit < 80 in ports)
"#,
    )
    .expect("membership links and interpolated comparisons should run");
    assert_eq!(mixed.stdout, "true\nfalse\nopen=true bounded=true\ntrue\n");

    // An invalid operand is reported at its own span, wherever it appears in a
    // chain or a membership test.
    for (source, column) in [
        ("def main():\n    print(missing < 1 < 2)\n", 11),
        ("def main():\n    print(1 < missing < 2)\n", 15),
        ("def main():\n    print(1 < 2 < missing)\n", 19),
        (
            "def main():\n    mut ports = list[int32]()\n    print(missing in ports)\n",
            11,
        ),
        ("def main():\n    print(1 in missing)\n", 16),
        ("def main():\n    print(1 in missing == 2)\n", 16),
    ] {
        let unresolved = crate::check_source(source)
            .expect_err("an unresolved operand must be reported, not swallowed");
        assert_eq!(unresolved.code, "AU2001", "{source}");
        assert_eq!(unresolved.message, "unknown name `missing`", "{source}");
        assert_eq!(
            unresolved.span.map(|span| span.column),
            Some(column),
            "{source}"
        );
    }

    // A value that adopts a container's element type is still range-checked,
    // and a membership link inside a chain reports its own operand mismatch.
    for (source, code, message) in [
        (
            "def main():\n    mut small = list[int8]()\n    print(300 in small)\n",
            "AU2999",
            "integer literal `300` does not fit in `int8`",
        ),
        (
            "def main():\n    mut small = list[int8]()\n    limit: int64 = 1\n    print(limit < 300 in small)\n",
            "AU2999",
            "integer literal `300` does not fit in `int8`",
        ),
        (
            "def main():\n    text = \"abc\"\n    limit: int64 = 1\n    print(limit < 2 in text)\n",
            "AU2002",
            "`in` expects a `str` substring, found `int64`",
        ),
    ] {
        let rejected = crate::check_source(source)
            .expect_err("an out-of-range or mistyped membership value must be rejected");
        assert_eq!(rejected.code, code, "{source}");
        assert_eq!(rejected.message, message, "{source}");
    }

    let mismatch = crate::check_source("def main():\n    print(1 < 2 < true)\n")
        .expect_err("a chain link with mismatched operand types must be rejected");
    assert!(
        mismatch
            .message
            .contains("binary operator operands must match"),
        "{}",
        mismatch.message
    );
}

#[test]
fn owned_result_consumption_keeps_enum_variant_paths_out_of_place_tracking() {
    crate::check_source(
        r#"
import json
import io

enum Shape:
    Empty
    Filled(int64)

def describe(shape: own Shape) -> str:
    match shape:
        case Shape.Empty:
            return "empty"
        case Shape.Filled(size):
            return f"filled {size}"

def main():
    err: io.Error = io.Error.NotFound
    print(err)
    print(json.dumps(json.Value.Null))
    values = [json.Value.Null]
    print(json.dumps(json.Value.Array(values)))
    print(describe(Shape.Empty))
"#,
    )
    .expect("module-qualified and user enum variant paths stay consumable in every position");
}

#[test]
fn conditional_result_consumption_preserves_preexisting_moves() {
    let diagnostic = crate::check_source(
        r#"
def take(value: own str) -> str:
    return value

def exercise(
    flag: bool,
    already: own str,
    left: own str,
    right: own str
):
    saved = take(already)
    selected = left if flag else right
    print(already)
"#,
    )
    .expect_err("result-arm consumption must retain move state from before the conditional");
    assert_eq!(diagnostic.code, "AU3001");
    assert!(diagnostic.message.contains("use of moved value `already`"));
}

#[test]
fn conditional_condition_moves_are_visible_to_both_result_arms() {
    for expression in [
        "text if consume_flag(text) else \"fallback\"",
        "\"fallback\" if consume_flag(text) else text",
    ] {
        let source = format!(
            r#"
def consume_flag(value: own str) -> bool:
    return true

def exercise(text: own str):
    selected = {expression}
"#
        );
        let diagnostic = crate::check_source(&source)
            .expect_err("a condition-side move must be the baseline for both result arms");
        assert_eq!(diagnostic.code, "AU3001", "{expression}");
        assert!(
            diagnostic.message.contains("use of moved value `text`"),
            "{expression}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn conditional_result_consumption_preserves_unrelated_partial_moves() {
    crate::check_source(
        r#"
class Pair:
    left: str
    right: str

def take(value: own str) -> str:
    return value

def choose(flag: bool, pair: own Pair, fallback: own str):
    moved_left = take(pair.left)
    selected = pair.right if flag else fallback
"#,
    )
    .expect("an earlier field move must not poison a disjoint conditional result field");
}

#[test]
fn conditional_arm_still_rejects_an_internal_move_followed_by_direct_reuse() {
    let diagnostic = crate::check_source(
        r#"
def take(value: own str) -> str:
    return value

def exercise(flag: bool, text: own str):
    selected = (take(text), text) if flag else ("fallback", "other")
"#,
    )
    .expect_err("a move and later direct use on the same arm must remain invalid");
    assert_eq!(diagnostic.code, "AU3001");
    assert!(diagnostic.message.contains("use of moved value `text`"));
}

#[test]
fn conditional_arm_rejects_direct_consumption_followed_by_an_internal_move() {
    let diagnostic = crate::check_source(
        r#"
def take(value: own str) -> str:
    return value

def exercise(flag: bool, text: own str):
    selected = (text, take(text)) if flag else ("fallback", "other")
"#,
    )
    .expect_err("a direct result move followed by an internal move on one arm must be rejected");
    assert_eq!(diagnostic.code, "AU3001");
    assert!(diagnostic.message.contains("use of moved value `text`"));
}

#[test]
fn conditional_result_consumption_interleaves_nested_composite_elements() {
    for body in [
        r#"selected = (text if flag else "fallback", take(text))"#,
        r#"consume_pair((text if flag else "fallback", take(text)))"#,
    ] {
        let source = format!(
            r#"
def take(value: own str) -> str:
    return value

def consume_pair(value: own (str, str)):
    pass

def exercise(flag: bool, text: own str):
    {body}
"#
        );
        let diagnostic = crate::check_source(&source).expect_err(
            "a nested branch-result move must be visible to the following composite element",
        );
        assert_eq!(diagnostic.code, "AU3001", "{body}");
        assert!(
            diagnostic.message.contains("use of moved value `text`"),
            "{body}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn consuming_a_field_of_a_conditional_result_moves_each_possible_field() {
    for reused in ["left", "right"] {
        let source = format!(
            r#"
class Holder:
    text: str

def take(value: own str):
    pass

def exercise(flag: bool, left: own Holder, right: own Holder):
    take((left if flag else right).text)
    print({reused}.text)
"#
        );
        let diagnostic = crate::check_source(&source)
            .expect_err("each projected conditional result arm must transfer its field");
        assert_eq!(diagnostic.code, "AU3001", "{reused}");
        assert!(
            diagnostic.message.contains("use of moved field `text`")
                && diagnostic.message.contains(&format!("from `{reused}`")),
            "{reused}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn consuming_a_field_of_a_match_result_moves_each_possible_field() {
    for reused in ["left", "right"] {
        let source = format!(
            r#"
class Holder:
    text: str

def exercise(flag: bool, left: own Holder, right: own Holder):
    selected = (match flag:
        case true: left
        case false: right
    ).text
    print({reused}.text)
"#
        );
        let diagnostic = crate::check_source(&source)
            .expect_err("each projected match result arm must transfer its field");
        assert_eq!(diagnostic.code, "AU3001", "{reused}");
        assert!(
            diagnostic.message.contains("use of moved field `text`")
                && diagnostic.message.contains(&format!("from `{reused}`")),
            "{reused}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn match_arm_rejects_direct_consumption_followed_by_an_internal_move() {
    let diagnostic = crate::check_source(
        r#"
def take(value: own str) -> str:
    return value

def exercise(flag: bool, text: own str):
    selected = match flag:
        case true: (text, take(text))
        case false: ("fallback", "other")
"#,
    )
    .expect_err(
        "a direct result move followed by an internal move on one match arm must be rejected",
    );
    assert_eq!(diagnostic.code, "AU3001");
    assert!(diagnostic.message.contains("use of moved value `text`"));
}

#[test]
fn conditional_inference_recursively_unifies_peer_tuple_and_generic_literals() {
    crate::check_source(
        r#"
def choose(
    flag: bool,
    left: own list[int32],
    right: own list[int32]
):
    nested_literals = ([1], right.copy()) if flag else (left.copy(), [2])
"#,
    )
    .expect(
        "corresponding tuple and generic positions should exchange literal context recursively",
    );
}

#[test]
fn conditional_inference_combines_complementary_empty_collection_context() {
    crate::check_source(
        r#"
def choose(
    flag: bool,
    left: own list[int32],
    right: own list[int32]
):
    nested_empty = ([], right) if flag else (left, [])
"#,
    )
    .expect("each empty collection should receive context from its corresponding peer position");
}

#[test]
fn conditional_inference_recurses_through_collection_shapes() {
    crate::check_source(
        r#"
def choose(
    flag: bool,
    left: own list[int32],
    right: own list[int32]
):
    nested_list = [([], right.copy())] if flag else [(left.copy(), [])]
    nested_set = {([], right.copy())} if flag else {(left.copy(), [])}
    nested_map = {1: ([], right)} if flag else {2: (left, [])}
"#,
    )
    .expect("peer collection shapes should exchange nested contextual element types");
}

#[test]
fn conditional_inference_speculation_uses_isolated_result_replay() {
    crate::check_source(
        r#"
def take(value: own str) -> str:
    return value

def choose(
    outer: bool,
    left_flag: bool,
    right_flag: bool,
    left: own str,
    right: own str
):
    selected = take(take(left) if left_flag else left) if outer else take(take(right) if right_flag else right)
"#,
    )
    .expect("speculative arm typing must preserve exactly-once nested result consumption");
}

#[test]
fn try_consumes_each_possible_conditional_result_source() {
    for reused in ["left", "right"] {
        let source = format!(
            r#"
def observe(value: Result[str, str]):
    pass

def exercise(
    flag: bool,
    left: own Result[str, str],
    right: own Result[str, str]
) -> Result[None, str]:
    text = try (left if flag else right)
    observe({reused})
    return Result.Ok(None)
"#
        );
        let diagnostic = crate::check_source(&source)
            .expect_err("`try` must consume each possible non-copy Result source");
        assert_eq!(diagnostic.code, "AU3001", "{reused}");
        assert!(
            diagnostic
                .message
                .contains(&format!("use of moved value `{reused}`")),
            "{reused}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn conditional_operands_participate_in_retained_access_conflicts() {
    for (conditional, position) in [
        ("1 if consume_flag(box) else 0", "condition"),
        ("consume_value(box) if flag else 0", "then arm"),
        ("0 if flag else consume_value(box)", "else arm"),
    ] {
        let source = format!(
            r#"
class Box:
    value: int32

def inspect(value: Box, result: int32):
    pass

def consume_flag(value: own Box) -> bool:
    return true

def consume_value(value: own Box) -> int32:
    return value.value

def exercise(flag: bool):
    box = Box(value=1)
    inspect(box, {conditional})
"#
        );
        let diagnostic = crate::check_source(&source)
            .expect_err("a retained borrow must conflict with nested conditional consumption");
        assert_eq!(diagnostic.code, "AU3002", "{position}");
        assert!(
            diagnostic.message.contains("cannot consume")
                && diagnostic.message.contains("shared-borrowed"),
            "{position}: {}",
            diagnostic.message
        );
    }

    for (conditional, position) in [
        ("1 if box.ready else 0", "condition"),
        ("box.value if flag else 0", "then arm"),
        ("0 if flag else box.value", "else arm"),
    ] {
        let source = format!(
            r#"
class Box:
    ready: bool
    value: int32

def mutate(value: mut Box, observed: int32):
    pass

def exercise(flag: bool):
    mut box = Box(ready=true, value=1)
    mutate(box, {conditional})
"#
        );
        let diagnostic = crate::check_source(&source)
            .expect_err("a retained mutable borrow must conflict with a conditional place read");
        assert_eq!(diagnostic.code, "AU3002", "{position}");
        assert!(
            diagnostic.message.contains("cannot borrow")
                && diagnostic.message.contains("mutably borrowed"),
            "{position}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn conditional_result_places_participate_in_call_access_conflicts() {
    for call in [
        "shared_then_owned(value, value if flag else other)",
        "shared_then_owned(value if flag else other, value)",
        "owned_then_shared(value if flag else other, value)",
        "owned_then_shared(value, value if flag else other)",
    ] {
        let source = format!(
            r#"
def shared_then_owned(shared: str, consumed: own str):
    pass

def owned_then_shared(consumed: own str, shared: str):
    pass

def exercise(flag: bool, value: own str, other: own str):
    {call}
"#
        );
        let diagnostic = crate::check_source(&source)
            .expect_err("a possible conditional result move must conflict with a shared argument");
        assert_eq!(diagnostic.code, "AU3002", "{call}");
        assert!(
            (diagnostic.message.contains("cannot consume")
                && diagnostic.message.contains("shared-borrowed"))
                || (diagnostic.message.contains("cannot borrow")
                    && diagnostic.message.contains("reserved for consumption")),
            "{call}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn d3_assert_checks_exact_types_and_remains_fallthrough() {
    crate::check_source(
        r#"
def verify(ready: bool, message: str):
    assert ready
    assert ready, message

verify(true, "ready")
"#,
    )
    .expect("both assertion forms and top-level assertions should check");

    let bad_condition =
        crate::check_source("assert 1\n").expect_err("assertion conditions must be exactly bool");
    assert_eq!(
        bad_condition.message,
        "`assert` condition must have type `bool`, found `int64`"
    );
    assert_eq!(bad_condition.span, Some(Span::new(1, 1)));
    assert_eq!(
        bad_condition.help,
        ["Aura has no implicit truthiness; compare the value explicitly, for example `value != 0`"]
    );

    let bad_message = crate::check_source("assert true, 1\n")
        .expect_err("assertion messages must be exactly str");
    assert_eq!(
        bad_message.message,
        "`assert` message must have type `str`, found `int64`"
    );
    assert_eq!(bad_message.span, Some(Span::new(1, 1)));

    let missing_return = crate::check_source("def verify() -> int32:\n    assert false\n")
        .expect_err("even a constant-false assertion does not narrow control flow");
    assert!(missing_return
        .message
        .contains("function `verify` is missing a return"));

    let mixed_entry = crate::check_source("assert true\n\ndef main():\n    pass\n")
        .expect_err("top-level assertions must preserve the explicit-main carve-out");
    assert!(mixed_entry.message.contains(
        "files cannot mix top-level statements, including declarations, with an explicit `main` function"
    ));
}

#[test]
fn d3_assert_keeps_condition_effects_and_discards_lazy_message_effects() {
    let condition_move = crate::check_source(
        r#"
def consume_for_condition(value: own str) -> bool:
    return true

def main():
    text = "aura"
    assert consume_for_condition(text)
    print(text)
"#,
    )
    .expect_err("condition ownership effects must persist after the assertion");
    assert!(condition_move.message.contains("use of moved value `text`"));

    let message_observes_post_condition = crate::check_source(
        r#"
def consume_for_condition(value: own str) -> bool:
    return true

def main():
    text = "aura"
    assert consume_for_condition(text), text
"#,
    )
    .expect_err("the message must be checked from the post-condition state");
    assert!(message_observes_post_condition
        .message
        .contains("use of moved value `text`"));

    crate::check_source(
        r#"
def consume_for_message(value: own str) -> str:
    return value

def main():
    text = "aura"
    assert true, consume_for_message(text)
    print(text)
"#,
    )
    .expect("lazy message ownership effects must not leak into the fallthrough state");

    let invalid_message_move = crate::check_source(
        r#"
def combine(first: own str, second: own str) -> str:
    return first

def main():
    text = "aura"
    assert false, combine(text, text)
"#,
    )
    .expect_err("lazy messages must still reject repeated ownership transfers internally");
    assert_eq!(invalid_message_move.code, "AU2004");
    assert!(invalid_message_move.message.contains(
        "overlaps consumed argument for parameter `first`; consumed values must be exclusive"
    ));
}

#[test]
fn d6_parameter_defaults_resolve_once_from_declared_types() {
    let program = crate::check_source(
        r#"
copy class CopyBox:
    value: int32

enum Maybe[T]:
    Some(T)
    None

def modes(scalar: int32, text: str, copy_box: CopyBox, copy_enum: Maybe[int32], heap_enum: Maybe[str], owned: own str, shared: int32, mutable: mut int32):
    pass

def generic[T](value: T):
    pass

def generic_enum[T](value: Maybe[T]):
    pass

def main() -> int32:
    generic[int32](1)
    generic_enum[int32](Maybe.Some(1))
    return 0
"#,
    )
    .expect("D6 parameter conventions should resolve from declaration types");

    assert_eq!(
        program.functions["modes"].signature.param_passings,
        // ADR-0022 Q1: every bare parameter is shared, copy or not.
        vec![
            ReceiverKind::Borrow,
            ReceiverKind::Borrow,
            ReceiverKind::Borrow,
            ReceiverKind::Borrow,
            ReceiverKind::Borrow,
            ReceiverKind::Value,
            ReceiverKind::Borrow,
            ReceiverKind::BorrowMut,
        ]
    );
    assert_eq!(
        program.functions["generic"].signature.param_passings,
        vec![ReceiverKind::Borrow],
        "specializing an unresolved generic with a copy type must not change its declaration ABI"
    );
    assert_eq!(
        program.functions["generic_enum"].signature.param_passings,
        vec![ReceiverKind::Borrow],
        "copy-ness inside an unresolved generic enum is declaration-stable"
    );
}

#[test]
fn d6_qualified_imported_copy_types_resolve_through_module_namespaces() {
    let mut token = class_info(
        "Token",
        true,
        vec![("value", Type::TypeParam("T".to_string()), false)],
    );
    token.module_name = "pkg.types".to_string();
    token.decl.type_params = vec!["T".to_string()];

    let mut marker = enum_info("Marker", Some(Type::TypeParam("T".to_string())));
    marker.module_name = "pkg.types".to_string();
    marker.decl.type_params = vec!["T".to_string()];

    let mut types = namespace("pkg.types");
    types.classes.insert("Token".to_string(), token.clone());
    types.all_classes.insert("Token".to_string(), token.clone());
    types.enums.insert("Marker".to_string(), marker.clone());
    types.all_enums.insert("Marker".to_string(), marker);

    let program = check_with_context(
        crate::parser::parse(
            r#"
def return_token(value: pkg.types.Token[int32]) -> pkg.types.Token[int32]:
    return value

def return_marker(value: pkg.types.Marker[int32]) -> pkg.types.Marker[int32]:
    return value

def reuse(token: pkg.types.Token[int32], marker: pkg.types.Marker[int32]) -> int32:
    return_token(token)
    return_token(token)
    return_marker(marker)
    return_marker(marker)
    print(token.value)
    print(marker)
    return 0
"#,
        )
        .expect("qualified imported types should parse"),
        ModuleContext {
            module_name: "app".to_string(),
            imported_bindings: BTreeMap::from([(
                "types".to_string(),
                ImportedBinding::Module(types),
            )]),
            module_registry: BTreeMap::new(),
            is_entry_module: true,
        },
    )
    .expect("qualified imported copy parameters should be movable and returnable");

    // ADR-0022 Q1: a bare qualified copy parameter is shared like any other.
    assert_eq!(
        program.functions["return_token"].signature.param_passings,
        vec![ReceiverKind::Borrow]
    );
    assert_eq!(
        program.functions["return_marker"].signature.param_passings,
        vec![ReceiverKind::Borrow]
    );
}

#[test]
fn d6_implicit_borrows_are_reusable_and_consumption_teaches_own() {
    crate::check_source(
        r#"
def view(value: str):
    print(value)

def main() -> int32:
    text = "aura"
    view(text)
    view(text)
    print(text)
    return 0
"#,
    )
    .expect("a bare non-copy parameter should borrow and remain reusable");

    let error = crate::check_source(
        r#"
def sink(value: own str):
    print(value)

def broken(value: str):
    sink(value)
"#,
    )
    .expect_err("consuming an implicitly borrowed parameter should teach explicit ownership");
    assert_eq!(
        error.message,
        "parameter `value` is borrowed; declare it as `own str` to take ownership, or clone the value before consuming it"
    );

    let caller_move = crate::check_source(
        r#"
def sink(value: own str):
    print(value)

def main() -> int32:
    text = "aura"
    sink(text)
    print(text)
    return 0
"#,
    )
    .expect_err("an explicit owned parameter should consume the caller argument");
    assert!(caller_move.message.contains("use of moved value `text`"));
}

#[test]
fn d6_parameter_defaults_allow_shared_and_owned_temporaries_but_reject_borrow_mut() {
    crate::check_source(
        r#"
def shared(value: str = "shared") -> str:
    return value.clone()

def owned(value: own str = "owned") -> str:
    return value

def main() -> int32:
    print(shared())
    print(owned())
    return 0
"#,
    )
    .expect("shared and owned defaults should remain valid");

    let expected = "`mut` parameter `value` cannot have a default: the default creates a caller-invisible temporary, so mutations through it would be silently lost; require the caller to pass a value, or take the parameter as `own T` and return the result";
    for source in [
        "def invalid(value: mut int32 = 1):\n    pass\n",
        "def invalid(value: mut str = \"lost\"):\n    pass\n",
    ] {
        let error = crate::check_source(source)
            .expect_err("borrow-mut defaults must be rejected for copy and non-copy types");
        assert_eq!(error.message, expected);
    }
}

#[test]
fn d6_place_iteration_and_queue_receive_have_distinct_ownership() {
    crate::check_source(
        r#"
class Job:
    name: str

def consume(value: own Job):
    print(value.name)

def main() -> int32:
    jobs = Queue[Job]()
    for job in jobs:
        consume(job)
    values: list[Job] = [Job(name="one")]
    for value in own values:
        consume(value)
    return 0
"#,
    )
    .expect("Queue items and owned place-iteration items should arrive owned");

    let borrowed_item = crate::check_source(
        r#"
class Job:
    name: str

def consume(value: own Job):
    print(value.name)

def main() -> int32:
    values: list[Job] = [Job(name="one")]
    for value in values:
        consume(value)
    return 0
"#,
    )
    .expect_err("bare place iteration over non-copy elements should bind shared items");
    assert!(borrowed_item
        .message
        .contains("cannot move borrowed value `value`"));

    let expected = "Queue iteration receives values; each received item is already owned by the loop binding, and the Queue handle is a copy value, so ownership modifiers have nothing to modify; use the bare form `for item in queue:`";
    for mode in ["own ", "mut "] {
        let source = format!(
            "def main() -> int32:\n    jobs = Queue[int32]()\n    for item in {mode}jobs:\n        print(item)\n    return 0\n"
        );
        let error = crate::check_source(&source)
            .expect_err("Queue iteration must reject every explicit ownership modifier");
        assert_eq!(error.message, expected);
    }
}

#[test]
fn range_iteration_rejects_ownership_modifiers_and_keeps_the_bare_form() {
    crate::check_source(
        r#"
def main() -> int32:
    mut total: int64 = 0
    for value in range(0, 3):
        total += value
    return total as int32
"#,
    )
    .expect("bare Range iteration should yield copy int64 values");

    let expected = "Range iteration yields copy `int64` values, so ownership modifiers have nothing to modify or transfer; use the bare form `for item in range(...):`";
    for mode in ["mut ", "own "] {
        let source = format!(
            "def main() -> int32:\n    for item in {mode}range(0, 3):\n        print(item)\n    return 0\n"
        );
        let error = crate::check_source(&source)
            .expect_err("Range iteration must reject every ownership modifier");
        assert_eq!(error.code, "AU3004", "{mode}");
        assert_eq!(error.message, expected, "{mode}");
    }
}

#[test]
fn d6_task_captures_are_owned_while_shared_child_parameters_are_allowed() {
    crate::check_source(
        r#"
def worker(value: str):
    print(value)

def main() -> int32:
    text = "aura"
    with TaskGroup() as group:
        group.start(worker, text)
    return 0
"#,
    )
    .expect("a task may own its capture while the target observes it through a shared parameter");

    let moved_capture = crate::check_source(
        r#"
def worker(value: str):
    print(value)

def main() -> int32:
    text = "aura"
    with TaskGroup() as group:
        group.start(worker, text)
    print(text)
    return 0
"#,
    )
    .expect_err("task captures should consume non-copy caller values");
    assert!(moved_capture.message.contains("use of moved value `text`"));

    let mutable_target = crate::check_source(
        r#"
def worker(value: mut str):
    pass

def main() -> int32:
    mut text = "aura"
    with TaskGroup() as group:
        group.start(worker, text)
    return 0
"#,
    )
    .expect_err("child tasks cannot write back through their starting frame");
    assert_eq!(mutable_target.code, "AU3002");
    assert_eq!(
        mutable_target.message,
        "task starting does not support mutable parameter `value` on function `worker`; child tasks cannot write back through the starting call frame"
    );
}

#[test]
fn d6_builtin_retention_metadata_controls_observable_consumption() {
    for source in [
        "def main() -> int32:\n    mut values = list[str]()\n    text = \"owned\"\n    values.append(text)\n    print(text)\n    return 0\n",
        "def main() -> int32:\n    jobs = Queue[str]()\n    text = \"owned\"\n    jobs.put(text)\n    print(text)\n    return 0\n",
    ] {
        let error = crate::check_source(source)
            .expect_err("retaining builtins should consume non-copy arguments");
        assert!(error.message.contains("use of moved value `text`"));
    }

    crate::check_source(
        r#"
import fs

def keep_after_remove() -> int32:
    mut values = {"kept"}
    text = "kept"
    values.remove(text)
    print(text)
    return 0

def keep_after_write(file: mut fs.File, text: str):
    file.write_all(text)
    print(text)
"#,
    )
    .expect("non-retaining remove and I/O write arguments should stay reusable");

    let immutable_receiver = crate::check_source(
        "def main() -> int32:\n    values = list[int32]()\n    values.append(1)\n    return 0\n",
    )
    .expect_err("borrow-mut builtin receivers should require mutable places");
    assert_eq!(immutable_receiver.code, "AU3003");
    assert!(immutable_receiver
        .message
        .contains("method `append` requires a mutable receiver"));

    let invalid_copy_slot = crate::check_source(
        "def main() -> int32:\n    values = list[int32]()\n    values.get(index=values)\n    return 0\n",
    )
    .expect_err("ill-typed copy slots should be diagnosed before overlap analysis");
    assert!(invalid_copy_slot
        .message
        .contains("list indices must have type `int64` or a losslessly narrower integer type, found `list[int32]`"));
}

#[test]
fn random_static_surface_supports_qualified_and_imported_rng_use() {
    let qualified = crate::check_source(
        r#"
import random

def sample(rng: mut random.Rng, values: mut list[str]) -> int64:
    rng.shuffle(values=values)
    fraction: float64 = rng.next_float()
    print(fraction)
    return rng.next_int(hi=10, lo=0)

def main() -> int32:
    mut rng: random.Rng = random.Rng(seed=42)
    mut values = ["a", "b", "c"]
    print(sample(rng, values))
    chosen: int64 = random.secure_int(lo=0, hi=10)
    bytes: list[uint8] = random.secure_bytes(n=4)
    print(chosen)
    print(bytes)
    return 0
"#,
    )
    .expect("qualified random APIs should type check");
    assert_eq!(
        qualified.functions["sample"].signature.params,
        vec![
            Type::named("random.Rng"),
            Type::Named("list".to_string(), vec![Type::named("str")])
        ]
    );

    let imported = crate::check_source(
        r#"
from random import Rng, secure_bytes

def make() -> Rng:
    return Rng(seed=7)

def main() -> int32:
    mut rng: Rng = make()
    print(rng.next_int(lo=-5, hi=6))
    bytes: list[uint8] = secure_bytes(n=0)
    print(bytes)
    return 0
"#,
    )
    .expect("from-imported Rng should remain a single constructible type binding");
    assert_eq!(
        imported.functions["make"].signature.return_type,
        Type::named("random.Rng")
    );
}

#[test]
fn random_static_surface_rejects_wrong_types_and_immutable_places() {
    for (source, expected) in [
        (
            "import random\n\ndef main() -> int32:\n    mut rng = random.Rng(seed=true)\n    return 0\n",
            "`random.Rng` expects `int64` for `seed`, found `bool`",
        ),
        (
            "import random\n\ndef main() -> int32:\n    mut rng = random.Rng(seed=1)\n    print(rng.next_int(lo=0, hi=1.5))\n    return 0\n",
            "`next_int` expects `int64` for `hi`, found `float64`",
        ),
        (
            "import random\n\ndef main() -> int32:\n    mut rng = random.Rng(seed=1)\n    rng.shuffle(values=3)\n    return 0\n",
            "`shuffle` expects `list[T]`, found `int64`",
        ),
        (
            "import random\n\ndef main() -> int32:\n    print(random.secure_int(lo=0, hi=false))\n    return 0\n",
            "argument type mismatch for function `secure_int`: expected `int64`, found `bool`",
        ),
        (
            "import random\n\ndef main() -> int32:\n    print(random.secure_bytes(n=false))\n    return 0\n",
            "argument type mismatch for function `secure_bytes`: expected `int64`, found `bool`",
        ),
    ] {
        let error = crate::check_source(source).expect_err("wrong random API types should fail");
        assert_eq!(error.code, "AU2002", "unexpected code for `{expected}`");
        assert!(
            error.message.contains(expected),
            "expected `{expected}` in `{}`",
            error.message
        );
    }

    let immutable_rng = crate::check_source(
        "import random\n\ndef main() -> int32:\n    rng = random.Rng(seed=1)\n    print(rng.next_float())\n    return 0\n",
    )
    .expect_err("stateful Rng methods require a mutable receiver");
    assert_eq!(immutable_rng.code, "AU3003");
    assert!(immutable_rng
        .message
        .contains("method `next_float` requires a mutable receiver"));

    let immutable_values = crate::check_source(
        "import random\n\ndef main() -> int32:\n    mut rng = random.Rng(seed=1)\n    values = [1, 2, 3]\n    rng.shuffle(values=values)\n    return 0\n",
    )
    .expect_err("shuffle requires a mutable list place");
    assert_eq!(immutable_values.code, "AU3002");
    assert_eq!(
        immutable_values.message,
        "builtin method `shuffle` argument is declared `mut` and requires a mutable place"
    );

    let explicit_type_args = crate::check_source(
        "import random\n\ndef main() -> int32:\n    mut rng = random.Rng[int64](seed=1)\n    return 0\n",
    )
    .expect_err("opaque Rng construction must not accept type arguments");
    assert!(explicit_type_args
        .message
        .contains("`random.Rng` does not take explicit type arguments"));
}

#[test]
fn random_rng_is_non_copy_and_builtin_methods_cannot_be_shadowed() {
    let moved = crate::check_source(
        r#"
import random

def consume(value: own random.Rng):
    pass

def main() -> int32:
    mut rng = random.Rng(seed=1)
    consume(rng)
    print(rng.next_float())
    return 0
"#,
    )
    .expect_err("Rng ownership should move rather than copy");
    assert_eq!(moved.code, "AU3001");
    assert!(moved.message.contains("use of moved value `rng`"));

    let collision = crate::check_source(
        r#"
import random

trait CounterfeitRandom:
    def next_int(mut self, lo: int64, hi: int64) -> int64

impl CounterfeitRandom for random.Rng:
    def next_int(mut self, lo: int64, hi: int64) -> int64:
        return lo
"#,
    )
    .expect_err("traits must not shadow stateful builtin Rng members");
    assert_eq!(collision.code, "AU2006");
    assert!(collision.message.contains("random.Rng.next_int"));
}

#[test]
fn builtin_method_names_cannot_be_shadowed_on_any_builtin_target() {
    for (source, expected) in [
        (
            "trait Sized:\n    def len(self) -> int64\n\nimpl Sized for list[int32]:\n    def len(self) -> int64:\n        return 99\n",
            "list.len",
        ),
        (
            "trait Probe:\n    def contains(self, needle: str) -> bool\n\nimpl Probe for str:\n    def contains(self, needle: str) -> bool:\n        return false\n",
            "str.contains",
        ),
        (
            "trait Lookup:\n    def get(self, key: str) -> Option[str]\n\nimpl Lookup for dict[str, str]:\n    def get(self, key: str) -> Option[str]:\n        return Option.None\n",
            "dict.get",
        ),
        (
            "import fs\n\ntrait Closer:\n    def close(self) -> int64\n\nimpl Closer for fs.File:\n    def close(self) -> int64:\n        return 7\n",
            "fs.File.close",
        ),
        (
            "trait Render:\n    def to_string(self) -> str\n\nimpl Render for int32:\n    def to_string(self) -> str:\n        return \"shadowed\"\n",
            "int32.to_string",
        ),
    ] {
        let collision = crate::check_source(source)
            .expect_err("a builtin method name must not be shadowed by a trait implementation");
        assert_eq!(collision.code, "AU2006", "{source}");
        assert!(
            collision
                .message
                .contains(&format!("collides with builtin method `{expected}`")),
            "{source}: {}",
            collision.message
        );
    }

    let accepted = crate::run_source(
        r#"
trait Describe:
    def describe(self) -> str

impl Describe for list[int32]:
    def describe(self) -> str:
        return f"vec of {self.len()}"

impl Describe for str:
    def describe(self) -> str:
        return f"text of {self.len()}"

def main():
    mut values = list[int32]()
    values.append(1)
    values.append(2)
    print(values.describe())
    text = "hello"
    print(text.describe())
"#,
    )
    .expect("a trait method that does not collide keeps dispatching on a builtin target");
    assert_eq!(accepted.stdout, "vec of 2\ntext of 5\n");
}

#[test]
fn random_unavailable_secure_float_is_an_unknown_member() {
    let error = crate::check_source(
        "import random\n\ndef main() -> int32:\n    print(random.secure_float())\n    return 0\n",
    )
    .expect_err("random.secure_float is intentionally unavailable");
    assert_eq!(error.code, "AU2001");
    assert!(error
        .message
        .contains("module `random` has no callable member `secure_float`"));
}

#[test]
fn random_rng_cannot_be_publicly_cloned_through_containers_or_user_types() {
    for (label, source) in [
        (
            "direct generator",
            r#"
import random

def main() -> int32:
    mut rng = random.Rng(seed=1)
    copy = rng.clone()
    print(copy)
    return 0
"#,
        ),
        (
            "direct list element",
            r#"
import random

def main() -> int32:
    generators = [random.Rng(seed=1)]
    copies = generators.copy()
    print(copies)
    return 0
"#,
        ),
        (
            "nested user field",
            r#"
import random

class Holder:
    generator: random.Rng

def main() -> int32:
    holders = [Holder(random.Rng(seed=1))]
    copies = holders.copy()
    print(copies)
    return 0
"#,
        ),
        (
            "nested dict value",
            r#"
import random

def main() -> int32:
    generators = {"main": random.Rng(seed=1)}
    copies = generators.copy()
    print(copies)
    return 0
"#,
        ),
    ] {
        let error = crate::check_source(source)
            .expect_err("nested random.Rng values must not become publicly cloneable");
        assert_eq!(error.code, "AU3007", "unexpected code for {label}");
        assert!(
            error.message.contains("non-cloneable `random.Rng`") && error.message.contains("clone"),
            "unexpected diagnostic for {label}: {}",
            error.message
        );
    }
}

#[test]
fn random_rng_clone_producing_collection_and_task_observers_are_rejected() {
    let cases = [
        (
            "list.get",
            "import random\n\ndef observe(values: list[random.Rng]):\n    print(values.get(0))\n",
        ),
        (
            "dict.get",
            "import random\n\ndef observe(values: dict[str, random.Rng]):\n    print(values.get(\"main\"))\n",
        ),
        (
            "dict.keys",
            "import random\n\ndef observe(values: dict[random.Rng, str]):\n    print(values.keys())\n",
        ),
        (
            "dict.values",
            "import random\n\ndef observe(values: dict[str, random.Rng]):\n    print(values.values())\n",
        ),
        (
            "dict.items",
            "import random\n\ndef observe(values: dict[str, random.Rng]):\n    print(values.items())\n",
        ),
        (
            "dict.items",
            "import random\n\ndef observe(values: dict[random.Rng, str]):\n    print(values.items())\n",
        ),
        (
            "Task.result",
            "import random\n\ndef observe(task: Task[random.Rng]):\n    print(task.result())\n",
        ),
        (
            "Task.result_or_none",
            "import random\n\ndef observe(task: Task[random.Rng]):\n    print(task.result_or_none())\n",
        ),
        (
            "Task.result_or",
            "import random\n\ndef observe(task: Task[random.Rng], fallback: own random.Rng):\n    print(task.result_or(fallback))\n",
        ),
        (
            "wait_any",
            "import random\n\ndef observe(tasks: list[Task[random.Rng]]):\n    print(wait_any(tasks))\n",
        ),
        (
            "wait_all",
            "import random\n\ndef observe(tasks: list[Task[random.Rng]]):\n    print(wait_all(tasks))\n",
        ),
    ];

    for (operation, source) in cases {
        let error = crate::check_source(source)
            .expect_err("clone-producing observations of random.Rng must be rejected");
        assert_eq!(error.code, "AU3007", "unexpected code for {operation}");
        assert!(
            error.message.contains(operation)
                && error.message.contains("non-cloneable `random.Rng`"),
            "unexpected diagnostic for {operation}: {}",
            error.message
        );
    }
}

#[test]
fn random_rng_clone_safety_defers_generic_obligations_to_use_sites() {
    crate::check_source(
        r#"
def duplicate[T](values: list[T]) -> list[T]:
    return values.copy()

def main() -> int32:
    values = [1, 2]
    copies = duplicate(values)
    print(copies)
    return 0
"#,
    )
    .expect("generic clone-producing definitions must accept clone-safe instantiations");

    for (label, source) in [
        (
            "direct generic instantiation",
            r#"
import random

def duplicate[T](values: list[T]) -> list[T]:
    return values.copy()

def main() -> int32:
    values = [random.Rng(seed=1)]
    copies = duplicate(values)
    print(copies)
    return 0
"#,
        ),
        (
            "generic-to-generic propagation",
            r#"
import random

def duplicate[T](values: list[T]) -> list[T]:
    return values.copy()

def forward[T](values: list[T]) -> list[T]:
    return duplicate(values)

def main() -> int32:
    values = [random.Rng(seed=1)]
    copies = forward(values)
    print(copies)
    return 0
"#,
        ),
    ] {
        let generic = crate::check_source(source)
            .expect_err("Rng-containing generic instantiations must fail at the use site");
        assert_eq!(generic.code, "AU3007", "unexpected code for {label}");
        assert!(
            generic.message.contains("non-cloneable `random.Rng`"),
            "unexpected diagnostic for {label}: {}",
            generic.message
        );
    }

    crate::check_source(
        r#"
import random

def clone_task_handles(values: list[Task[int64]]) -> list[Task[int64]]:
    return values.copy()

def clone_queue_handles(values: list[Queue[random.Rng]]) -> list[Queue[random.Rng]]:
    return values.copy()

def pop_generator(values: mut list[random.Rng]) -> random.Rng:
    return values.pop()

def pop_first_generator(values: mut list[random.Rng]) -> random.Rng:
    return values.pop(0)

def remove_mapped_generator(values: mut dict[str, random.Rng]) -> Option[random.Rng]:
    return values.remove("main")

def shuffle_generators(rng: mut random.Rng, values: mut list[random.Rng]):
    rng.shuffle(values)
"#,
    )
    .expect("copying handles and transferring or shuffling Rng values must remain valid");

    let moved = crate::check_source(
        r#"
import random

def consume(values: own list[random.Rng]):
    pass

def main() -> int32:
    generators = [random.Rng(seed=1)]
    consume(generators)
    print(generators)
    return 0
"#,
    )
    .expect_err("a moved RNG container must not receive an unavailable clone suggestion");
    assert_eq!(moved.code, "AU3001");
    assert!(moved.help.iter().all(|help| !help.contains("`.clone()`")));
    assert!(moved.edits.is_empty());
}

#[test]
fn rng_clone_safety_seeds_inherent_associated_method_class_type_arguments() {
    let surface = r#"
class Factory[T]:
    def probe() -> int32:
        values = list[T]()
        copies = values.copy()
        print(copies)
        return 0
"#;

    crate::check_source(&format!(
        "{surface}\ndef main() -> int32:\n    print(Factory[int64].probe())\n    with group = TaskGroup():\n        group.start_soon(Factory[int64].probe)\n    return 0\n"
    ))
    .expect("an inherent associated method should use its safe class specialization");

    let error = crate::check_source(&format!(
        "import random\n{surface}\ndef main() -> int32:\n    print(Factory[random.Rng].probe())\n    return 0\n"
    ))
    .expect_err("an inherent associated method must reject an unsafe class specialization");
    assert_eq!(error.code, "AU3007");
    assert!(error.message.contains("non-cloneable `random.Rng`"));

    let task_error = crate::check_source(&format!(
        "import random\n{surface}\ndef main() -> int32:\n    with group = TaskGroup():\n        group.start_soon(Factory[random.Rng].probe)\n    return 0\n"
    ))
    .expect_err("task targets must retain an inherent associated method's class specialization");
    assert_eq!(task_error.code, "AU3007");
    assert!(task_error.message.contains("non-cloneable `random.Rng`"));
}

#[test]
fn rng_clone_safety_trait_contracts_cover_direct_and_bound_dispatch() {
    let trait_surface = r#"
trait Copier[Item]:
    def duplicate(self, values: list[Item]) -> list[Item]:
        return values.copy()

class Wrapper[Element]:
    marker: Element

impl[Element] Copier[Element] for Wrapper[Element]:
    pass

def through[Value, C: Copier[Value]](copier: C, values: list[Value]) -> list[Value]:
    return copier.duplicate(values)
"#;

    crate::check_source(&format!(
        "{trait_surface}\ndef main() -> int32:\n    wrapper = Wrapper(1)\n    values = [2, 3]\n    direct = wrapper.duplicate(values)\n    via_bound = through(wrapper, values)\n    print(direct)\n    print(via_bound)\n    return 0\n"
    ))
    .expect("safe concrete direct and bound-based trait dispatch should type-check");

    for (label, call) in [
        ("direct trait dispatch", "wrapper.duplicate(values)"),
        ("bound-based trait dispatch", "through(wrapper, values)"),
    ] {
        let error = crate::check_source(&format!(
            "import random\n{trait_surface}\ndef main() -> int32:\n    wrapper = Wrapper(random.Rng(seed=1))\n    values = [random.Rng(seed=2)]\n    copies = {call}\n    print(copies)\n    return 0\n"
        ))
        .expect_err("trait clone-safety contracts must reject Rng instantiations");
        assert_eq!(error.code, "AU3007", "unexpected code for {label}");
        assert!(
            error.message.contains("non-cloneable `random.Rng`"),
            "unexpected diagnostic for {label}: {}",
            error.message
        );
    }

    let strengthened = crate::check_source(
        r#"
trait Copier[T]:
    def duplicate(self) -> list[T]

class Wrapper[T]:
    values: list[T]

impl[T] Copier[T] for Wrapper[T]:
    def duplicate(self) -> list[T]:
        return self.values.copy()
"#,
    )
    .expect_err("an impl may not strengthen an abstract trait clone-safety contract");
    assert_eq!(strengthened.code, "AU3007");
    assert!(strengthened.message.contains("would strengthen"));
}

#[test]
fn rng_clone_safety_trait_method_generics_wait_for_call_inference() {
    let instance_surface = r#"
trait Copier:
    def duplicate[T](self, values: list[T]) -> list[T]:
        return values.copy()

class Marker:
    value: int64

impl Copier for Marker:
    pass
"#;

    crate::check_source(&format!(
        "{instance_surface}\ndef main() -> int32:\n    marker = Marker(0)\n    values = [1, 2]\n    copies = marker.duplicate(values)\n    print(copies)\n    return 0\n"
    ))
    .expect("instance trait dispatch should infer a clone-safe method type argument");

    let instance_rng = crate::check_source(&format!(
        "import random\n{instance_surface}\ndef main() -> int32:\n    marker = Marker(0)\n    values = [random.Rng(seed=1)]\n    copies = marker.duplicate(values)\n    print(copies)\n    return 0\n"
    ))
    .expect_err("instance trait dispatch must reject an inferred Rng method type argument");
    assert_eq!(instance_rng.code, "AU3007");
    assert!(instance_rng.message.contains("non-cloneable `random.Rng`"));

    let bound_rng = crate::check_source(&format!(
        "import random\n{instance_surface}\ndef forward[T, U, C: Copier](marker: C, values: list[U]) -> list[U]:\n    return marker.duplicate(values)\n\ndef main() -> int32:\n    marker = Marker(0)\n    values = [random.Rng(seed=1)]\n    copies = forward[int64, random.Rng, Marker](marker, values)\n    print(copies)\n    return 0\n"
    ))
    .expect_err(
        "bound dispatch must propagate the inferred method obligation to the matching caller parameter",
    );
    assert_eq!(bound_rng.code, "AU3007");
    assert!(bound_rng.message.contains("non-cloneable `random.Rng`"));

    let associated_surface = r#"
trait Factory:
    def duplicate[T](values: list[T]) -> list[T]:
        return values.copy()

class Marker:
    value: int64

impl Factory for Marker:
    pass
"#;

    crate::check_source(&format!(
        "{associated_surface}\ndef main() -> int32:\n    values = [1, 2]\n    copies = Marker.duplicate(values)\n    print(copies)\n    return 0\n"
    ))
    .expect("associated trait dispatch should infer a clone-safe method type argument");

    let associated_rng = crate::check_source(&format!(
        "import random\n{associated_surface}\ndef main() -> int32:\n    values = [random.Rng(seed=1)]\n    copies = Marker.duplicate(values)\n    print(copies)\n    return 0\n"
    ))
    .expect_err("associated trait dispatch must reject an inferred Rng method type argument");
    assert_eq!(associated_rng.code, "AU3007");
    assert!(associated_rng
        .message
        .contains("non-cloneable `random.Rng`"));
}

#[test]
fn rng_clone_safety_method_inference_preserves_general_diagnostics() {
    let surface = r#"
trait Display:
    def text(self) -> str

trait Factory:
    def choose[T](self, left: own T, right: own T) -> T:
        return left

    def empty[T](self) -> Option[T]:
        return Option.None

    def display[T: Display](self, value: own T) -> T:
        return value

class Marker:
    value: int64

class Plain:
    value: int64

impl Factory for Marker:
    pass
"#;

    let mismatch = crate::check_source(&format!(
        "{surface}\ndef main() -> int32:\n    marker = Marker(0)\n    value = marker.choose(1, \"different\")\n    return 0\n"
    ))
    .expect_err("generic trait methods must reject conflicting inferred argument types");
    assert!(
        mismatch
            .message
            .contains("argument type mismatch for method `choose`"),
        "unexpected mismatch diagnostic: {}",
        mismatch.message
    );

    let uninferred = crate::check_source(&format!(
        "{surface}\ndef main() -> int32:\n    marker = Marker(0)\n    value = marker.empty()\n    return 0\n"
    ))
    .expect_err("generic trait methods must diagnose type parameters with no inference source");
    assert!(
        uninferred
            .message
            .contains("cannot infer type parameter `T` for method `empty`"),
        "unexpected inference diagnostic: {}",
        uninferred.message
    );

    let missing_bound = crate::check_source(&format!(
        "{surface}\ndef main() -> int32:\n    marker = Marker(0)\n    value = marker.display(Plain(1))\n    return 0\n"
    ))
    .expect_err("generic trait methods must enforce inferred type-parameter bounds");
    assert!(
        missing_bound
            .message
            .contains("type `Plain` does not implement trait `Display`"),
        "unexpected bound diagnostic: {}",
        missing_bound.message
    );
}

#[test]
fn rng_clone_safety_operator_inference_preserves_general_diagnostics() {
    let conflict = crate::check_source(
        r#"
class Pair[A, B]:
    first: A
    second: B

trait Add[Rhs, Out]:
    def add[T](self, rhs: Pair[T, T]) -> Out

def combine[X: Add[Pair[int64, str], int64]](left: X, right: Pair[int64, str]) -> int64:
    return left + right
"#,
    )
    .expect_err("operator methods must reject conflicting inferred argument types");
    assert_eq!(conflict.code, "AU2002");
    assert!(
        conflict.message.contains(
            "argument type mismatch for operator trait `Add.add`: conflicting inferred types for `T`: `int64` and `str`"
        ),
        "unexpected mismatch diagnostic: {}",
        conflict.message
    );

    let uninferred = crate::check_source(
        r#"
trait Neg[Out]:
    def neg[T](self) -> Out

def invert[X: Neg[int64]](value: X) -> int64:
    return -value
"#,
    )
    .expect_err("operator methods must diagnose type parameters with no inference source");
    assert_eq!(uninferred.code, "AU2002");
    assert!(
        uninferred
            .message
            .contains("cannot infer type parameter `T` for operator trait `Neg.neg`"),
        "unexpected inference diagnostic: {}",
        uninferred.message
    );

    let missing_bound = crate::check_source(
        r#"
trait Display:
    def text(self) -> str

class Plain:
    value: int64

trait Add[Rhs, Out]:
    def add[T: Display](self, rhs: T) -> Out

def combine[X: Add[Plain, int64]](left: X, right: Plain) -> int64:
    return left + right
"#,
    )
    .expect_err("operator methods must enforce inferred type-parameter bounds");
    assert_eq!(missing_bound.code, "AU2002");
    assert!(
        missing_bound
            .message
            .contains("type `Plain` does not implement trait `Display`"),
        "unexpected bound diagnostic: {}",
        missing_bound.message
    );
}

#[test]
fn rng_clone_safety_ignores_handle_payloads_and_ambiguous_nominal_fallbacks() {
    let classes = BTreeMap::new();
    let enums = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let imported_modules = BTreeMap::new();

    assert_eq!(
        rng_clone_safety_in_context_with_modules(
            &Type::named("random.Rng"),
            &classes,
            &enums,
            &imported_modules,
            &module_registry,
        ),
        RngCloneSafety::ContainsRng,
        "the canonical builtin type remains non-cloneable in reduced checker contexts",
    );
    for handle in ["Queue", "Task"] {
        let ty = Type::Named(
            handle.to_string(),
            vec![Type::TypeParam("Payload".to_string())],
        );
        assert_eq!(
            rng_clone_safety_in_context_with_modules(
                &ty,
                &classes,
                &enums,
                &imported_modules,
                &module_registry,
            ),
            RngCloneSafety::Safe,
            "cloning a {handle} copies only its handle",
        );
        assert!(
            rng_clone_obligation_params_in_context_with_modules(
                &ty,
                &classes,
                &enums,
                &imported_modules,
                &module_registry,
            )
            .is_empty(),
            "a {handle} payload must not create clone-safety obligations",
        );
    }
    let safe_tuple = Type::Tuple(vec![Type::named("int32"), Type::Unit]);
    assert_eq!(
        rng_clone_safety_in_context_with_modules(
            &safe_tuple,
            &classes,
            &enums,
            &imported_modules,
            &module_registry,
        ),
        RngCloneSafety::Safe,
    );
    let rng_tuple = Type::Tuple(vec![Type::named("random.Rng"), Type::named("int32")]);
    assert_eq!(
        rng_clone_safety_in_context_with_modules(
            &rng_tuple,
            &classes,
            &enums,
            &imported_modules,
            &module_registry,
        ),
        RngCloneSafety::ContainsRng,
        "a tuple is only clone-safe when every element is clone-safe"
    );
    let generic_tuple = Type::Tuple(vec![
        Type::TypeParam("Element".to_string()),
        Type::named("int32"),
    ]);
    assert_eq!(
        rng_clone_safety_in_context_with_modules(
            &generic_tuple,
            &classes,
            &enums,
            &imported_modules,
            &module_registry,
        ),
        RngCloneSafety::Unknown,
    );
    assert_eq!(
        rng_clone_obligation_params_in_context_with_modules(
            &generic_tuple,
            &classes,
            &enums,
            &imported_modules,
            &module_registry,
        ),
        BTreeSet::from(["Element".to_string()]),
        "generic tuple elements propagate clone-safety obligations"
    );

    let mut first_class = class_info("Shared", false, Vec::new());
    first_class.module_name = "first".to_string();
    let mut second_class = first_class.clone();
    second_class.module_name = "second".to_string();
    let mut first_enum = enum_info("Choice", None);
    first_enum.module_name = "first".to_string();
    let mut second_enum = first_enum.clone();
    second_enum.module_name = "second".to_string();

    let mut first = namespace("first");
    first
        .classes
        .insert("Shared".to_string(), first_class.clone());
    first.enums.insert("Choice".to_string(), first_enum.clone());
    let mut duplicate = namespace("duplicate");
    duplicate
        .classes
        .insert("Shared".to_string(), first_class.clone());
    duplicate
        .enums
        .insert("Choice".to_string(), first_enum.clone());
    let same_identity = BTreeMap::from([
        ("duplicate".to_string(), duplicate),
        ("first".to_string(), first.clone()),
    ]);
    assert_eq!(
        copy_class_info_from_modules("Shared", &same_identity, &module_registry)
            .map(|info| info.module_name.as_str()),
        Some("first"),
        "the same nominal class re-exported twice is not ambiguous",
    );
    assert_eq!(
        copy_enum_info_from_modules("Choice", &same_identity, &module_registry)
            .map(|info| info.module_name.as_str()),
        Some("first"),
        "the same nominal enum re-exported twice is not ambiguous",
    );

    let mut second = namespace("second");
    second.classes.insert("Shared".to_string(), second_class);
    second.enums.insert("Choice".to_string(), second_enum);
    let ambiguous = BTreeMap::from([("first".to_string(), first), ("second".to_string(), second)]);
    assert!(
        copy_class_info_from_modules("Shared", &ambiguous, &module_registry).is_none(),
        "unqualified same-leaf classes from different modules must remain ambiguous",
    );
    assert!(
        copy_enum_info_from_modules("Choice", &ambiguous, &module_registry).is_none(),
        "unqualified same-leaf enums from different modules must remain ambiguous",
    );
}

#[test]
fn rng_clone_safety_terminates_for_expanding_recursive_generics() {
    let surface = r#"
class Grow[T]:
    next: indirect Grow[list[T]]
    value: T

def duplicate[T](values: list[Grow[T]]) -> list[Grow[T]]:
    return values.copy()
"#;

    crate::check_source(&format!(
        "{surface}\ndef accept_safe(values: list[Grow[int64]]) -> list[Grow[int64]]:\n    return duplicate(values)\n\ndef main() -> int32:\n    return 0\n"
    ))
    .expect("an expanding recursive generic clone obligation should terminate and accept a safe concrete instantiation");

    let error = crate::check_source(&format!(
        "import random\n{surface}\ndef reject(values: list[Grow[random.Rng]]) -> list[Grow[random.Rng]]:\n    return duplicate(values)\n"
    ))
    .expect_err("the recursive generic must still reject an Rng instantiation");
    assert_eq!(error.code, "AU3007");
    assert!(error.message.contains("non-cloneable `random.Rng`"));
}

#[test]
fn rng_clone_safety_defers_obligations_through_generic_enum_payloads() {
    let surface = r#"
enum Envelope[T]:
    Empty
    Value(T)

def duplicate[T](values: list[Envelope[T]]) -> list[Envelope[T]]:
    return values.copy()
"#;

    crate::check_source(&format!(
        "{surface}\ndef accept(values: list[Envelope[int64]]) -> list[Envelope[int64]]:\n    return duplicate(values)\n"
    ))
    .expect("an enum payload obligation should accept a clone-safe specialization");

    let error = crate::check_source(&format!(
        "import random\n{surface}\ndef reject(values: list[Envelope[random.Rng]]) -> list[Envelope[random.Rng]]:\n    return duplicate(values)\n"
    ))
    .expect_err("an enum payload obligation must reject an Rng specialization");
    assert_eq!(error.code, "AU3007");
    assert!(error.message.contains("non-cloneable `random.Rng`"));
    assert!(error.message.contains("duplicate"));
}

#[test]
fn rng_clone_safety_terminates_for_recursive_generic_enums() {
    let surface = r#"
enum Chain[T]:
    End(T)
    Link(indirect Chain[list[T]])

def duplicate[T](values: list[Chain[T]]) -> list[Chain[T]]:
    return values.copy()
"#;

    crate::check_source(&format!(
        "{surface}\ndef accept(values: list[Chain[int64]]) -> list[Chain[int64]]:\n    return duplicate(values)\n"
    ))
    .expect("a recursive enum obligation should terminate for a safe specialization");

    let error = crate::check_source(&format!(
        "import random\n{surface}\ndef reject(values: list[Chain[random.Rng]]) -> list[Chain[random.Rng]]:\n    return duplicate(values)\n"
    ))
    .expect_err("a recursive enum obligation must terminate and reject an Rng specialization");
    assert_eq!(error.code, "AU3007");
    assert!(error.message.contains("non-cloneable `random.Rng`"));
}

#[test]
fn qualified_imported_generic_constructors_preserve_substitutions_and_nominal_identity() {
    let mut remote_holder = class_info(
        "Holder",
        false,
        vec![("value", Type::TypeParam("T".to_string()), false)],
    );
    remote_holder.module_name = "pkg.remote".to_string();
    remote_holder.decl.type_params = vec!["T".to_string()];

    let mut remote_envelope = enum_info("Envelope", Some(Type::TypeParam("T".to_string())));
    remote_envelope.module_name = "pkg.remote".to_string();
    remote_envelope.decl.type_params = vec!["T".to_string()];

    let mut remote = namespace("pkg.remote");
    remote
        .classes
        .insert("Holder".to_string(), remote_holder.clone());
    remote
        .all_classes
        .insert("Holder".to_string(), remote_holder);
    remote
        .enums
        .insert("Envelope".to_string(), remote_envelope.clone());
    remote
        .all_enums
        .insert("Envelope".to_string(), remote_envelope);
    let random = crate::builtin_modules::builtin_module_namespace(&["random".to_string()])
        .expect("random namespace should exist");

    let context = || ModuleContext {
        module_name: "app".to_string(),
        imported_bindings: BTreeMap::from([
            (
                "remote".to_string(),
                ImportedBinding::Module(remote.clone()),
            ),
            (
                "random".to_string(),
                ImportedBinding::Module(random.clone()),
            ),
        ]),
        module_registry: BTreeMap::from([
            ("pkg.remote".to_string(), remote.clone()),
            ("random".to_string(), random.clone()),
        ]),
        is_entry_module: true,
    };

    check_with_context(
        crate::parser::parse(
            r#"
class Holder[T]:
    value: T

enum Envelope[T]:
    Value(T)

def accept():
    local_holder = Holder[random.Rng](random.Rng(seed=1))
    remote_holder: pkg.remote.Holder[random.Rng] = remote.Holder[random.Rng](random.Rng(seed=2))
    local_envelope: Envelope[random.Rng] = Envelope.Value(random.Rng(seed=3))
    remote_envelope: pkg.remote.Envelope[random.Rng] = remote.Envelope.Value(random.Rng(seed=4))
    print(local_holder)
    print(remote_holder)
    print(local_envelope)
    print(remote_envelope)
"#,
        )
        .expect("same-leaf generic constructors should parse"),
        context(),
    )
    .expect("qualified imported generic constructors should honor their concrete substitutions");

    for (label, source) in [
        (
            "class field",
            "def reject():\n    value = remote.Holder[int64](random.Rng(seed=1))\n",
        ),
        (
            "enum payload",
            "def reject():\n    value: pkg.remote.Envelope[int64] = remote.Envelope.Value(random.Rng(seed=1))\n",
        ),
    ] {
        let error = check_with_context(
            crate::parser::parse(source).expect("mismatched constructor should parse"),
            context(),
        )
        .expect_err("explicit imported generic substitutions must reject mismatched values");
        assert!(
            error.message.contains("expects `int64`, found `random.Rng`"),
            "unexpected diagnostic for {label}: {}",
            error.message
        );
    }
}

#[test]
fn qualified_imported_generic_constructor_inference_and_substituted_bounds_are_enforced() {
    let mut accepts = trait_info("Accepts", vec!["Other"]);
    accepts.module_name = "contracts".to_string();

    let mut phantom = class_info(
        "Phantom",
        false,
        vec![("marker", Type::named("int64"), false)],
    );
    phantom.module_name = "pkg.remote".to_string();
    phantom.decl.type_params = vec!["T".to_string()];

    let mut phantom_choice = enum_info("PhantomChoice", Some(Type::named("int64")));
    phantom_choice.module_name = "pkg.remote".to_string();
    phantom_choice.decl.type_params = vec!["T".to_string()];

    let bound = TraitBound {
        trait_name: "Accepts".to_string(),
        trait_args: vec![Type::TypeParam("A".to_string())],
    };
    let mut bounded = class_info(
        "Bounded",
        false,
        vec![
            ("first", Type::TypeParam("A".to_string()), false),
            ("second", Type::TypeParam("B".to_string()), false),
        ],
    );
    bounded.module_name = "pkg.remote".to_string();
    bounded.decl.type_params = vec!["A".to_string(), "B".to_string()];
    bounded
        .type_param_bounds
        .insert("B".to_string(), vec![bound.clone()]);

    let mut bounded_choice = enum_info("BoundedChoice", Some(Type::TypeParam("B".to_string())));
    bounded_choice.module_name = "pkg.remote".to_string();
    bounded_choice.decl.type_params = vec!["A".to_string(), "B".to_string()];
    bounded_choice
        .type_param_bounds
        .insert("B".to_string(), vec![bound]);

    let mut remote = namespace("pkg.remote");
    for (name, info) in [("Phantom", phantom), ("Bounded", bounded)] {
        remote.classes.insert(name.to_string(), info.clone());
        remote.all_classes.insert(name.to_string(), info);
    }
    for (name, info) in [
        ("PhantomChoice", phantom_choice),
        ("BoundedChoice", bounded_choice),
    ] {
        remote.enums.insert(name.to_string(), info.clone());
        remote.all_enums.insert(name.to_string(), info);
    }

    let context = || ModuleContext {
        module_name: "app".to_string(),
        imported_bindings: BTreeMap::from([
            (
                "remote".to_string(),
                ImportedBinding::Module(remote.clone()),
            ),
            (
                "Accepts".to_string(),
                ImportedBinding::Trait(accepts.clone()),
            ),
        ]),
        module_registry: BTreeMap::from([("pkg.remote".to_string(), remote.clone())]),
        is_entry_module: true,
    };
    let local_surface = r#"
class Accepted:
    marker: int64

impl Accepts[int64] for Accepted:
    pass
"#;

    check_with_context(
        crate::parser::parse(&format!(
            "{local_surface}\ndef accept():\n    pair = remote.Bounded[int64, Accepted](1, Accepted(2))\n    choice = remote.BoundedChoice[int64, Accepted].Value(Accepted(3))\n    print(pair)\n    print(choice)\n"
        ))
        .expect("bounded imported constructors should parse"),
        context(),
    )
    .expect("substituted imported class and enum bounds should accept a matching impl");

    for (label, statement, expected) in [
        (
            "phantom class parameter",
            "value = remote.Phantom(1)",
            "cannot infer type parameter `T` for class constructor `pkg.remote.Phantom`",
        ),
        (
            "phantom enum parameter",
            "value = remote.PhantomChoice.Value(1)",
            "cannot infer type parameter `T` for enum variant `PhantomChoice.Value`",
        ),
        (
            "substituted class bound",
            "value = remote.Bounded[str, Accepted](\"x\", Accepted(1))",
            "does not implement trait `Accepts[str]`",
        ),
        (
            "substituted enum bound",
            "value = remote.BoundedChoice[str, Accepted].Value(Accepted(1))",
            "does not implement trait `Accepts[str]`",
        ),
    ] {
        let source = format!("{local_surface}\ndef reject():\n    {statement}\n");
        let error = check_with_context(
            crate::parser::parse(&source).expect("constructor diagnostic case should parse"),
            context(),
        )
        .expect_err("the imported constructor should report the requested diagnostic");
        assert!(
            error.message.contains(expected),
            "unexpected diagnostic for {label}: {}",
            error.message
        );
    }
}

#[test]
fn rng_clone_safety_covers_associated_operator_and_specialized_trait_routes() {
    let associated = r#"
trait Factory[Item]:
    def duplicate(values: list[Item]) -> list[Item]:
        return values.copy()

class Wrapper[Element]:
    marker: Element

impl[Element] Factory[Element] for Wrapper[Element]:
    pass
"#;
    crate::check_source(&format!(
        "{associated}\ndef main() -> int32:\n    values = [1, 2]\n    copies = Wrapper[int64].duplicate(values)\n    print(copies)\n    return 0\n"
    ))
    .expect("a safe associated default-trait instantiation should type-check");
    let associated_rng = crate::check_source(&format!(
        "import random\n{associated}\ndef main() -> int32:\n    values = [random.Rng(seed=1)]\n    copies = Wrapper[random.Rng].duplicate(values)\n    print(copies)\n    return 0\n"
    ))
    .expect_err("associated trait dispatch must enforce clone-safety obligations");
    assert_eq!(associated_rng.code, "AU3007");

    let specialized_safe = crate::check_source(
        r#"
trait Copier[Item]:
    def duplicate(self, values: list[Item]) -> list[Item]:
        return values.copy()

class Fixed:
    value: int64

impl Copier[int64] for Fixed:
    pass

def main() -> int32:
    fixed = Fixed(1)
    values = [2, 3]
    print(fixed.duplicate(values))
    return 0
"#,
    )
    .expect("a concrete safe inherited default method should type-check");
    assert!(specialized_safe.trait_impls[0]
        .methods
        .contains_key("duplicate"));

    let specialized_rng = crate::check_source(
        r#"
import random

trait Copier[Item]:
    def duplicate(self, values: list[Item]) -> list[Item]:
        return values.copy()

class Fixed:
    value: int64

impl Copier[random.Rng] for Fixed:
    pass
"#,
    )
    .expect_err("a concrete Rng specialization cannot satisfy the default contract");
    assert_eq!(specialized_rng.code, "AU3007");
    assert!(specialized_rng.message.contains("cannot satisfy"));

    let operator = r#"
trait Add[Item, Out]:
    def add(self, rhs: own Item) -> list[Item]:
        values = [rhs]
        return values.copy()

class Wrapper[Element]:
    marker: Element

impl[Element] Add[Element, list[Element]] for Wrapper[Element]:
    pass
"#;
    crate::check_source(&format!(
        "{operator}\ndef main() -> int32:\n    wrapper = Wrapper(1)\n    print(wrapper + 2)\n    return 0\n"
    ))
    .expect("safe operator dispatch should type-check");
    let operator_rng = crate::check_source(&format!(
        "import random\n{operator}\ndef main() -> int32:\n    wrapper = Wrapper(random.Rng(seed=1))\n    print(wrapper + random.Rng(seed=2))\n    return 0\n"
    ))
    .expect_err("operator dispatch must enforce clone-safety obligations");
    assert_eq!(operator_rng.code, "AU3007");
}

#[test]
fn rng_clone_safety_operator_method_generics_bind_the_actual_rhs() {
    let surface = r#"
trait Add[Rhs]:
    def add[T](self, rhs: own T):
        values = [rhs]
        copies = values.copy()
        print(copies)

class Marker:
    value: int64

impl[Rhs] Add[Rhs] for Marker:
    pass

def combine[T, U](marker: Marker, rhs: own U):
    marker + rhs
"#;

    crate::check_source(&format!(
        "{surface}\ndef main() -> int32:\n    marker = Marker(0)\n    combine[int64, int64](marker, 1)\n    return 0\n"
    ))
    .expect("operator method inference should accept a clone-safe int64 RHS");

    let error = crate::check_source(&format!(
        "import random\n{surface}\ndef main() -> int32:\n    marker = Marker(0)\n    combine[int64, random.Rng](marker, random.Rng(seed=1))\n    return 0\n"
    ))
    .expect_err(
        "operator method inference must attach clone safety to the actual RHS, not a same-named caller parameter",
    );
    assert_eq!(error.code, "AU3007", "unexpected diagnostic: {error:?}");
    assert!(error.message.contains("non-cloneable `random.Rng`"));
}

#[test]
fn rng_clone_safety_is_enforced_when_try_selects_a_from_impl() {
    let surface = r#"
class Target[Element]:
    values: list[Element]

trait From[Item]:
    def from(value: own Item) -> Target[Item]:
        values = [value]
        return Target(values.copy())

impl[Item] From[Item] for Target[Item]:
    pass
"#;
    crate::check_source(&format!(
        "{surface}\ndef read() -> Result[int32, int64]:\n    return Result.Err(1)\n\ndef load() -> Result[int32, Target[int64]]:\n    return Result.Ok(try read())\n"
    ))
    .expect("try should accept a clone-safe From instantiation");

    let error = crate::check_source(&format!(
        "import random\n{surface}\ndef read() -> Result[int32, random.Rng]:\n    return Result.Err(random.Rng(seed=1))\n\ndef load() -> Result[int32, Target[random.Rng]]:\n    return Result.Ok(try read())\n"
    ))
    .expect_err("try must enforce the selected From method's clone obligation");
    assert_eq!(error.code, "AU3007", "unexpected diagnostic: {error:?}");
    assert!(error.message.contains("non-cloneable `random.Rng`"));
}

#[test]
fn rng_clone_safety_from_method_generics_bind_the_actual_source() {
    let surface = r#"
class Target[Source]:
    value: int64

trait From[Source]:
    def from[T](value: own T) -> Target[Source]:
        values = [value]
        copies = values.copy()
        print(copies)
        return Target(0)

impl[Source] From[Source] for Target[Source]:
    pass

def convert[T, U](value: own Result[int32, U]) -> Result[int32, Target[U]]:
    return Result.Ok(try value)
"#;

    crate::check_source(&format!(
        "{surface}\ndef main() -> int32:\n    input: Result[int32, int64] = Result.Err(1)\n    converted = convert[int64, int64](input)\n    return 0\n"
    ))
    .expect("implicit From inference should accept a clone-safe int64 source");

    let error = crate::check_source(&format!(
        "import random\n{surface}\ndef main() -> int32:\n    input: Result[int32, random.Rng] = Result.Err(random.Rng(seed=1))\n    converted = convert[int64, random.Rng](input)\n    return 0\n"
    ))
    .expect_err(
        "implicit From inference must attach clone safety to the actual source, not a same-named caller parameter",
    );
    assert_eq!(error.code, "AU3007", "unexpected diagnostic: {error:?}");
    assert!(error.message.contains("non-cloneable `random.Rng`"));
}

#[test]
fn user_defined_rng_class_does_not_acquire_random_module_semantics() {
    crate::check_source(
        r#"
class Rng:
    value: int64

    def next_int(self, lo: int64, hi: int64) -> int64:
        return self.value

    def next_float(self) -> str:
        return "local"

    def shuffle(self, value: int64) -> int64:
        return value

trait LocalShuffle:
    def rearrange(self) -> str

impl LocalShuffle for Rng:
    def rearrange(self) -> str:
        return self.next_float()

def main() -> int32:
    rng = Rng(5)
    print(rng.next_int(0, 10))
    print(rng.next_float())
    print(rng.shuffle(7))
    print(rng.rearrange())
    return 0
"#,
    )
    .expect("an ordinary class named Rng must keep its own methods and shared receivers");
}

#[test]
fn d3_int_alias_canonicalizes_across_signatures_generics_and_casts() {
    let source = r#"
def identity[T](value: own T) -> T:
    return value

def round_trip(value: int, values: list[int]) -> int:
    casted: int = value as int
    return identity[int](value=casted)

def main() -> int32:
    values: list[int] = [2147483648]
    print(round_trip(1, values))
    return 0
"#;

    let program = crate::check_source(source).expect("the int alias surface should type check");
    let round_trip = program
        .functions
        .get("round_trip")
        .expect("round_trip should be registered");
    assert_eq!(
        round_trip.signature.params,
        vec![
            Type::named("int64"),
            Type::Named("list".to_string(), vec![Type::named("int64")]),
        ]
    );
    assert_eq!(round_trip.signature.return_type, Type::named("int64"));

    assert_eq!(
        lower_type(
            &type_ref("int"),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("int should lower as a built-in alias"),
        Type::named("int64")
    );
}

#[test]
fn d3_unhinted_integer_literals_default_to_checked_int64() {
    let source = r#"
def accept(value: int64):
    print(value)

def accept_values(values: list[int64]):
    print(values.len())

def accept_option(value: Option[int64]):
    print(value != Option.None)

def main() -> int32:
    positive = 2147483648
    negative = -2147483649
    values = [1, 2147483648]
    maybe = Option.Some(1)
    accept(positive)
    accept(negative)
    accept_values(values)
    accept_option(maybe)
    return 0
"#;
    crate::check_source(source).expect("unhinted integer expressions should infer int64");

    for (literal, expected_message) in [
        (
            "9223372036854775808",
            "integer literal `9223372036854775808` does not fit in `int64`",
        ),
        (
            "-9223372036854775809",
            "integer literal `-9223372036854775809` does not fit in `int64`",
        ),
    ] {
        let source = format!("def main():\n    value = {literal}\n");
        let error = crate::check_source(&source)
            .expect_err("an unhinted literal outside int64 must be rejected");
        assert_eq!(error.message, expected_message);
    }

    crate::check_source(
        "def main() -> int32:\n    positive: int128 = 9223372036854775808\n    negative: int128 = -9223372036854775809\n    narrow: int32 = 2147483647\n    return 0\n",
    )
    .expect("explicit wider and fixed-width integer contexts should remain authoritative");
}

#[test]
fn integer_base_spellings_preserve_contextual_fixed_width_typing() {
    crate::check_source(
        r#"
def main():
    signed8_max: int8 = 0x7f
    signed8_min: int8 = -0x80
    unsigned8_max: uint8 = 0Xff
    signed16_max: int16 = 0x7fff
    signed16_min: int16 = -0x8000
    unsigned16_max: uint16 = 0xffff
    signed32_max: int32 = 0x7fff_ffff
    signed32_min: int32 = -0x8000_0000
    unsigned32_max: uint32 = 0xffff_ffff
    signed64_max: int64 = 0x7fff_ffff_ffff_ffff
    signed64_min: int64 = -0x8000_0000_0000_0000
    unsigned64_max: uint64 = 0xffff_ffff_ffff_ffff
    signed128_max: int128 = 0x7fff_ffff_ffff_ffff_ffff_ffff_ffff_ffff
    signed128_min: int128 = -0x8000_0000_0000_0000_0000_0000_0000_0000
    unsigned128_max: uint128 = 0xffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff
    alias: int = 0b1010_0110
    octal: uint16 = 0O17_777
"#,
    )
    .expect("base-prefixed values should retain ordinary contextual integer typing");

    let narrow = crate::check_source("def main():\n    value: int8 = 0x80\n")
        .expect_err("base spelling must not bypass contextual range checking");
    assert_eq!(
        narrow.message,
        "integer literal `128` does not fit in `int8`"
    );

    let defaulted = crate::check_source("def main():\n    value = 0x8000_0000_0000_0000\n")
        .expect_err("unhinted base spelling must still default to int64");
    assert_eq!(
        defaulted.message,
        "integer literal `9223372036854775808` does not fit in `int64`"
    );
}

#[test]
fn bitwise_shift_and_power_operators_require_the_accepted_exact_numeric_types() {
    crate::check_source(
        r#"
def main():
    mut a: int8 = 5
    b: int8 = 3
    anded: int8 = a & b
    ored: int8 = a | b
    xored: int8 = a ^ b
    inverted: int8 = ~a
    left: int8 = b << 1
    right: int8 = a >> 1
    powered: int8 = b ** 3
    float_power: float64 = 2.0 ** -2.0
    a &= b
    a |= b
    a ^= b
    a <<= 1
    a >>= 1
    a **= 2
"#,
    )
    .expect("accepted bitwise, shift, power, and compound forms should type check");

    for (source, code, fragment) in [
        (
            "def main():\n    value = 1 & 1.0\n",
            "AU2003",
            "require integer operands",
        ),
        (
            "def main():\n    value = 1.0 << 1.0\n",
            "AU2003",
            "require integer operands",
        ),
        (
            "def main():\n    value = ~1.0\n",
            "AU2003",
            "expects an integer",
        ),
        (
            "def main():\n    value = 2 ** -1\n",
            "AU2003",
            "negative exponent",
        ),
    ] {
        let error = crate::check_source(source).expect_err(source);
        assert_eq!(error.code, code, "{source}: {error}");
        assert!(error.message.contains(fragment), "{source}: {error}");
    }
}

#[test]
fn index_domain_positions_contextually_type_literals_as_int64() {
    crate::check_source(
        r#"
def main() -> int32:
    mut values: list[int32] = [1]
    values[0] = 2
    print(values[0])
    print(values.get(index=0))
    jobs = Queue[int32](capacity=4)
    for value in range(stop=4):
        print(value)
    for value in range(1, 4):
        print(value)
    jobs.close()
    return 0
"#,
    )
    .expect("index-domain APIs should contextually type ordinary literals as int64");

    for statement in [
        "print(values[9223372036854775808])",
        "values[9223372036854775808] = 1",
    ] {
        let source = format!(
            "def main() -> int32:\n    mut values: list[int32] = [1]\n    {statement}\n    return 0\n"
        );
        let error = crate::check_source(&source).expect_err("list index literals must fit int64");
        assert_eq!(
            error.message,
            "integer literal `9223372036854775808` does not fit in `int64`"
        );
    }
}

#[test]
fn index_domain_accepts_default_int64_variables() {
    for statement in [
        "print(values[index])",
        "values[index] = 2",
        "print(values.get(index))",
        "values.set(index, 2)",
        "values.pop(index)",
        "values.swap(index, 0)",
        "values.insert(index, 2)",
    ] {
        let source = format!(
            "def main() -> int32:\n    mut values: list[int32] = [1]\n    index = 0\n    {statement}\n    return 0\n"
        );
        crate::check_source(&source).unwrap_or_else(|error| {
            panic!("default int64 index failed for `{statement}`: {error:?}")
        });
    }
}

#[test]
fn d3_generic_calls_use_expected_results_to_contextually_type_literal_arguments() {
    crate::check_source(
        r#"
def identity[T](value: own T) -> T:
    return value

def main() -> int32:
    positional: int32 = identity(100)
    named: int32 = identity(value=5)
    defaulted: int64 = identity(2147483648)
    print(positional)
    print(named)
    print(defaulted)
    return 0
"#,
    )
    .expect("the expected generic result should contextually type literal arguments");
}

#[test]
fn d3_int_alias_is_a_reserved_builtin_type_name() {
    let direct = reject_reserved_type_name("int", Span::new(1, 1))
        .expect_err("the int alias should be reserved");
    assert_eq!(direct.message, "`int` is a reserved built-in type name");

    let error = crate::check_source("class int:\n    value: int32\n")
        .expect_err("user types cannot shadow the int alias");
    assert_eq!(error.message, "`int` is a reserved built-in type name");
}

fn projection_path(path: &str) -> ProjectionPath {
    if path.is_empty() {
        return ProjectionPath::default();
    }
    ProjectionPath(
        path.split('.')
            .map(|field| PlaceProjection::Field(field.to_string()))
            .collect(),
    )
}

fn place_path(path: &str) -> PlacePath {
    let mut segments = path.split('.');
    let root = segments.next().expect("test place paths require a root");
    PlacePath {
        root: root.to_string(),
        projections: ProjectionPath(
            segments
                .map(|field| PlaceProjection::Field(field.to_string()))
                .collect(),
        ),
    }
}

#[test]
fn contextual_none_equality_type_checks_symmetrically() {
    let source = include_str!("../tests/fixtures/check-pass/contextual_none_positions.au");
    crate::check_source(source).expect("contextual None positions should type-check");
}

#[test]
fn contextual_none_rejects_non_optional_comparisons_symmetrically() {
    for expression in ["value == None", "None != value"] {
        let source = format!("def invalid(value: int32) -> bool:\n    return {expression}\n");
        let error = crate::check_source(&source)
            .expect_err("None comparisons with non-optional values should fail");
        assert_eq!(
            error.message,
            "type `int32` is not optional; only `Option[T]` values can be compared with `None`"
        );
    }
}

fn nested_type_ref(name: &str, args: Vec<TypeRef>) -> TypeRef {
    TypeRef::named(name, args, false, Span::new(1, 1))
}

fn type_to_ref(ty: &Type) -> TypeRef {
    match ty {
        Type::Named(name, args) => TypeRef::named(
            name,
            args.iter().map(type_to_ref).collect(),
            false,
            Span::new(1, 1),
        ),
        Type::Tuple(elements) => TypeRef::tuple(
            elements.iter().map(type_to_ref).collect(),
            false,
            Span::new(1, 1),
        ),
        Type::Function {
            params,
            return_type,
        } => TypeRef::function_with_params(
            params
                .iter()
                .map(|param| {
                    let mode = match param.passing {
                        ReceiverKind::Borrow => ParamMode::Default,
                        ReceiverKind::BorrowMut => ParamMode::BorrowMut,
                        ReceiverKind::Value => ParamMode::Own,
                    };
                    crate::ast::FunctionTypeParam::new(
                        mode,
                        type_to_ref(&param.ty),
                        Span::new(1, 1),
                    )
                })
                .collect(),
            type_to_ref(return_type),
            Span::new(1, 1),
        ),
        Type::Closure {
            params,
            return_type,
            ..
        } => TypeRef::function_with_params(
            params
                .iter()
                .map(|param| {
                    let mode = match param.passing {
                        ReceiverKind::Borrow => ParamMode::Default,
                        ReceiverKind::BorrowMut => ParamMode::BorrowMut,
                        ReceiverKind::Value => ParamMode::Own,
                    };
                    crate::ast::FunctionTypeParam::new(
                        mode,
                        type_to_ref(&param.ty),
                        Span::new(1, 1),
                    )
                })
                .collect(),
            type_to_ref(return_type),
            Span::new(1, 1),
        ),
        Type::TypeParam(name) | Type::Module(name) => type_ref(name),
        Type::Unit => type_ref("None"),
    }
}

fn expr(kind: ExprKind) -> Expr {
    Expr {
        kind,
        span: Span::new(1, 1),
    }
}

fn arg(value: Expr) -> Argument {
    Argument {
        name: None,
        span: value.span,
        value,
    }
}

fn named_arg(name: &str, value: Expr) -> Argument {
    Argument {
        name: Some(name.to_string()),
        span: value.span,
        value,
    }
}

fn function_decl(name: &str) -> FunctionDecl {
    FunctionDecl {
        public: true,
        name: name.to_string(),
        type_params: Vec::new(),
        type_param_bounds: BTreeMap::new(),
        receiver: None,
        params: Vec::new(),
        return_type: type_ref("None"),
        view_return: None,
        body: Vec::new(),
        span: Span::new(1, 1),
    }
}

fn unary_function_decl(name: &str) -> FunctionDecl {
    let mut decl = function_decl(name);
    decl.params.push(Param {
        name: "value".to_string(),
        mode: ParamMode::Own,
        ty: type_ref("int32"),
        default: None,
        span: Span::new(1, 1),
    });
    decl
}

fn trait_decl(name: &str, type_params: Vec<&str>) -> TraitDecl {
    TraitDecl {
        public: true,
        name: name.to_string(),
        type_params: type_params.into_iter().map(str::to_string).collect(),
        supertraits: Vec::new(),
        methods: Vec::new(),
        span: Span::new(1, 1),
    }
}

fn function_signature(params: Vec<Type>, return_type: Type) -> FunctionSignature {
    let param_passings = vec![ReceiverKind::Value; params.len()];
    FunctionSignature {
        params,
        param_passings,
        return_type,
        rng_clone_safe_type_params: BTreeSet::new(),
        array_equality_safe_type_params: BTreeSet::new(),
    }
}

fn trait_info(name: &str, type_params: Vec<&str>) -> TraitInfo {
    TraitInfo {
        module_name: "<test>".to_string(),
        decl: trait_decl(name, type_params),
        supertraits: Vec::new(),
        methods: BTreeMap::new(),
    }
}

fn class_decl(name: &str, copy: bool, fields: Vec<FieldDecl>) -> ClassDecl {
    ClassDecl {
        public: true,
        copy,
        name: name.to_string(),
        type_params: Vec::new(),
        type_param_bounds: BTreeMap::new(),
        fields,
        methods: Vec::new(),
        span: Span::new(1, 1),
    }
}

fn field_decl(name: &str, ty: TypeRef, indirect: bool) -> FieldDecl {
    FieldDecl {
        public: true,
        name: name.to_string(),
        ty: TypeRef { indirect, ..ty },
        default: None,
        span: Span::new(1, 1),
    }
}

fn class_info(name: &str, copy: bool, field_specs: Vec<(&str, Type, bool)>) -> ClassInfo {
    let decl_fields = field_specs
        .iter()
        .map(|(field_name, ty, indirect)| field_decl(field_name, type_to_ref(ty), *indirect))
        .collect::<Vec<_>>();
    let fields = field_specs
        .into_iter()
        .map(|(field_name, ty, _)| {
            (
                field_name.to_string(),
                FieldInfo {
                    public: true,
                    ty,
                    span: Span::new(1, 1),
                },
            )
        })
        .collect();
    ClassInfo {
        module_name: "<test>".to_string(),
        is_builtin: false,
        decl: class_decl(name, copy, decl_fields),
        type_param_bounds: BTreeMap::new(),
        fields,
        methods: BTreeMap::new(),
    }
}

fn enum_info(name: &str, payload: Option<Type>) -> EnumInfo {
    let payload_fields = payload
        .as_ref()
        .map(|ty| crate::ast::EnumPayloadFieldDecl {
            name: None,
            ty: type_to_ref(ty),
            span: Span::new(1, 1),
        })
        .into_iter()
        .collect::<Vec<_>>();
    let payload_infos = payload
        .into_iter()
        .map(|ty| EnumPayloadFieldInfo {
            name: None,
            ty,
            span: Span::new(1, 1),
        })
        .collect::<Vec<_>>();
    EnumInfo {
        module_name: "<test>".to_string(),
        decl: EnumDecl {
            public: true,
            name: name.to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            variants: vec![crate::ast::EnumVariantDecl {
                name: "Value".to_string(),
                payloads: payload_fields,
                named_payloads: false,
                span: Span::new(1, 1),
            }],
            span: Span::new(1, 1),
        },
        type_param_bounds: BTreeMap::new(),
        variants: BTreeMap::from([(
            "Value".to_string(),
            EnumVariantInfo {
                payloads: payload_infos,
                named_payloads: false,
                span: Span::new(1, 1),
            },
        )]),
    }
}

fn namespace(path: &str) -> ModuleNamespace {
    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: path.rsplit('.').next().unwrap_or(path).to_string(),
        path: path.to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: BTreeMap::new(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::new(),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    }
}

fn checker<'a>(
    module_name: &'a str,
    type_names: &'a BTreeMap<String, Span>,
    type_arities: &'a BTreeMap<String, usize>,
    classes: &'a BTreeMap<String, ClassInfo>,
    enums: &'a BTreeMap<String, EnumInfo>,
    functions: &'a BTreeMap<String, FunctionInfo>,
    traits: &'a BTreeMap<String, TraitInfo>,
    trait_impls: &'a [TraitImplInfo],
    imported_modules: &'a BTreeMap<String, ModuleNamespace>,
    module_registry: &'a BTreeMap<String, ModuleNamespace>,
) -> FunctionChecker<'a> {
    FunctionChecker::new(
        module_name,
        type_names,
        type_arities,
        empty_canonical_type_names(),
        classes,
        enums,
        functions,
        empty_constants(),
        traits,
        trait_impls,
        imported_modules,
        module_registry,
    )
}

fn local_binding(
    ty: Type,
    assignable: bool,
    mutable_place: bool,
    passing: ReceiverKind,
    moved: bool,
    moved_fields: &[&str],
) -> LocalBinding {
    LocalBinding {
        ty,
        assignable,
        mutable_place,
        managed_resource: false,
        passing,
        borrow_origin: None,
        borrowed_at: None,
        match_borrow_place: None,
        stale_match_borrow_place: None,
        shared_match_scrutinee: None,
        moved,
        moved_at: moved.then_some(Span::new(1, 1)),
        moved_fields: moved_fields
            .iter()
            .map(|field| (projection_path(field), Span::new(1, 1)))
            .collect(),
        frozen_places: BTreeMap::new(),
        shared_match_places: BTreeMap::new(),
        captured: false,
        view: None,
        closure_loans: Vec::new(),
    }
}

fn assign_stmt(
    target: AssignTarget,
    mutable: bool,
    annotation: Option<TypeRef>,
    op: Option<BinaryOp>,
    value: Expr,
) -> AssignStmt {
    AssignStmt {
        mutable,
        target,
        annotation,
        op,
        value,
        span: Span::new(1, 1),
    }
}

fn type_maps_from_program(program: &Program) -> (BTreeMap<String, Span>, BTreeMap<String, usize>) {
    let mut type_names = BTreeMap::new();
    let mut type_arities = BTreeMap::new();
    for (name, class_info) in &program.classes {
        type_names.insert(name.clone(), class_info.decl.span);
        type_arities.insert(name.clone(), class_info.decl.type_params.len());
    }
    for (name, enum_info) in &program.enums {
        type_names.insert(name.clone(), enum_info.decl.span);
        type_arities.insert(name.clone(), enum_info.decl.type_params.len());
    }
    for (name, trait_info) in &program.traits {
        type_names.insert(name.clone(), trait_info.decl.span);
        type_arities.insert(name.clone(), trait_info.decl.type_params.len());
    }
    (type_names, type_arities)
}

#[test]
fn checker_small_helper_utilities_cover_default_arg_and_recursive_type_paths() {
    let mut collected = BTreeSet::new();
    let type_names = BTreeMap::from([("Known".to_string(), Span::new(1, 1))]);
    collect_type_ref_type_params(&type_ref("T"), &type_names, &mut collected, true);
    collect_type_ref_type_params(
        &nested_type_ref("list", vec![type_ref("U")]),
        &type_names,
        &mut collected,
        false,
    );
    collect_type_ref_type_params(&type_ref("int32"), &type_names, &mut collected, true);
    collect_type_ref_type_params(&type_ref("Known"), &type_names, &mut collected, true);
    assert_eq!(
        collected,
        BTreeSet::from(["T".to_string(), "U".to_string()])
    );

    let param_names = vec!["left".to_string(), "right".to_string()];
    let binary_default = expr(ExprKind::Binary {
        op: BinaryOp::Add,
        left: Box::new(expr(ExprKind::Map(vec![MapEntryExpr {
            key: expr(ExprKind::String("key".to_string())),
            value: expr(ExprKind::Int(1)),
        }]))),
        right: Box::new(expr(ExprKind::Set(vec![expr(ExprKind::Index {
            object: Box::new(expr(ExprKind::Name("left".to_string()))),
            index: Box::new(expr(ExprKind::Int(0))),
        })]))),
    });
    assert_eq!(
        default_argument_references_param(&binary_default, &param_names),
        Some("left".to_string())
    );
    let fstring_default = expr(ExprKind::FString(vec![
        crate::ast::FormatPart::Literal("value=".to_string()),
        crate::ast::FormatPart::Expr(expr(ExprKind::Name("right".to_string()))),
    ]));
    assert_eq!(
        default_argument_references_param(&fstring_default, &param_names),
        Some("right".to_string())
    );
    assert_eq!(
        default_argument_references_param(&expr(ExprKind::Bool(true)), &param_names),
        None
    );

    assert_eq!(
        render_literal_pattern_key(&LiteralPatternKey::String("aura".to_string())),
        "\"aura\""
    );
    assert_eq!(
        render_literal_pattern_key(&LiteralPatternKey::Bool(false)),
        "false"
    );
    assert_eq!(
        render_literal_pattern_key(&LiteralPatternKey::Float(1.5f64.to_bits())),
        "1.5"
    );

    let string_ty = Type::named("str");
    let int_ty = Type::named("int32");
    let set_ty = Type::Named("set".to_string(), vec![string_ty.clone()]);
    let map_ty = Type::Named("dict".to_string(), vec![string_ty.clone(), int_ty.clone()]);
    assert_eq!(set_element_type(&set_ty), Some(&string_ty));
    assert_eq!(map_key_value_types(&map_ty), Some((&string_ty, &int_ty)));

    assert!(Type::named("int32").is_copy());
    assert!(!Type::Named("list".to_string(), vec![Type::named("int32")]).is_copy());
    assert!(type_contains_named(
        &Type::Named(
            "dict".to_string(),
            vec![
                Type::named("str"),
                Type::Named("list".to_string(), vec![Type::named("Leaf")]),
            ],
        ),
        "Leaf"
    ));

    let classes = BTreeMap::from([
        (
            "Branch".to_string(),
            class_info("Branch", false, vec![("leaf", Type::named("Leaf"), false)]),
        ),
        (
            "Root".to_string(),
            class_info(
                "Root",
                false,
                vec![("branch", Type::named("Branch"), false)],
            ),
        ),
        (
            "RootIndirect".to_string(),
            class_info(
                "RootIndirect",
                false,
                vec![("branch", Type::named("Branch"), true)],
            ),
        ),
        (
            "Leaf".to_string(),
            class_info("Leaf", false, vec![("value", Type::named("int32"), false)]),
        ),
    ]);
    assert!(type_reaches_class_through_non_indirect_fields(
        &Type::named("Root"),
        "Leaf",
        &classes,
        &mut BTreeSet::new(),
    ));
    assert!(!type_reaches_class_through_non_indirect_fields(
        &Type::named("RootIndirect"),
        "Leaf",
        &classes,
        &mut BTreeSet::new(),
    ));

    let recursive_classes = BTreeMap::from([
        (
            "A".to_string(),
            class_info("A", false, vec![("b", Type::named("B"), false)]),
        ),
        (
            "B".to_string(),
            class_info("B", false, vec![("a", Type::named("A"), false)]),
        ),
    ]);
    assert!(!type_reaches_class_through_non_indirect_fields(
        &Type::named("A"),
        "Missing",
        &recursive_classes,
        &mut BTreeSet::new(),
    ));

    let mut broken = class_info(
        "Broken",
        false,
        vec![("lost", Type::named("Missing"), false)],
    );
    broken.fields.clear();
    assert!(!type_reaches_class_through_non_indirect_fields(
        &Type::named("Broken"),
        "Missing",
        &BTreeMap::from([("Broken".to_string(), broken)]),
        &mut BTreeSet::new(),
    ));
}

#[test]
fn checker_helper_paths_cover_explicit_type_args_and_pattern_unification_edges() {
    let program = crate::check_source(
            "class Box[T]:\n    value: T\n\ntrait Show:\n    def show(self) -> str\n\ndef main():\n    pass\n",
        )
        .expect("helper program should type check");
    let (type_names, type_arities) = type_maps_from_program(&program);
    let checker = FunctionChecker::new(
        &program.module_name,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &program.classes,
        &program.enums,
        &program.functions,
        &program.constants,
        &program.traits,
        &program.trait_impls,
        &program.imported_modules,
        &program.module_registry,
    );
    let span = Span::new(3, 4);
    let empty_type_params = BTreeMap::new();
    assert_eq!(
        lower_type(
            &type_ref("TaskGroup"),
            &type_names,
            &type_arities,
            empty_canonical_type_names(),
            &empty_type_params
        )
        .expect("TaskGroup should lower without type arguments"),
        Type::named("TaskGroup")
    );
    assert_eq!(
        lower_type(
            &type_ref("Duration"),
            &type_names,
            &type_arities,
            empty_canonical_type_names(),
            &empty_type_params
        )
        .expect("Duration should lower without type arguments"),
        Type::named("Duration")
    );

    let explicit = checker
        .explicit_type_substitutions(&["T".to_string()], &[type_ref("str")], span, "Box")
        .expect("single explicit type arg should lower");
    assert_eq!(
        explicit,
        HashMap::from([("T".to_string(), Type::named("str"))])
    );

    let explicit_arity = checker
        .explicit_type_substitutions(
            &["T".to_string()],
            &[type_ref("str"), type_ref("int32")],
            span,
            "Box",
        )
        .expect_err("mismatched explicit type arg counts should fail");
    assert!(explicit_arity
        .message
        .contains("Box expects 1 type argument"));

    checker
        .validate_integer_literal(7, &Type::named("str"), span)
        .expect("non-integer targets should skip integer literal bounds checks");
    checker
        .validate_integer_literal(127, &Type::named("int8"), span)
        .expect("in-range integers should validate");
    let overflow = checker
        .validate_integer_literal(128, &Type::named("int8"), span)
        .expect_err("overflowing integer literals should fail");
    assert!(overflow.message.contains("does not fit in `int8`"));

    let type_params = BTreeSet::from(["T".to_string()]);
    let mut module_substitutions = HashMap::new();
    assert!(type_pattern_matches(
        &Type::Module("helpers.math".to_string()),
        &Type::Module("helpers.math".to_string()),
        &type_params,
        &mut module_substitutions,
    ));
    assert!(type_pattern_matches(
        &Type::Unit,
        &Type::Unit,
        &type_params,
        &mut HashMap::new(),
    ));
    assert!(type_pattern_matches(
        &Type::TypeParam("U".to_string()),
        &Type::TypeParam("U".to_string()),
        &type_params,
        &mut HashMap::new(),
    ));
    assert_eq!(type_pattern_specificity(&Type::Unit), 1);
    assert_eq!(
        type_pattern_specificity(&Type::Module("helpers.math".to_string())),
        1
    );
    assert!(has_unresolved_type_params(&Type::Named(
        "Option".to_string(),
        vec![Type::TypeParam("T".to_string())],
    )));
    assert!(!has_unresolved_type_params(&Type::Unit));
    assert!(!has_unresolved_type_params(&Type::Module(
        "helpers.math".to_string()
    )));
    assert_eq!(
        substitute_type(&Type::Module("helpers.math".to_string()), &HashMap::new()),
        Type::Module("helpers.math".to_string())
    );
    let mut collected = BTreeSet::new();
    collect_type_params_from_type(&Type::Unit, &mut collected);
    collect_type_params_from_type(&Type::Module("helpers.math".to_string()), &mut collected);
    assert!(collected.is_empty());

    unify_type_pattern(&Type::Unit, &Type::Unit, &mut HashMap::new())
        .expect("unit patterns should unify with unit");
    unify_type_pattern(
        &Type::Module("helpers.math".to_string()),
        &Type::Module("helpers.math".to_string()),
        &mut HashMap::new(),
    )
    .expect("module patterns should unify with matching modules");

    let unit_mismatch = unify_type_pattern(&Type::Unit, &Type::named("int32"), &mut HashMap::new())
        .expect_err("unit mismatches should report `None` diagnostics");
    assert!(unit_mismatch
        .message
        .contains("expected `None`, found `int32`"));

    let module_mismatch = unify_type_pattern(
        &Type::Module("helpers.math".to_string()),
        &Type::Module("helpers.other".to_string()),
        &mut HashMap::new(),
    )
    .expect_err("module mismatches should mention both paths");
    assert!(module_mismatch
        .message
        .contains("expected `module helpers.math`, found `module helpers.other`"));
    let named_against_unit =
        unify_type_pattern(&Type::named("str"), &Type::Unit, &mut HashMap::new())
            .expect_err("named patterns should reject non-named actual types");
    assert!(named_against_unit
        .message
        .contains("expected `str`, found `None`"));
}

#[test]
fn checker_expression_helper_paths_cover_collection_specialization_and_control_edges() {
    let program = crate::check_source(
            "class Counter:\n    value: int32\n\nclass Holder[T]:\n    value: T\n\nclass PairBox[A, B]:\n    left: A\n    right: B\n\nclass Flag:\n    value: bool\n\nenum Maybe[T]:\n    Value(T)\n    Empty\n\nenum Pair[A, B]:\n    Empty\n\ndef work(value: int32) -> int32:\n    return value\n\ndef main():\n    pass\n",
        )
        .expect("helper program should type check");
    let (type_names, type_arities) = type_maps_from_program(&program);
    let mut not_decl = function_decl("not");
    not_decl.receiver = Some(ReceiverKind::Borrow);
    not_decl.return_type = type_ref("Out");
    let mut neg_decl = function_decl("neg");
    neg_decl.receiver = Some(ReceiverKind::Borrow);
    neg_decl.return_type = type_ref("Out");
    let traits = BTreeMap::from([
        (
            "Not".to_string(),
            TraitInfo {
                module_name: program.module_name.clone(),
                decl: trait_decl("Not", vec!["Out"]),
                supertraits: Vec::new(),
                methods: BTreeMap::from([(
                    "not".to_string(),
                    TraitMethodInfo {
                        decl: not_decl.clone(),
                        signature: function_signature(
                            Vec::new(),
                            Type::TypeParam("Out".to_string()),
                        ),
                        type_param_bounds: BTreeMap::new(),
                    },
                )]),
            },
        ),
        (
            "Neg".to_string(),
            TraitInfo {
                module_name: program.module_name.clone(),
                decl: trait_decl("Neg", vec!["Out"]),
                supertraits: Vec::new(),
                methods: BTreeMap::from([(
                    "neg".to_string(),
                    TraitMethodInfo {
                        decl: neg_decl.clone(),
                        signature: function_signature(
                            Vec::new(),
                            Type::TypeParam("Out".to_string()),
                        ),
                        type_param_bounds: BTreeMap::new(),
                    },
                )]),
            },
        ),
    ]);
    let trait_impls = vec![
        TraitImplInfo {
            module_name: program.module_name.clone(),
            decl: ImplDecl {
                type_params: Vec::new(),
                type_param_bounds: BTreeMap::new(),
                trait_name: "Not".to_string(),
                trait_args: vec![type_ref("Flag")],
                for_type: type_ref("Flag"),
                methods: vec![not_decl.clone()],
                span: Span::new(1, 1),
            },
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            trait_name: "Not".to_string(),
            trait_args: vec![Type::named("Flag")],
            for_type: Type::named("Flag"),
            methods: BTreeMap::from([(
                "not".to_string(),
                TraitImplMethodInfo {
                    decl: not_decl.clone(),
                    signature: function_signature(Vec::new(), Type::named("Flag")),
                    type_param_bounds: BTreeMap::new(),
                },
            )]),
        },
        TraitImplInfo {
            module_name: program.module_name.clone(),
            decl: ImplDecl {
                type_params: Vec::new(),
                type_param_bounds: BTreeMap::new(),
                trait_name: "Neg".to_string(),
                trait_args: vec![type_ref("Flag")],
                for_type: type_ref("Flag"),
                methods: vec![neg_decl.clone()],
                span: Span::new(1, 1),
            },
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            trait_name: "Neg".to_string(),
            trait_args: vec![Type::named("Flag")],
            for_type: Type::named("Flag"),
            methods: BTreeMap::from([(
                "neg".to_string(),
                TraitImplMethodInfo {
                    decl: neg_decl.clone(),
                    signature: function_signature(Vec::new(), Type::named("Flag")),
                    type_param_bounds: BTreeMap::new(),
                },
            )]),
        },
    ];
    let mut checker = FunctionChecker::new(
        &program.module_name,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &program.classes,
        &program.enums,
        &program.functions,
        &program.constants,
        &traits,
        &trait_impls,
        &program.imported_modules,
        &program.module_registry,
    );
    let vec_string = Type::Named("list".to_string(), vec![Type::named("str")]);
    let set_string = Type::Named("set".to_string(), vec![Type::named("str")]);
    let map_string_string = Type::Named(
        "dict".to_string(),
        vec![Type::named("str"), Type::named("str")],
    );
    let option_int = Type::Named("Option".to_string(), vec![Type::named("int32")]);
    let result_int_string = Type::Named(
        "Result".to_string(),
        vec![Type::named("int32"), Type::named("str")],
    );
    let task_int = Type::Named("Task".to_string(), vec![Type::named("int32")]);
    let task_list_int = Type::Named("list".to_string(), vec![task_int.clone()]);
    let mut locals = HashMap::from([
        (
            "moved".to_string(),
            local_binding(
                Type::named("str"),
                true,
                true,
                ReceiverKind::Value,
                true,
                &[],
            ),
        ),
        (
            "partial".to_string(),
            local_binding(
                Type::named("Counter"),
                true,
                true,
                ReceiverKind::Value,
                false,
                &["value"],
            ),
        ),
        (
            "flag".to_string(),
            local_binding(
                Type::named("Flag"),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "words".to_string(),
            local_binding(
                vec_string.clone(),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "labels".to_string(),
            local_binding(
                set_string.clone(),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "scores".to_string(),
            local_binding(
                map_string_string.clone(),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "result_value".to_string(),
            local_binding(
                result_int_string.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "unit_value".to_string(),
            local_binding(Type::Unit, false, false, ReceiverKind::Value, false, &[]),
        ),
        (
            "text".to_string(),
            local_binding(
                Type::named("str"),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "group".to_string(),
            local_binding(
                Type::named("TaskGroup"),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "task".to_string(),
            local_binding(
                task_int.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "tasks".to_string(),
            local_binding(
                task_list_int.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "unit_tasks".to_string(),
            local_binding(
                Type::Named("list".to_string(), vec![Type::Unit]),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
    ]);

    assert_eq!(
        checker
            .type_of_expr_hint(
                &expr(ExprKind::Name("None".to_string())),
                &mut locals,
                Some(&option_int)
            )
            .expect("None should follow Option hints"),
        option_int
    );
    assert_eq!(
        checker
            .type_of_expr(&expr(ExprKind::Name("work".to_string())), &mut locals)
            .expect("functions should resolve to first-class callable types"),
        Type::Function {
            params: vec![FunctionParamContract {
                name: "value".to_string(),
                ty: Type::named("int32"),
                passing: ReceiverKind::Borrow,
                has_default: false,
                default_erased: false,
            }],
            return_type: Box::new(Type::named("int32")),
        }
    );
    assert_eq!(
        checker
            .type_of_expr(&expr(ExprKind::Name("Counter".to_string())), &mut locals)
            .expect("classes should resolve to named types"),
        Type::named("Counter")
    );
    assert_eq!(
        checker
            .type_of_expr(&expr(ExprKind::Name("Maybe".to_string())), &mut locals)
            .expect("enums should resolve to named types"),
        Type::named("Maybe")
    );
    assert!(checker
        .type_of_expr(&expr(ExprKind::Name("moved".to_string())), &mut locals)
        .expect_err("moved bindings should fail")
        .message
        .contains("use of moved value"));
    assert!(checker
        .type_of_expr(&expr(ExprKind::Name("partial".to_string())), &mut locals)
        .expect_err("partially moved bindings should fail")
        .message
        .contains("partially moved"));
    assert!(checker
        .type_of_expr(&expr(ExprKind::Name("missing".to_string())), &mut locals)
        .expect_err("unknown names should fail")
        .message
        .contains("unknown name"));

    assert_eq!(
        checker
            .type_of_expr_hint(
                &expr(ExprKind::List(vec![
                    expr(ExprKind::String("left".to_string())),
                    expr(ExprKind::String("right".to_string())),
                ])),
                &mut locals,
                Some(&vec_string),
            )
            .expect("str lists should type check"),
        vec_string
    );
    assert!(checker
        .type_of_expr_hint(
            &expr(ExprKind::List(vec![
                expr(ExprKind::String("left".to_string())),
                expr(ExprKind::Int(1)),
            ])),
            &mut locals,
            Some(&Type::Named("list".to_string(), vec![Type::named("str")])),
        )
        .expect_err("heterogeneous lists should fail")
        .message
        .contains("list literal elements must all have type"));
    assert!(checker
        .type_of_expr(&expr(ExprKind::List(Vec::new())), &mut locals)
        .expect_err("empty lists require context")
        .message
        .contains("empty list literals require an expected `list[T]`"));

    assert_eq!(
        checker
            .type_of_expr_hint(
                &expr(ExprKind::Set(vec![
                    expr(ExprKind::String("left".to_string())),
                    expr(ExprKind::String("right".to_string())),
                ])),
                &mut locals,
                Some(&set_string),
            )
            .expect("str sets should type check"),
        set_string
    );
    assert!(checker
        .type_of_expr_hint(
            &expr(ExprKind::Set(vec![
                expr(ExprKind::String("left".to_string())),
                expr(ExprKind::Int(1)),
            ])),
            &mut locals,
            Some(&Type::Named("set".to_string(), vec![Type::named("str")])),
        )
        .expect_err("heterogeneous sets should fail")
        .message
        .contains("set literal elements must all have type"));
    assert!(checker
        .type_of_expr(&expr(ExprKind::Set(Vec::new())), &mut locals)
        .expect_err("empty sets require context")
        .message
        .contains("empty set literals require an expected `set[T]`"));

    assert_eq!(
        checker
            .type_of_expr_hint(
                &expr(ExprKind::Map(vec![MapEntryExpr {
                    key: expr(ExprKind::String("name".to_string())),
                    value: expr(ExprKind::String("aura".to_string())),
                }])),
                &mut locals,
                Some(&map_string_string),
            )
            .expect("str maps should type check"),
        map_string_string
    );
    assert!(checker
        .type_of_expr_hint(
            &expr(ExprKind::Map(vec![MapEntryExpr {
                key: expr(ExprKind::Int(1)),
                value: expr(ExprKind::String("aura".to_string())),
            }])),
            &mut locals,
            Some(&map_string_string),
        )
        .expect_err("mismatched map keys should fail")
        .message
        .contains("map literal keys must all have type"));
    assert!(checker
        .type_of_expr(&expr(ExprKind::Map(Vec::new())), &mut locals)
        .expect_err("empty maps require context")
        .message
        .contains("empty map literals require an expected `dict[K, V]`"));

    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("set".to_string()))),
                type_args: vec![type_ref("int32"), type_ref("str")],
            }),
            &mut locals,
        )
        .expect_err("Set arity mismatches should fail")
        .message
        .contains("type `set` expects exactly one type argument"));
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("set".to_string()))),
                    type_args: vec![type_ref("int32")],
                }),
                &mut locals,
            )
            .expect("Set specialization should preserve its explicit element type"),
        Type::Named("set".to_string(), vec![Type::named("int32")])
    );
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("dict".to_string()))),
                type_args: vec![type_ref("str")],
            }),
            &mut locals,
        )
        .expect_err("Map arity mismatches should fail")
        .message
        .contains("type `dict` expects exactly two type arguments"));
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("dict".to_string()))),
                    type_args: vec![type_ref("str"), type_ref("int32")],
                }),
                &mut locals,
            )
            .expect("Map specialization should preserve explicit key/value types"),
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")]
        )
    );
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("Holder".to_string()))),
                type_args: vec![type_ref("int32"), type_ref("str")],
            }),
            &mut locals,
        )
        .expect_err("class arity mismatches should fail")
        .message
        .contains("class `Holder` expects 1 type argument"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("PairBox".to_string()))),
                type_args: vec![type_ref("int32")],
            }),
            &mut locals,
        )
        .expect_err("plural class arity diagnostics should be covered")
        .message
        .contains("class `PairBox` expects 2 type arguments"));
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("Holder".to_string()))),
                    type_args: vec![type_ref("int32")],
                }),
                &mut locals,
            )
            .expect("generic class specialization should lower explicit type args"),
        Type::Named("Holder".to_string(), vec![Type::named("int32")])
    );
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("Maybe".to_string()))),
                type_args: vec![type_ref("int32"), type_ref("str")],
            }),
            &mut locals,
        )
        .expect_err("enum arity mismatches should fail")
        .message
        .contains("enum `Maybe` expects 1 type argument"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("Pair".to_string()))),
                type_args: vec![type_ref("int32")],
            }),
            &mut locals,
        )
        .expect_err("plural enum arity diagnostics should be covered")
        .message
        .contains("enum `Pair` expects 2 type arguments"));
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("Maybe".to_string()))),
                    type_args: vec![type_ref("str")],
                }),
                &mut locals,
            )
            .expect("generic enum specialization should lower explicit type args"),
        Type::Named("Maybe".to_string(), vec![Type::named("str")])
    );
    let nongeneric_function_specialization = checker
        .type_of_expr(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("work".to_string()))),
                type_args: vec![type_ref("int32")],
            }),
            &mut locals,
        )
        .expect_err("a nongeneric function value rejects explicit type arguments");
    assert_eq!(nongeneric_function_specialization.code, "AU2002");
    assert!(nongeneric_function_specialization
        .message
        .contains("expects 0 type arguments, found 1"));

    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Cast {
                expr: Box::new(expr(ExprKind::String("aura".to_string()))),
                ty: type_ref("int32"),
            }),
            &mut locals,
        )
        .expect_err("casts should stay numeric-only")
        .message
        .contains("casts are only supported between numeric types"));

    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(expr(ExprKind::Bool(true))),
                }),
                &mut locals,
            )
            .expect("bool negation should type check"),
        Type::named("bool")
    );
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(expr(ExprKind::Name("flag".to_string()))),
                }),
                &mut locals,
            )
            .expect("trait-based unary not should resolve"),
        Type::named("Flag")
    );
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Unary {
                op: UnaryOp::Not,
                expr: Box::new(expr(ExprKind::String("aura".to_string()))),
            }),
            &mut locals,
        )
        .expect_err("unary not should reject non-bool non-trait types")
        .message
        .contains("`not` expects `bool`"));
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(expr(ExprKind::Name("flag".to_string()))),
                }),
                &mut locals,
            )
            .expect("trait-based unary neg should resolve"),
        Type::named("Flag")
    );
    assert_eq!(
        checker
            .type_of_expr_hint(
                &expr(ExprKind::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(expr(ExprKind::Int(7))),
                }),
                &mut locals,
                Some(&Type::named("int64")),
            )
            .expect("negative integer literals should honor integer hints"),
        Type::named("int64")
    );
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Unary {
                op: UnaryOp::Neg,
                expr: Box::new(expr(ExprKind::String("aura".to_string()))),
            }),
            &mut locals,
        )
        .expect_err("unary neg should reject non-numeric non-trait types")
        .message
        .contains("unary `-` expects a numeric value"));

    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Call {
                callee: Box::new(expr(ExprKind::Member {
                    object: Box::new(expr(ExprKind::Name("group".to_string()))),
                    field: "start".to_string(),
                })),
                args: vec![arg(expr(ExprKind::Name("work".to_string())))],
            }),
            &mut locals,
        )
        .expect_err("TaskGroup.start should enforce callable arguments")
        .message
        .contains("missing required argument `value`"));
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Call {
                    callee: Box::new(expr(ExprKind::Member {
                        object: Box::new(expr(ExprKind::Name("group".to_string()))),
                        field: "start".to_string(),
                    })),
                    args: vec![
                        arg(expr(ExprKind::Name("work".to_string()))),
                        arg(expr(ExprKind::Int(1))),
                    ],
                }),
                &mut locals,
            )
            .expect("task group start should type check"),
        Type::Named("Task".to_string(), vec![Type::named("int32")])
    );
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Call {
                    callee: Box::new(expr(ExprKind::Member {
                        object: Box::new(expr(ExprKind::Name("group".to_string()))),
                        field: "start".to_string(),
                    })),
                    args: vec![
                        arg(expr(ExprKind::Name("work".to_string()))),
                        named_arg("value", expr(ExprKind::Int(1))),
                    ],
                }),
                &mut locals,
            )
            .expect("task group start should forward named arguments to its target"),
        Type::Named("Task".to_string(), vec![Type::named("int32")])
    );
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Call {
                    callee: Box::new(expr(ExprKind::Member {
                        object: Box::new(expr(ExprKind::Name("group".to_string()))),
                        field: "start_soon".to_string(),
                    })),
                    args: vec![
                        arg(expr(ExprKind::Name("work".to_string()))),
                        arg(expr(ExprKind::Int(1))),
                    ],
                }),
                &mut locals,
            )
            .expect("start_soon should erase the Task handle"),
        Type::Unit
    );
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Call {
                    callee: Box::new(expr(ExprKind::Name("wait_any".to_string()))),
                    args: vec![arg(expr(ExprKind::Name("tasks".to_string())))],
                }),
                &mut locals,
            )
            .expect("wait_any should type check"),
        Type::Named("WaitAny".to_string(), vec![Type::named("int32")])
    );
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Call {
                    callee: Box::new(expr(ExprKind::Name("wait_all".to_string()))),
                    args: vec![arg(expr(ExprKind::Name("tasks".to_string())))],
                }),
                &mut locals,
            )
            .expect("wait_all should type check"),
        Type::Named("WaitAll".to_string(), vec![Type::named("int32")])
    );
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Call {
                callee: Box::new(expr(ExprKind::Name("wait_any".to_string()))),
                args: vec![arg(expr(ExprKind::Name("unit_value".to_string())))],
            }),
            &mut locals,
        )
        .expect_err("wait_any should reject non-container task arguments")
        .message
        .contains("`wait_any` expects `list[Task[T]]`, found `None`"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Call {
                callee: Box::new(expr(ExprKind::Name("wait_all".to_string()))),
                args: vec![arg(expr(ExprKind::Name("unit_tasks".to_string())))],
            }),
            &mut locals,
        )
        .expect_err("wait_all should reject list[None] task containers")
        .message
        .contains("`wait_all` expects `list[Task[T]]`, found `list[None]`"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Call {
                callee: Box::new(expr(ExprKind::Name("wait_any".to_string()))),
                args: vec![arg(expr(ExprKind::Name("words".to_string())))],
            }),
            &mut locals,
        )
        .expect_err("wait_any should reject non-task list elements")
        .message
        .contains("`wait_any` expects `list[Task[T]]`, found `list[str]`"));

    checker.current_return_type = Some(result_int_string.clone());
    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Try(Box::new(expr(ExprKind::Name(
                    "result_value".to_string(),
                ))))),
                &mut locals
            )
            .expect("matching Result try expressions should return the success type"),
        Type::named("int32")
    );
    checker.current_return_type = None;
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Try(Box::new(expr(ExprKind::Name(
                "result_value".to_string(),
            ))))),
            &mut locals,
        )
        .expect_err("try outside functions should fail")
        .message
        .contains("only allowed inside a function body"));
    checker.current_return_type = Some(Type::named("int32"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Try(Box::new(expr(ExprKind::Name(
                "result_value".to_string(),
            ))))),
            &mut locals,
        )
        .expect_err("non-Result returns should fail")
        .message
        .contains("enclosing function to return `Result`"));
    checker.current_return_type = Some(Type::Unit);
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Try(Box::new(expr(ExprKind::Name(
                "result_value".to_string(),
            ))))),
            &mut locals,
        )
        .expect_err("non-named returns should fail")
        .message
        .contains("enclosing function to return `Result`"));
    checker.current_return_type = Some(Type::Named(
        "Result".to_string(),
        vec![Type::named("int32"), Type::named("bool")],
    ));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Try(Box::new(expr(ExprKind::Name(
                "result_value".to_string(),
            ))))),
            &mut locals,
        )
        .expect_err("mismatched error types should fail")
        .message
        .contains("does not match enclosing `Result` error type"));
    checker.current_return_type = Some(result_int_string.clone());
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Try(Box::new(expr(ExprKind::Int(1))))),
            &mut locals,
        )
        .expect_err("try requires Result expressions")
        .message
        .contains("`try` requires a `Result[T, E]`"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Try(Box::new(expr(ExprKind::Name(
                "unit_value".to_string(),
            ))))),
            &mut locals,
        )
        .expect_err("try requires named Result expressions")
        .message
        .contains("`try` requires a `Result[T, E]`"));

    assert_eq!(
        checker
            .type_of_expr(
                &expr(ExprKind::Binary {
                    op: BinaryOp::And,
                    left: Box::new(expr(ExprKind::Bool(true))),
                    right: Box::new(expr(ExprKind::Bool(false))),
                }),
                &mut locals,
            )
            .expect("boolean and should type check"),
        Type::named("bool")
    );
    for (left, right) in [
        (ExprKind::Int(1), ExprKind::Float(2.0)),
        (ExprKind::Float(1.0), ExprKind::Int(2)),
    ] {
        assert_eq!(
            checker
                .type_of_expr(
                    &expr(ExprKind::Binary {
                        op: BinaryOp::Add,
                        left: Box::new(expr(left)),
                        right: Box::new(expr(right)),
                    }),
                    &mut locals,
                )
                .expect("an exact integer literal adopts the float operand context"),
            Type::named("float64")
        );
    }

    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("partial".to_string()))),
                field: "value".to_string(),
            }),
            &mut locals,
        )
        .expect_err("moved fields should fail on member access")
        .message
        .contains("use of moved field"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("Option".to_string()))),
                    type_args: vec![type_ref("int32")],
                })),
                field: "Some".to_string(),
            }),
            &mut locals,
        )
        .expect_err("payload variants still require construction")
        .message
        .contains("requires a payload"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("Maybe".to_string()))),
                    type_args: vec![type_ref("int32"), type_ref("str")],
                })),
                field: "Empty".to_string(),
            }),
            &mut locals,
        )
        .expect_err("generic enum arity should be enforced on members too")
        .message
        .contains("enum `Maybe` expects 1 type argument"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("Pair".to_string()))),
                    type_args: vec![type_ref("int32")],
                })),
                field: "Empty".to_string(),
            }),
            &mut locals,
        )
        .expect_err("plural enum arity diagnostics should be covered on members")
        .message
        .contains("enum `Pair` expects 2 type arguments"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("Maybe".to_string()))),
                    type_args: vec![type_ref("int32")],
                })),
                field: "Value".to_string(),
            }),
            &mut locals,
        )
        .expect_err("payload variants should reject bare member access")
        .message
        .contains("requires a payload"));
    assert_eq!(
        checker
            .type_of_expr_hint(
                &expr(ExprKind::Member {
                    object: Box::new(expr(ExprKind::Name("Maybe".to_string()))),
                    field: "Empty".to_string(),
                }),
                &mut locals,
                Some(&Type::Named(
                    "Maybe".to_string(),
                    vec![Type::named("int32")]
                )),
            )
            .expect("expected generic enum hints should flow into bare variants"),
        Type::Named("Maybe".to_string(), vec![Type::named("int32")])
    );
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("Maybe".to_string()))),
                field: "Empty".to_string(),
            }),
            &mut locals,
        )
        .expect_err("generic enum variants without hints should fail inference")
        .message
        .contains("cannot infer type parameter"));

    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Index {
                object: Box::new(expr(ExprKind::Name("words".to_string()))),
                index: Box::new(expr(ExprKind::String("zero".to_string()))),
            }),
            &mut locals,
        )
        .expect_err("list indices should stay integer-only")
        .message
        .contains("list indices must have type `int64` or a losslessly narrower integer type"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Index {
                object: Box::new(expr(ExprKind::Name("scores".to_string()))),
                index: Box::new(expr(ExprKind::Int(0))),
            }),
            &mut locals,
        )
        .expect_err("map indices should honor key types")
        .message
        .contains("map keys must have type"));
    assert!(checker
        .type_of_expr(
            &expr(ExprKind::Index {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                index: Box::new(expr(ExprKind::Int(0))),
            }),
            &mut locals,
        )
        .expect_err("non-indexable values should fail")
        .message
        .contains("cannot index non-Array, list, or dict value"));

    let missing_specialized_variant = checker
        .type_of_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("Maybe".to_string()))),
                    type_args: vec![type_ref("int32")],
                })),
                field: "Missing".to_string(),
            }),
            &mut HashMap::new(),
        )
        .expect_err("unknown specialized enum variants should fail");
    assert!(missing_specialized_variant
        .message
        .contains("enum `Maybe` has no variant `Missing`"));

    let specialized_payload_required = checker
        .type_of_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("Maybe".to_string()))),
                    type_args: vec![type_ref("int32")],
                })),
                field: "Value".to_string(),
            }),
            &mut HashMap::new(),
        )
        .expect_err("payload variants should reject field-style access");
    assert!(specialized_payload_required
        .message
        .contains("variant `Value` of enum `Maybe` requires a payload"));

    let missing_variant = checker
        .type_of_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("Maybe".to_string()))),
                field: "Missing".to_string(),
            }),
            &mut HashMap::new(),
        )
        .expect_err("unknown enum variants should fail");
    assert!(missing_variant
        .message
        .contains("enum `Maybe` has no variant `Missing`"));

    let payload_required = checker
        .type_of_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("Maybe".to_string()))),
                field: "Value".to_string(),
            }),
            &mut HashMap::new(),
        )
        .expect_err("payload variants should reject field-style access");
    assert!(payload_required
        .message
        .contains("variant `Value` of enum `Maybe` requires a payload"));
}

#[test]
fn checker_assignment_helper_paths_cover_index_member_and_binding_edges() {
    let classes = BTreeMap::from([(
        "Counter".to_string(),
        class_info(
            "Counter",
            false,
            vec![
                ("value", Type::named("int32"), false),
                ("text", Type::named("str"), false),
            ],
        ),
    )]);
    let type_names = BTreeMap::from([("Counter".to_string(), Span::new(1, 1))]);
    let type_arities = BTreeMap::from([("Counter".to_string(), 0usize)]);
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let vec_int = Type::Named("list".to_string(), vec![Type::named("int32")]);
    let map_string_int = Type::Named(
        "dict".to_string(),
        vec![Type::named("str"), Type::named("int32")],
    );

    let mut locals = HashMap::from([(
        "nums".to_string(),
        local_binding(
            vec_int.clone(),
            true,
            false,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Index {
                    object: Box::new(expr(ExprKind::Name("nums".to_string()))),
                    index: Box::new(expr(ExprKind::Int(0))),
                },
                false,
                None,
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("immutable indexed places should fail")
        .message
        .contains("cannot assign through immutable place"));

    let mut locals = HashMap::from([(
        "nums".to_string(),
        local_binding(vec_int.clone(), true, true, ReceiverKind::Value, false, &[]),
    )]);
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Index {
                    object: Box::new(expr(ExprKind::Name("nums".to_string()))),
                    index: Box::new(expr(ExprKind::String("zero".to_string()))),
                },
                false,
                None,
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("list indices should stay integer-only in assignments")
        .message
        .contains("list indices must have type `int64` or a losslessly narrower integer type"));

    let mut locals = HashMap::from([(
        "scores".to_string(),
        local_binding(
            map_string_int.clone(),
            true,
            true,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Index {
                    object: Box::new(expr(ExprKind::Name("scores".to_string()))),
                    index: Box::new(expr(ExprKind::Int(0))),
                },
                false,
                None,
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("map keys should honor their declared type")
        .message
        .contains("map keys must have type"));

    let mut locals = HashMap::from([(
        "text".to_string(),
        local_binding(
            Type::named("str"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Index {
                    object: Box::new(expr(ExprKind::Name("text".to_string()))),
                    index: Box::new(expr(ExprKind::Int(0))),
                },
                false,
                None,
                None,
                expr(ExprKind::String("x".to_string())),
            ),
            &mut locals,
        )
        .expect_err("non-indexable assignment targets should fail")
        .message
        .contains("cannot index non-Array, list, or dict value"));

    let mut locals = HashMap::from([(
        "nums".to_string(),
        local_binding(vec_int.clone(), true, true, ReceiverKind::Value, false, &[]),
    )]);
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Index {
                    object: Box::new(expr(ExprKind::Name("nums".to_string()))),
                    index: Box::new(expr(ExprKind::Int(0))),
                },
                false,
                None,
                None,
                expr(ExprKind::String("x".to_string())),
            ),
            &mut locals,
        )
        .expect_err("indexed assignment types should match")
        .message
        .contains("cannot assign value of type `str` to indexed element of type `int32`"));

    let mut locals = HashMap::from([(
        "counter".to_string(),
        local_binding(
            Type::named("Counter"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &["value"],
        ),
    )]);
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Member {
                    object: Box::new(expr(ExprKind::Name("counter".to_string()))),
                    field: "value".to_string(),
                },
                false,
                None,
                Some(BinaryOp::Add),
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("compound member writes should reject moved fields")
        .message
        .contains("cannot read moved field `value`"));

    let mut locals = HashMap::from([(
        "counter".to_string(),
        local_binding(
            Type::named("Counter"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &["value"],
        ),
    )]);
    checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Member {
                    object: Box::new(expr(ExprKind::Name("counter".to_string()))),
                    field: "value".to_string(),
                },
                false,
                None,
                None,
                expr(ExprKind::Int(7)),
            ),
            &mut locals,
        )
        .expect("plain member reassignment should clear moved field paths");
    assert!(locals
        .get("counter")
        .expect("counter binding should remain")
        .moved_fields
        .is_empty());

    let mut locals = HashMap::from([(
        "total".to_string(),
        local_binding(
            Type::named("int32"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Name("total".to_string()),
                true,
                None,
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("mut cannot redeclare existing bindings")
        .message
        .contains("`total` is already declared"));
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Name("total".to_string()),
                false,
                Some(type_ref("int32")),
                Some(BinaryOp::Add),
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("compound assignment annotations should fail")
        .message
        .contains("cannot include a type annotation"));

    let mut locals = HashMap::from([(
        "locked".to_string(),
        local_binding(
            Type::named("int32"),
            false,
            false,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Name("locked".to_string()),
                false,
                None,
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("immutable bindings should reject reassignment")
        .message
        .contains("cannot assign to immutable binding `locked`"));

    let mut locals = HashMap::from([(
        "moved".to_string(),
        local_binding(
            Type::named("int32"),
            true,
            true,
            ReceiverKind::Value,
            true,
            &[],
        ),
    )]);
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Name("moved".to_string()),
                false,
                None,
                Some(BinaryOp::Add),
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("compound assignment should reject moved bindings")
        .message
        .contains("cannot read moved value `moved`"));

    let mut locals = HashMap::from([(
        "typed".to_string(),
        local_binding(
            Type::named("int32"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Name("typed".to_string()),
                false,
                Some(type_ref("str")),
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("reassignment annotations should match the existing type")
        .message
        .contains("reassignment annotation for `typed` has type `str`, expected `int32`"));

    let mut locals = HashMap::from([(
        "typed".to_string(),
        local_binding(
            Type::named("int32"),
            true,
            true,
            ReceiverKind::Value,
            true,
            &["value"],
        ),
    )]);
    checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Name("typed".to_string()),
                false,
                None,
                None,
                expr(ExprKind::Int(3)),
            ),
            &mut locals,
        )
        .expect("plain reassignment should clear moved state");
    let typed = locals.get("typed").expect("typed binding should remain");
    assert!(!typed.moved);
    assert!(typed.moved_fields.is_empty());

    let mut locals = HashMap::new();
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Name("fresh".to_string()),
                false,
                None,
                Some(BinaryOp::Add),
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("compound assignment needs an existing binding")
        .message
        .contains("compound assignment requires an existing mutable binding `fresh`"));
    assert!(checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Name("fresh".to_string()),
                false,
                Some(type_ref("str")),
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("new bindings should honor their annotations")
        .message
        .contains("binding `fresh` has annotated type `str`, but value has type `int64`"));
    checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Name("fresh".to_string()),
                true,
                None,
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect("new mutable bindings should insert locals");
    let fresh = locals
        .get("fresh")
        .expect("fresh binding should be inserted");
    assert_eq!(fresh.ty, Type::named("int64"));
    assert!(fresh.assignable);
    assert!(fresh.mutable_place);

    let mut locals = HashMap::from([(
        "counter".to_string(),
        local_binding(
            Type::named("Counter"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    let member_type_mismatch = checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Member {
                    object: Box::new(expr(ExprKind::Name("counter".to_string()))),
                    field: "value".to_string(),
                },
                false,
                None,
                None,
                expr(ExprKind::String("oops".to_string())),
            ),
            &mut locals,
        )
        .expect_err("member assignments should enforce field types");
    assert!(member_type_mismatch
        .message
        .contains("cannot assign value of type `str` to member `counter.value` of type `int32`"));
}

#[test]
fn checker_call_surface_helpers_cover_builtin_constructors_and_builtin_calls() {
    let mut box_class = class_info(
        "Box",
        false,
        vec![("value", Type::TypeParam("T".to_string()), false)],
    );
    box_class.decl.type_params = vec!["T".to_string()];
    let mut phantom_class = class_info(
        "Phantom",
        false,
        vec![("value", Type::named("int32"), false)],
    );
    phantom_class.decl.type_params = vec!["T".to_string()];

    let classes = BTreeMap::from([
        ("Box".to_string(), box_class),
        ("Phantom".to_string(), phantom_class),
    ]);
    let type_names = BTreeMap::from([
        ("Box".to_string(), Span::new(1, 1)),
        ("Phantom".to_string(), Span::new(1, 1)),
    ]);
    let type_arities =
        BTreeMap::from([("Box".to_string(), 1usize), ("Phantom".to_string(), 1usize)]);
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let span = Span::new(1, 1);
    let channel_string = Type::Named("Queue".to_string(), vec![Type::named("str")]);
    let mut locals = HashMap::from([
        (
            "text".to_string(),
            local_binding(
                Type::named("str"),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "count".to_string(),
            local_binding(
                Type::named("int32"),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "tasks".to_string(),
            local_binding(
                Type::Named(
                    "list".to_string(),
                    vec![Type::Named("Task".to_string(), vec![Type::named("int32")])],
                ),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "numbers".to_string(),
            local_binding(
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "generic_tasks".to_string(),
            local_binding(
                Type::Named("list".to_string(), vec![Type::TypeParam("T".to_string())]),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "queued_tasks".to_string(),
            local_binding(
                Type::Named(
                    "Queue".to_string(),
                    vec![Type::Named("Task".to_string(), vec![Type::named("int32")])],
                ),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
    ]);

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("Queue".to_string()))),
                type_args: vec![type_ref("str"), type_ref("int32")],
            }),
            &[],
            span,
            &mut locals,
            None,
        )
        .expect_err("Queue arity mismatches should fail")
        .message
        .contains("expects exactly one type argument"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("Queue".to_string()))),
                type_args: vec![type_ref("str")],
            }),
            &[named_arg(
                "capacity",
                expr(ExprKind::String("large".to_string()))
            )],
            span,
            &mut locals,
            None,
        )
        .expect_err("Queue capacity should stay int32")
        .message
        .contains("field `capacity` expects `int32`"));
    assert_eq!(
        checker
            .type_of_call(
                &expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("Queue".to_string()))),
                    type_args: vec![type_ref("str")],
                }),
                &[named_arg("capacity", expr(ExprKind::Int(4)))],
                span,
                &mut locals,
                None,
            )
            .expect("Queue[T](capacity=...) should type check"),
        channel_string
    );

    for (name, type_args, expected_fragment) in [
        (
            "list",
            vec![type_ref("int32"), type_ref("str")],
            "expects exactly one type argument",
        ),
        (
            "set",
            vec![type_ref("int32"), type_ref("str")],
            "expects exactly one type argument",
        ),
    ] {
        assert!(checker
            .type_of_call(
                &expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name(name.to_string()))),
                    type_args,
                }),
                &[],
                span,
                &mut locals,
                None,
            )
            .expect_err("collection constructor arity mismatches should fail")
            .message
            .contains(expected_fragment));
    }
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("list".to_string()))),
                type_args: vec![type_ref("int32")],
            }),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("Vec constructors stay argument-free")
        .message
        .contains("does not take constructor arguments"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("set".to_string()))),
                type_args: vec![type_ref("int32")],
            }),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("Set constructors stay argument-free")
        .message
        .contains("does not take constructor arguments"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("dict".to_string()))),
                type_args: vec![type_ref("str")],
            }),
            &[],
            span,
            &mut locals,
            None,
        )
        .expect_err("Map arity mismatches should fail")
        .message
        .contains("expects exactly two type arguments"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("dict".to_string()))),
                type_args: vec![type_ref("str"), type_ref("int32")],
            }),
            &[named_arg("value", expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("Map constructors stay argument-free")
        .message
        .contains("does not take constructor arguments"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("TaskGroup".to_string()))),
                type_args: vec![type_ref("int32")],
            }),
            &[],
            span,
            &mut locals,
            None,
        )
        .expect_err("TaskGroup should reject explicit type args")
        .message
        .contains("does not take type arguments"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("TaskGroup".to_string()))),
                type_args: Vec::new(),
            }),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("TaskGroup should reject constructor args")
        .message
        .contains("does not take constructor arguments"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("range".to_string())),
            &[arg(expr(ExprKind::String("bad".to_string())))],
            span,
            &mut locals,
            None,
        )
        .expect_err("range arguments stay int32-only")
        .message
        .contains(
            "`range` arguments must have type `int64` or a losslessly narrower integer type"
        ));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("wait_any".to_string())),
            &[arg(expr(ExprKind::Name("count".to_string())))],
            span,
            &mut locals,
            None,
        )
        .expect_err("wait_any() requires a list[Task[T]]")
        .message
        .contains("expects `list[Task[T]]`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("wait_any".to_string())),
            &[arg(expr(ExprKind::Name("queued_tasks".to_string())))],
            span,
            &mut locals,
            None,
        )
        .expect_err("wait_any() requires list rather than Queue")
        .message
        .contains("expects `list[Task[T]]`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("wait_any".to_string())),
            &[arg(expr(ExprKind::Name("generic_tasks".to_string())))],
            span,
            &mut locals,
            None,
        )
        .expect_err("wait_any() requires list[Task[T]], not list[T]")
        .message
        .contains("expects `list[Task[T]]`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("wait_any".to_string())),
            &[arg(expr(ExprKind::Name("numbers".to_string())))],
            span,
            &mut locals,
            None,
        )
        .expect_err("wait_any() requires task elements")
        .message
        .contains("expects `list[Task[T]]`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("wait_all".to_string())),
            &[
                arg(expr(ExprKind::Name("tasks".to_string()))),
                named_arg("timeout", expr(ExprKind::Int(1))),
            ],
            span,
            &mut locals,
            None,
        )
        .expect_err("wait_all() timeout requires Duration")
        .message
        .contains("`wait_all(timeout=...)` expects `Duration`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("sleep".to_string())),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("sleep() requires Duration")
        .message
        .contains("expects a `Duration`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("abs".to_string())),
            &[arg(expr(ExprKind::String("bad".to_string())))],
            span,
            &mut locals,
            None,
        )
        .expect_err("abs() stays numeric-only")
        .message
        .contains("expects an integer or float value"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("min".to_string())),
            &[
                arg(expr(ExprKind::String("bad".to_string()))),
                arg(expr(ExprKind::Int(1)))
            ],
            span,
            &mut locals,
            None,
        )
        .expect_err("min() rejects non-numeric left operands")
        .message
        .contains("expects numeric arguments"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("max".to_string())),
            &[arg(expr(ExprKind::Int(1))), arg(expr(ExprKind::Float(1.0)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("min/max arguments must match")
        .message
        .contains("arguments must match"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("sqrt".to_string())),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("sqrt() is float-only")
        .message
        .contains("expects `float32` or `float64`"));
    for builtin in ["parse_int32", "parse_int64", "parse_float64"] {
        assert!(checker
            .type_of_call(
                &expr(ExprKind::Name(builtin.to_string())),
                &[arg(expr(ExprKind::Int(1)))],
                span,
                &mut locals,
                None,
            )
            .expect_err("parse helpers stay string-only")
            .message
            .contains("expects `str`"));
    }

    assert_eq!(
        checker
            .type_of_call(
                &expr(ExprKind::Name("Box".to_string())),
                &[arg(expr(ExprKind::Int(1)))],
                span,
                &mut locals,
                None,
            )
            .expect("positional class constructors should infer the field type"),
        Type::Named("Box".to_string(), vec![Type::named("int64")])
    );
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("Box".to_string())),
            &[named_arg("missing", expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("unknown constructor fields should fail")
        .message
        .contains("has no field named `missing`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("Box".to_string())),
            &[
                named_arg("value", expr(ExprKind::Int(1))),
                named_arg("value", expr(ExprKind::Int(2))),
            ],
            span,
            &mut locals,
            None,
        )
        .expect_err("duplicate constructor fields should fail")
        .message
        .contains("provided more than once"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("Box".to_string())),
            &[],
            span,
            &mut locals,
            Some(&Type::Named("Box".to_string(), vec![Type::named("int32")])),
        )
        .expect_err("required constructor fields should stay required")
        .message
        .contains("missing required field `value`"));
    assert_eq!(
        checker
            .type_of_call(
                &expr(ExprKind::Name("Box".to_string())),
                &[named_arg("value", expr(ExprKind::Int(1)))],
                span,
                &mut locals,
                None,
            )
            .expect("generic class constructors should infer type parameters from fields"),
        Type::Named("Box".to_string(), vec![Type::named("int64")])
    );
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("Box".to_string()))),
                type_args: vec![type_ref("str")],
            }),
            &[named_arg("value", expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("explicit generic constructors should honor their field type")
        .message
        .contains("field `value` expects `str`, found `int64`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("Phantom".to_string())),
            &[named_arg("value", expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("unused generic parameters should still need inference")
        .message
        .contains("cannot infer type parameter `T` for class constructor `Phantom`"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("min".to_string())),
            &[
                arg(expr(ExprKind::Name("text".to_string()))),
                arg(expr(ExprKind::Name("text".to_string()))),
            ],
            span,
            &mut locals,
            None,
        )
        .expect_err("min should reject non-numeric matching arguments")
        .message
        .contains("`min` expects numeric arguments, found `str`"));
}

#[test]
fn checker_user_call_argument_mismatch_reports_direct_callable_mismatch() {
    let error = crate::check_source(
        r#"
def takes_count(value: int32) -> None:
    pass

def main() -> None:
    takes_count("bad")
"#,
    )
    .expect_err("ordinary callable arguments should enforce declared parameter types");

    assert!(error.message.contains(
        "argument type mismatch for function `takes_count`: expected `int32`, found `str`"
    ));
}

#[test]
fn checker_type_of_call_covers_associated_methods_generic_variants_and_private_fields() {
    let span = Span::new(1, 1);

    let mut widget = class_info(
        "Widget",
        false,
        vec![("value", Type::named("int32"), false)],
    );
    widget.methods.insert(
        "build".to_string(),
        MethodInfo {
            decl: {
                let mut decl = function_decl("build");
                decl.return_type = type_ref("int32");
                decl
            },
            signature: function_signature(Vec::new(), Type::named("int32")),
            type_param_bounds: BTreeMap::new(),
        },
    );
    widget.methods.insert(
        "touch".to_string(),
        MethodInfo {
            decl: {
                let mut decl = function_decl("touch");
                decl.receiver = Some(ReceiverKind::Borrow);
                decl.return_type = type_ref("int32");
                decl
            },
            signature: function_signature(Vec::new(), Type::named("int32")),
            type_param_bounds: BTreeMap::new(),
        },
    );

    let mut secret_box = class_info(
        "SecretBox",
        false,
        vec![("secret", Type::named("int32"), false)],
    );
    secret_box.module_name = "pkg.lib".to_string();
    secret_box.decl.fields[0].public = false;
    secret_box
        .fields
        .get_mut("secret")
        .expect("secret field should exist")
        .public = false;

    let mut status = enum_info("Status", Some(Type::TypeParam("T".to_string())));
    status.decl.type_params = vec!["T".to_string()];
    status.variants.insert(
        "Ready".to_string(),
        EnumVariantInfo {
            payloads: Vec::new(),
            named_payloads: false,
            span,
        },
    );
    status.decl.variants.push(crate::ast::EnumVariantDecl {
        name: "Ready".to_string(),
        payloads: Vec::new(),
        named_payloads: false,
        span,
    });

    let mut shape = enum_info("Shape", None);
    shape.decl.variants = vec![crate::ast::EnumVariantDecl {
        name: "Point".to_string(),
        payloads: vec![
            crate::ast::EnumPayloadFieldDecl {
                name: Some("x".to_string()),
                ty: type_ref("int32"),
                span,
            },
            crate::ast::EnumPayloadFieldDecl {
                name: Some("y".to_string()),
                ty: type_ref("int32"),
                span,
            },
        ],
        named_payloads: true,
        span,
    }];
    shape.variants = BTreeMap::from([(
        "Point".to_string(),
        EnumVariantInfo {
            payloads: vec![
                EnumPayloadFieldInfo {
                    name: Some("x".to_string()),
                    ty: Type::named("int32"),
                    span,
                },
                EnumPayloadFieldInfo {
                    name: Some("y".to_string()),
                    ty: Type::named("int32"),
                    span,
                },
            ],
            named_payloads: true,
            span,
        },
    )]);

    let classes = BTreeMap::from([
        ("Widget".to_string(), widget),
        ("SecretBox".to_string(), secret_box),
    ]);
    let enums = BTreeMap::from([("Shape".to_string(), shape), ("Status".to_string(), status)]);
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let type_names = BTreeMap::from([
        ("Widget".to_string(), span),
        ("SecretBox".to_string(), span),
        ("Shape".to_string(), span),
        ("Status".to_string(), span),
    ]);
    let type_arities = BTreeMap::from([
        ("Widget".to_string(), 0usize),
        ("SecretBox".to_string(), 0usize),
        ("Shape".to_string(), 0usize),
        ("Status".to_string(), 1usize),
    ]);
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let mut locals = HashMap::from([
        (
            "flag".to_string(),
            local_binding(
                Type::named("bool"),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "widget".to_string(),
            local_binding(
                Type::named("Widget"),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
    ]);

    assert_eq!(
        checker
            .type_of_call(
                &expr(ExprKind::Member {
                    object: Box::new(expr(ExprKind::Name("Widget".to_string()))),
                    field: "build".to_string(),
                }),
                &[],
                span,
                &mut locals,
                None,
            )
            .expect("associated class methods should type check"),
        Type::named("int32")
    );
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("widget".to_string()))),
                field: "build".to_string(),
            }),
            &[],
            span,
            &mut locals,
            None,
        )
        .expect_err("associated methods should require class-name calls")
        .message
        .contains(
            "associated method `build` on class `Widget` must be called through the class name"
        ));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("Widget".to_string()))),
                field: "touch".to_string(),
            }),
            &[],
            span,
            &mut locals,
            None,
        )
        .expect_err("receiver methods should require instances when called from class names")
        .message
        .contains("requires an instance receiver"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("Widget".to_string())),
            &[arg(expr(ExprKind::Int(1))), arg(expr(ExprKind::Int(2)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("class constructors should reject extra positional arguments")
        .message
        .contains("received too many positional arguments"));

    let status_ready = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Name("Status".to_string()))),
        field: "Ready".to_string(),
    });
    let status_value = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Name("Status".to_string()))),
        field: "Value".to_string(),
    });
    let specialized_status_value = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Specialize {
            expr: Box::new(expr(ExprKind::Name("Status".to_string()))),
            type_args: vec![type_ref("int32")],
        })),
        field: "Value".to_string(),
    });

    assert!(checker
        .type_of_expr(&status_value, &mut locals)
        .expect_err("payload variants used as values should require construction")
        .message
        .contains("requires a payload"));
    assert_eq!(
        checker
            .type_of_call(
                &status_ready,
                &[],
                span,
                &mut locals,
                Some(&Type::Named(
                    "Status".to_string(),
                    vec![Type::named("int32")]
                )),
            )
            .expect("payload-free generic variants should follow expected types"),
        Type::Named("Status".to_string(), vec![Type::named("int32")])
    );
    assert!(checker
        .type_of_call(&status_ready, &[], span, &mut locals, None)
        .expect_err("generic payload-free variants should still need inference")
        .message
        .contains("cannot infer type parameter `T` for enum variant `Status.Ready`"));
    assert_eq!(
        checker
            .type_of_call(
                &status_value,
                &[named_arg("value", expr(ExprKind::Int(1)))],
                span,
                &mut locals,
                Some(&Type::Named(
                    "Status".to_string(),
                    vec![Type::named("int32")]
                )),
            )
            .expect("enum variant constructors should accept `value=` for single payload variants"),
        Type::Named("Status".to_string(), vec![Type::named("int32")])
    );
    assert!(checker
        .type_of_call(&specialized_status_value, &[], span, &mut locals, None,)
        .expect_err("payload variants require exactly one argument")
        .message
        .contains("payload"));
    assert!(checker
        .type_of_call(
            &specialized_status_value,
            &[arg(expr(ExprKind::Int(1))), arg(expr(ExprKind::Int(2)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("single-payload variants should reject extra payloads")
        .message
        .contains("expects 1 payload argument"));
    assert!(checker
        .type_of_call(
            &specialized_status_value,
            &[named_arg("item", expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("single-payload variants should only accept value=")
        .message
        .contains("only accepts the keyword `value=`"));
    assert!(checker
        .type_of_call(
            &specialized_status_value,
            &[arg(expr(ExprKind::Bool(true)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("payload variants should enforce specialized payload types")
        .message
        .contains("expects `int32`, found `bool`"));
    assert_eq!(
        checker
            .type_of_call(
                &specialized_status_value,
                &[arg(expr(ExprKind::Int(1)))],
                span,
                &mut locals,
                None,
            )
            .expect("specialized generic enum constructors should type check"),
        Type::Named("Status".to_string(), vec![Type::named("int32")])
    );

    let shape_point = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Name("Shape".to_string()))),
        field: "Point".to_string(),
    });
    assert_eq!(
        checker
            .type_of_call(
                &shape_point,
                &[
                    named_arg("x", expr(ExprKind::Int(1))),
                    named_arg("y", expr(ExprKind::Int(2))),
                ],
                span,
                &mut locals,
                None,
            )
            .expect("named enum payload constructors should type check"),
        Type::named("Shape")
    );
    assert_eq!(
        checker
            .type_of_call(
                &shape_point,
                &[arg(expr(ExprKind::Int(1))), arg(expr(ExprKind::Int(2)))],
                span,
                &mut locals,
                None,
            )
            .expect("named enum payloads should still accept positional construction"),
        Type::named("Shape")
    );
    assert!(checker
        .type_of_call(
            &shape_point,
            &[named_arg("x", expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("named enum payloads should require all fields")
        .message
        .contains("expects 2 payload arguments, found 1"));
    assert!(checker
        .type_of_call(
            &shape_point,
            &[
                named_arg("x", expr(ExprKind::Int(1))),
                named_arg("z", expr(ExprKind::Int(2))),
            ],
            span,
            &mut locals,
            None,
        )
        .expect_err("named enum payloads should require declared names")
        .message
        .contains("is missing payload argument `y`"));
    assert!(checker
        .type_of_call(
            &shape_point,
            &[
                named_arg("x", expr(ExprKind::Int(1))),
                named_arg("y", expr(ExprKind::Int(2))),
                named_arg("z", expr(ExprKind::Int(3))),
            ],
            span,
            &mut locals,
            None,
        )
        .expect_err("named enum payloads should reject extra names after required fields")
        .message
        .contains("has no payload named `z`"));
    assert!(checker
        .type_of_call(
            &shape_point,
            &[
                named_arg("x", expr(ExprKind::Int(1))),
                named_arg("y", expr(ExprKind::Bool(true))),
            ],
            span,
            &mut locals,
            None,
        )
        .expect_err("named enum payloads should enforce field types")
        .message
        .contains("expects `int32`, found `bool`"));
    assert!(checker
        .type_of_call(
            &status_ready,
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            Some(&Type::Named(
                "Status".to_string(),
                vec![Type::named("int32")]
            )),
        )
        .expect_err("payload-free variants should reject arguments")
        .message
        .contains("does not take a payload"));

    let option_some = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Specialize {
            expr: Box::new(expr(ExprKind::Name("Option".to_string()))),
            type_args: vec![type_ref("int32")],
        })),
        field: "Some".to_string(),
    });
    let option_none = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Specialize {
            expr: Box::new(expr(ExprKind::Name("Option".to_string()))),
            type_args: vec![type_ref("int32")],
        })),
        field: "None".to_string(),
    });
    assert_eq!(
        checker
            .type_of_call(
                &option_some,
                &[named_arg("value", expr(ExprKind::Int(1)))],
                span,
                &mut locals,
                None,
            )
            .expect("builtin enum constructors should accept `value=`"),
        Type::Named("Option".to_string(), vec![Type::named("int32")])
    );
    assert!(checker
        .type_of_call(&option_some, &[], span, &mut locals, None)
        .expect_err("Option.Some still requires a payload")
        .message
        .contains("payload"));
    let inferred_option_some = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Name("Option".to_string()))),
        field: "Some".to_string(),
    });
    let inferred_option_none = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Name("Option".to_string()))),
        field: "None".to_string(),
    });
    assert!(checker
        .type_of_call(&inferred_option_some, &[], span, &mut locals, None)
        .expect_err("unqualified Option.Some should still require a payload")
        .message
        .contains("expects 1 payload argument, found 0"));
    assert!(checker
        .type_of_expr(&inferred_option_none, &mut locals)
        .expect_err("bare Option.None needs an expected type")
        .message
        .contains("cannot infer type parameter `T`"));
    assert!(checker
        .type_of_expr_hint(
            &inferred_option_some,
            &mut locals,
            Some(&Type::Named(
                "Option".to_string(),
                vec![Type::named("int32")]
            )),
        )
        .expect_err("payload-bearing builtin variants should not be used as values")
        .message
        .contains("requires a payload"));
    assert_eq!(
        checker
            .type_of_call(&option_none, &[], span, &mut locals, None)
            .expect("Option.None with explicit type args should type check"),
        Type::Named("Option".to_string(), vec![Type::named("int32")])
    );

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("SecretBox".to_string())),
            &[named_arg("secret", expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("external private constructor fields should be rejected")
        .message
        .contains("field `secret` is private on `SecretBox`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Name("SecretBox".to_string())),
            &[],
            span,
            &mut locals,
            None,
        )
        .expect_err("external private required fields should not be inferred")
        .message
        .contains("cannot initialize private field `secret` from another module"));
}

#[test]
fn checker_member_call_helpers_cover_string_map_set_and_channel_builtins() {
    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();
    let classes = BTreeMap::new();
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let span = Span::new(1, 1);
    let string_ty = Type::named("str");
    let vec_string = Type::Named("list".to_string(), vec![string_ty.clone()]);
    let map_string = Type::Named(
        "dict".to_string(),
        vec![string_ty.clone(), string_ty.clone()],
    );
    let set_string = Type::Named("set".to_string(), vec![string_ty.clone()]);
    let channel_string = Type::Named("Queue".to_string(), vec![string_ty.clone()]);
    let mut locals = HashMap::from([
        (
            "text".to_string(),
            local_binding(
                string_ty.clone(),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "parts".to_string(),
            local_binding(
                vec_string.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "mapping".to_string(),
            local_binding(
                map_string.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "mutable_mapping".to_string(),
            local_binding(
                map_string.clone(),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "items".to_string(),
            local_binding(
                set_string.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "mutable_items".to_string(),
            local_binding(
                set_string.clone(),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "queue".to_string(),
            local_binding(
                channel_string.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
    ]);

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "join".to_string(),
            }),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("join() expects list[str]")
        .message
        .contains("`join` expects `list[str]`"));
    assert_eq!(
        checker
            .type_of_call(
                &expr(ExprKind::Member {
                    object: Box::new(expr(ExprKind::Name("text".to_string()))),
                    field: "join".to_string(),
                }),
                &[arg(expr(ExprKind::Name("parts".to_string())))],
                span,
                &mut locals,
                None,
            )
            .expect("join() should accept list[str]"),
        string_ty
    );
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "strip_prefix".to_string(),
            }),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("strip_prefix() expects str")
        .message
        .contains("expects `str`"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "get".to_string(),
            }),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("map.get() should enforce key type")
        .message
        .contains("`get` expects `str`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mutable_mapping".to_string()))),
                field: "remove".to_string(),
            }),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("map.remove() should enforce key types")
        .message
        .contains("`remove` expects `str`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "clear".to_string(),
            }),
            &[],
            span,
            &mut locals,
            None,
        )
        .expect_err("map.clear() requires a mutable receiver")
        .message
        .contains("requires a mutable receiver"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mutable_mapping".to_string()))),
                field: "update".to_string(),
            }),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("dict.update() should enforce dict types")
        .message
        .contains("`update` expects `dict[str, str]`"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("items".to_string()))),
                field: "add".to_string(),
            }),
            &[arg(expr(ExprKind::String("aura".to_string())))],
            span,
            &mut locals,
            None,
        )
        .expect_err("set.add() requires a mutable receiver")
        .message
        .contains("requires a mutable receiver"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mutable_items".to_string()))),
                field: "add".to_string(),
            }),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("set.add() should enforce element types")
        .message
        .contains("`add` expects `str`"));
    assert_eq!(
        checker
            .type_of_call(
                &expr(ExprKind::Member {
                    object: Box::new(expr(ExprKind::Name("mutable_items".to_string()))),
                    field: "remove".to_string(),
                }),
                &[arg(expr(ExprKind::String("aura".to_string())))],
                span,
                &mut locals,
                None,
            )
            .expect("set.remove() should type check on mutable sets"),
        Type::Unit
    );

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "put".to_string(),
            }),
            &[arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("channel.put() should enforce payload types")
        .message
        .contains("`put` expects `str`"));
}

#[test]
fn checker_member_call_helpers_cover_successful_string_vec_map_and_runtime_surfaces() {
    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();
    let classes = BTreeMap::new();
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let span = Span::new(1, 1);
    let string_ty = Type::named("str");
    let int_ty = Type::named("int32");
    let count_ty = Type::named("int64");
    let float_ty = Type::named("float64");
    let bool_ty = Type::named("bool");
    let vec_int = Type::Named("list".to_string(), vec![int_ty.clone()]);
    let vec_bool = Type::Named("list".to_string(), vec![bool_ty.clone()]);
    let bytes_ty = Type::Named("list".to_string(), vec![Type::named("uint8")]);
    let headers_ty = Type::Named(
        "dict".to_string(),
        vec![string_ty.clone(), string_ty.clone()],
    );
    let map_ty = Type::Named("dict".to_string(), vec![string_ty.clone(), int_ty.clone()]);
    let set_ty = Type::Named("set".to_string(), vec![string_ty.clone()]);
    let channel_ty = Type::Named("Queue".to_string(), vec![string_ty.clone()]);
    let task_ty = Type::Named("Task".to_string(), vec![int_ty.clone()]);
    let result_ty = |ok: Type| {
        Type::Named(
            "Result".to_string(),
            vec![ok, crate::builtin_modules::io_error_type()],
        )
    };
    let process_result_ty = |ok: Type| {
        Type::Named(
            "Result".to_string(),
            vec![ok, crate::builtin_modules::process_error_type()],
        )
    };
    let option_ty = |inner: Type| Type::Named("Option".to_string(), vec![inner]);
    let bytes_expr = || expr(ExprKind::List(vec![expr(ExprKind::Int(1))]));
    let headers_expr = || {
        expr(ExprKind::Map(vec![MapEntryExpr {
            key: expr(ExprKind::String("content-type".to_string())),
            value: expr(ExprKind::String("text/plain".to_string())),
        }]))
    };
    let timeout_arg = || arg(expr(ExprKind::DurationNanos(1_000_000)));
    let mut locals = HashMap::from([
        (
            "number".to_string(),
            local_binding(
                int_ty.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "ratio".to_string(),
            local_binding(
                float_ty.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "flag".to_string(),
            local_binding(
                bool_ty.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "text".to_string(),
            local_binding(
                string_ty.clone(),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "values".to_string(),
            local_binding(vec_int.clone(), true, true, ReceiverKind::Value, false, &[]),
        ),
        (
            "immutable_values".to_string(),
            local_binding(
                vec_int.clone(),
                true,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "bad_bytes".to_string(),
            local_binding(vec_bool, false, false, ReceiverKind::Value, false, &[]),
        ),
        (
            "mapping".to_string(),
            local_binding(map_ty.clone(), true, true, ReceiverKind::Value, false, &[]),
        ),
        (
            "items".to_string(),
            local_binding(set_ty.clone(), true, true, ReceiverKind::Value, false, &[]),
        ),
        (
            "queue".to_string(),
            local_binding(
                channel_ty.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "task".to_string(),
            local_binding(
                task_ty.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "group".to_string(),
            local_binding(
                Type::named("TaskGroup"),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
    ]);
    for (name, ty) in [
        ("tcp_listener", Type::named("net.TcpListener")),
        ("tcp_stream", Type::named("net.TcpStream")),
        ("udp_socket", Type::named("net.UdpSocket")),
        ("udp_datagram", Type::named("net.UdpDatagram")),
        ("http_listener", Type::named("net.HttpListener")),
        ("http_exchange", Type::named("net.HttpExchange")),
        ("http_response", Type::named("net.HttpResponse")),
        ("websocket_listener", Type::named("net.WebSocketListener")),
        ("websocket", Type::named("net.WebSocket")),
        ("unix_listener", Type::named("net.UnixListener")),
        ("unix_stream", Type::named("net.UnixStream")),
        ("tls_listener", Type::named("net.TlsListener")),
        ("tls_stream", Type::named("net.TlsStream")),
        ("child", Type::named("process.Child")),
        ("pipe", Type::named("process.Pipe")),
        ("completed", Type::named("process.Completed")),
        ("supervisor", Type::named("process.Supervisor")),
    ] {
        locals.insert(
            name.to_string(),
            local_binding(ty, false, false, ReceiverKind::Value, false, &[]),
        );
    }
    locals.insert(
        "file".to_string(),
        local_binding(
            Type::named("fs.File"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &[],
        ),
    );

    for (callee, args, expected) in [
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("number".to_string()))),
                field: "to_string".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("ratio".to_string()))),
                field: "sqrt".to_string(),
            }),
            Vec::new(),
            float_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("flag".to_string()))),
                field: "to_string".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "len".to_string(),
            }),
            Vec::new(),
            count_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "byte_len".to_string(),
            }),
            Vec::new(),
            count_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "split".to_string(),
            }),
            vec![arg(expr(ExprKind::String(",".to_string())))],
            Type::Named("list".to_string(), vec![string_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "replace".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("a".to_string()))),
                arg(expr(ExprKind::String("b".to_string()))),
            ],
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "to_lower".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "to_upper".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "contains".to_string(),
            }),
            vec![arg(expr(ExprKind::String("ur".to_string())))],
            bool_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "starts_with".to_string(),
            }),
            vec![arg(expr(ExprKind::String("au".to_string())))],
            bool_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "ends_with".to_string(),
            }),
            vec![arg(expr(ExprKind::String("ra".to_string())))],
            bool_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "join".to_string(),
            }),
            vec![arg(expr(ExprKind::List(vec![
                expr(ExprKind::String("left".to_string())),
                expr(ExprKind::String("right".to_string())),
            ])))],
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "strip_prefix".to_string(),
            }),
            vec![arg(expr(ExprKind::String("au".to_string())))],
            Type::Named("Option".to_string(), vec![string_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "strip_suffix".to_string(),
            }),
            vec![arg(expr(ExprKind::String("x".to_string())))],
            Type::Named("Option".to_string(), vec![string_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "clone".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "trim".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "len".to_string(),
            }),
            Vec::new(),
            count_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "is_empty".to_string(),
            }),
            Vec::new(),
            bool_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "copy".to_string(),
            }),
            Vec::new(),
            vec_int.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "append".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "pop".to_string(),
            }),
            Vec::new(),
            int_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "get".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(0)))],
            Type::Named("Option".to_string(), vec![int_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "set".to_string(),
            }),
            vec![
                named_arg("index", expr(ExprKind::Int(0))),
                named_arg("value", expr(ExprKind::Int(9))),
            ],
            int_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "contains".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(9)))],
            bool_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "remove".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(0)))],
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "swap".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(0))), arg(expr(ExprKind::Int(1)))],
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "extend".to_string(),
            }),
            vec![arg(expr(ExprKind::List(vec![expr(ExprKind::Int(2))])))],
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "insert".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(0))), arg(expr(ExprKind::Int(9)))],
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "clear".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "reverse".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "len".to_string(),
            }),
            Vec::new(),
            count_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "is_empty".to_string(),
            }),
            Vec::new(),
            bool_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "copy".to_string(),
            }),
            Vec::new(),
            map_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "get".to_string(),
            }),
            vec![arg(expr(ExprKind::String("count".to_string())))],
            Type::Named("Option".to_string(), vec![int_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "remove".to_string(),
            }),
            vec![arg(expr(ExprKind::String("count".to_string())))],
            Type::Named("Option".to_string(), vec![int_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "keys".to_string(),
            }),
            Vec::new(),
            Type::Named("list".to_string(), vec![string_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "values".to_string(),
            }),
            Vec::new(),
            Type::Named("list".to_string(), vec![int_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "items".to_string(),
            }),
            Vec::new(),
            Type::Named(
                "list".to_string(),
                vec![Type::Tuple(vec![string_ty.clone(), int_ty.clone()])],
            ),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "clear".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "update".to_string(),
            }),
            vec![arg(expr(ExprKind::Map(vec![MapEntryExpr {
                key: expr(ExprKind::String("next".to_string())),
                value: expr(ExprKind::Int(2)),
            }])))],
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("items".to_string()))),
                field: "len".to_string(),
            }),
            Vec::new(),
            count_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("items".to_string()))),
                field: "is_empty".to_string(),
            }),
            Vec::new(),
            bool_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("items".to_string()))),
                field: "copy".to_string(),
            }),
            Vec::new(),
            set_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("items".to_string()))),
                field: "add".to_string(),
            }),
            vec![arg(expr(ExprKind::String("name".to_string())))],
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("items".to_string()))),
                field: "remove".to_string(),
            }),
            vec![arg(expr(ExprKind::String("name".to_string())))],
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "try_put".to_string(),
            }),
            vec![arg(expr(ExprKind::String("ok".to_string())))],
            Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named("SendError".to_string(), vec![string_ty.clone()]),
                ],
            ),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "get".to_string(),
            }),
            Vec::new(),
            Type::Named("QueueReceive".to_string(), vec![string_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "get_or_none".to_string(),
            }),
            Vec::new(),
            Type::Named("Option".to_string(), vec![string_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "get_or".to_string(),
            }),
            vec![arg(expr(ExprKind::String("fallback".to_string())))],
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "put".to_string(),
            }),
            vec![arg(expr(ExprKind::String("ok".to_string())))],
            Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named("SendError".to_string(), vec![string_ty.clone()]),
                ],
            ),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("task".to_string()))),
                field: "result".to_string(),
            }),
            Vec::new(),
            Type::Named("TaskResult".to_string(), vec![int_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("task".to_string()))),
                field: "result_or_none".to_string(),
            }),
            Vec::new(),
            Type::Named("Option".to_string(), vec![int_ty.clone()]),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("task".to_string()))),
                field: "result_or".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(0)))],
            int_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("group".to_string()))),
                field: "cancel".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("file".to_string()))),
                field: "read_all".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("file".to_string()))),
                field: "read_bytes".to_string(),
            }),
            Vec::new(),
            result_ty(bytes_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("file".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![arg(expr(ExprKind::String("ok".to_string())))],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("file".to_string()))),
                field: "write_bytes".to_string(),
            }),
            vec![arg(bytes_expr())],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("file".to_string()))),
                field: "flush".to_string(),
            }),
            Vec::new(),
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("file".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "stdin".to_string(),
            }),
            Vec::new(),
            option_ty(Type::named("process.Pipe")),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "stdout".to_string(),
            }),
            Vec::new(),
            option_ty(Type::named("process.Pipe")),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "stderr".to_string(),
            }),
            Vec::new(),
            option_ty(Type::named("process.Pipe")),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "wait".to_string(),
            }),
            vec![timeout_arg()],
            Type::named("process.Wait"),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "wait_or_none".to_string(),
            }),
            vec![timeout_arg()],
            process_result_ty(option_ty(Type::named("process.ExitStatus"))),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "wait_ok".to_string(),
            }),
            vec![timeout_arg()],
            process_result_ty(Type::named("process.ExitStatus")),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "kill".to_string(),
            }),
            Vec::new(),
            process_result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "terminate".to_string(),
            }),
            Vec::new(),
            process_result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "read_all".to_string(),
            }),
            Vec::new(),
            process_result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "read_line".to_string(),
            }),
            vec![timeout_arg()],
            process_result_ty(option_ty(string_ty.clone())),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "read_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), timeout_arg()],
            process_result_ty(option_ty(bytes_ty.clone())),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![arg(expr(ExprKind::String("ok".to_string()))), timeout_arg()],
            process_result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "write_bytes".to_string(),
            }),
            vec![arg(bytes_expr()), timeout_arg()],
            process_result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "flush".to_string(),
            }),
            Vec::new(),
            process_result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("completed".to_string()))),
                field: "status".to_string(),
            }),
            Vec::new(),
            Type::named("process.ExitStatus"),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("completed".to_string()))),
                field: "success".to_string(),
            }),
            Vec::new(),
            bool_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("completed".to_string()))),
                field: "stdout".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("completed".to_string()))),
                field: "stderr".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("completed".to_string()))),
                field: "stdout_bytes".to_string(),
            }),
            Vec::new(),
            bytes_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("completed".to_string()))),
                field: "stderr_bytes".to_string(),
            }),
            Vec::new(),
            bytes_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("completed".to_string()))),
                field: "check".to_string(),
            }),
            Vec::new(),
            process_result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("svc".to_string()))),
                arg(expr(ExprKind::List(vec![expr(ExprKind::String(
                    "/bin/echo".to_string(),
                ))]))),
            ],
            process_result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "wait".to_string(),
            }),
            vec![timeout_arg()],
            Type::named("process.SupervisorWait"),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "wait_or_none".to_string(),
            }),
            vec![timeout_arg()],
            process_result_ty(option_ty(Type::named("process.SupervisorEvent"))),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "stop".to_string(),
            }),
            Vec::new(),
            process_result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "is_empty".to_string(),
            }),
            Vec::new(),
            bool_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_listener".to_string()))),
                field: "accept".to_string(),
            }),
            vec![timeout_arg()],
            result_ty(Type::named("net.TcpStream")),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_listener".to_string()))),
                field: "local_addr".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_listener".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "read_all".to_string(),
            }),
            vec![timeout_arg()],
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "read_line".to_string(),
            }),
            vec![timeout_arg()],
            result_ty(option_ty(string_ty.clone())),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "read_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), timeout_arg()],
            result_ty(option_ty(bytes_ty.clone())),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "read_exact".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), timeout_arg()],
            result_ty(bytes_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![arg(expr(ExprKind::String("ok".to_string()))), timeout_arg()],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "write_bytes".to_string(),
            }),
            vec![arg(bytes_expr()), timeout_arg()],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "shutdown_read".to_string(),
            }),
            Vec::new(),
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "shutdown_write".to_string(),
            }),
            Vec::new(),
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "shutdown_both".to_string(),
            }),
            Vec::new(),
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "flush".to_string(),
            }),
            Vec::new(),
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "local_addr".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "peer_addr".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "send_text".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("127.0.0.1:9".to_string()))),
                arg(expr(ExprKind::String("ok".to_string()))),
                timeout_arg(),
            ],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "send_bytes".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("127.0.0.1:9".to_string()))),
                arg(bytes_expr()),
                timeout_arg(),
            ],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "recv".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), timeout_arg()],
            result_ty(option_ty(bytes_ty.clone())),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "recv_from".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), timeout_arg()],
            result_ty(option_ty(Type::named("net.UdpDatagram"))),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "local_addr".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "peer_addr".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_datagram".to_string()))),
                field: "address".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_datagram".to_string()))),
                field: "bytes".to_string(),
            }),
            Vec::new(),
            bytes_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_datagram".to_string()))),
                field: "text".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_listener".to_string()))),
                field: "accept".to_string(),
            }),
            vec![timeout_arg()],
            result_ty(Type::named("net.HttpExchange")),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_listener".to_string()))),
                field: "local_addr".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_listener".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "method".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "path".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "headers".to_string(),
            }),
            Vec::new(),
            headers_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "body_text".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "body_bytes".to_string(),
            }),
            Vec::new(),
            bytes_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "respond_text".to_string(),
            }),
            vec![
                arg(expr(ExprKind::Int(200))),
                arg(expr(ExprKind::String("ok".to_string()))),
                arg(headers_expr()),
            ],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "respond_bytes".to_string(),
            }),
            vec![
                arg(expr(ExprKind::Int(200))),
                arg(bytes_expr()),
                arg(headers_expr()),
            ],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_response".to_string()))),
                field: "status".to_string(),
            }),
            Vec::new(),
            int_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_response".to_string()))),
                field: "reason".to_string(),
            }),
            Vec::new(),
            string_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_response".to_string()))),
                field: "headers".to_string(),
            }),
            Vec::new(),
            headers_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_response".to_string()))),
                field: "text".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_response".to_string()))),
                field: "bytes".to_string(),
            }),
            Vec::new(),
            bytes_ty.clone(),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket_listener".to_string()))),
                field: "accept".to_string(),
            }),
            vec![timeout_arg()],
            result_ty(Type::named("net.WebSocket")),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket_listener".to_string()))),
                field: "local_addr".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket".to_string()))),
                field: "send_text".to_string(),
            }),
            vec![arg(expr(ExprKind::String("ok".to_string()))), timeout_arg()],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket".to_string()))),
                field: "send_bytes".to_string(),
            }),
            vec![arg(bytes_expr()), timeout_arg()],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket".to_string()))),
                field: "recv_text".to_string(),
            }),
            vec![timeout_arg()],
            result_ty(option_ty(string_ty.clone())),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket".to_string()))),
                field: "recv_bytes".to_string(),
            }),
            vec![timeout_arg()],
            result_ty(option_ty(bytes_ty.clone())),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_listener".to_string()))),
                field: "accept".to_string(),
            }),
            vec![timeout_arg()],
            result_ty(Type::named("net.UnixStream")),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_listener".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_stream".to_string()))),
                field: "read_line".to_string(),
            }),
            vec![timeout_arg()],
            result_ty(option_ty(string_ty.clone())),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_stream".to_string()))),
                field: "read_exact".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), timeout_arg()],
            result_ty(bytes_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_stream".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![arg(expr(ExprKind::String("ok".to_string()))), timeout_arg()],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_stream".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_listener".to_string()))),
                field: "accept".to_string(),
            }),
            vec![timeout_arg()],
            result_ty(Type::named("net.TlsStream")),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_listener".to_string()))),
                field: "local_addr".to_string(),
            }),
            Vec::new(),
            result_ty(string_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_listener".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_stream".to_string()))),
                field: "read_line".to_string(),
            }),
            vec![timeout_arg()],
            result_ty(option_ty(string_ty.clone())),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_stream".to_string()))),
                field: "read_exact".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), timeout_arg()],
            result_ty(bytes_ty.clone()),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_stream".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![arg(expr(ExprKind::String("ok".to_string()))), timeout_arg()],
            result_ty(Type::Unit),
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_stream".to_string()))),
                field: "close".to_string(),
            }),
            Vec::new(),
            Type::Unit,
        ),
    ] {
        assert_eq!(
            checker
                .type_of_call(&callee, &args, span, &mut locals, None)
                .expect("member call should type check"),
            expected,
            "{callee:?}"
        );
    }

    for (callee, args, expected) in [
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "put".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("ok".to_string()))),
                arg(expr(ExprKind::Int(1))),
            ],
            "`put(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "try_put".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`try_put` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "get".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`get(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "get_or_none".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`get(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "get_or".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("fallback".to_string()))),
                arg(expr(ExprKind::Int(1))),
            ],
            "`get_or(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("queue".to_string()))),
                field: "get_or".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`get_or` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("task".to_string()))),
                field: "result".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`result(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("task".to_string()))),
                field: "result_or_none".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`result(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("task".to_string()))),
                field: "result_or".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(0))), arg(expr(ExprKind::Int(1)))],
            "`result_or(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("task".to_string()))),
                field: "result_or".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`result_or` expects `int32`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("group".to_string()))),
                field: "start".to_string(),
            }),
            Vec::new(),
            "`start` expects a target function followed by its arguments",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("group".to_string()))),
                field: "start_soon".to_string(),
            }),
            vec![named_arg(
                "target",
                expr(ExprKind::Name("worker".to_string())),
            )],
            "`start_soon` does not take keyword arguments",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "wait".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`wait(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "wait_or_none".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`wait_or_none(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("child".to_string()))),
                field: "wait_ok".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`wait_ok(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "read_line".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`read_line(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "read_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::String("bad".to_string())))],
            "`read_bytes` expects `int32`, found `str`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "read_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), arg(expr(ExprKind::Int(1)))],
            "`read_bytes(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`write_all` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("ok".to_string()))),
                arg(expr(ExprKind::Int(1))),
            ],
            "`write_all(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "write_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::Name("bad_bytes".to_string())))],
            "`write_bytes` expects `list[uint8]`, found `list[bool]`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pipe".to_string()))),
                field: "write_bytes".to_string(),
            }),
            vec![arg(bytes_expr()), arg(expr(ExprKind::Int(1)))],
            "`write_bytes(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::Bool(true))),
                arg(expr(ExprKind::List(vec![expr(ExprKind::String(
                    "/bin/echo".to_string(),
                ))]))),
            ],
            "`start` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("svc".to_string()))),
                arg(expr(ExprKind::Bool(true))),
            ],
            "`start` expects `list[str]`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("svc".to_string()))),
                arg(expr(ExprKind::List(vec![expr(ExprKind::String(
                    "/bin/echo".to_string(),
                ))]))),
                named_arg("cwd", expr(ExprKind::Bool(true))),
            ],
            "`start` expects `Option[str]`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("svc".to_string()))),
                arg(expr(ExprKind::List(vec![expr(ExprKind::String(
                    "/bin/echo".to_string(),
                ))]))),
                named_arg("env", expr(ExprKind::Bool(true))),
            ],
            "`start` expects `dict[str, str]`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("svc".to_string()))),
                arg(expr(ExprKind::List(vec![expr(ExprKind::String(
                    "/bin/echo".to_string(),
                ))]))),
                named_arg("stdin", expr(ExprKind::Bool(true))),
            ],
            "`start` expects `process.Stdio`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("svc".to_string()))),
                arg(expr(ExprKind::List(vec![expr(ExprKind::String(
                    "/bin/echo".to_string(),
                ))]))),
                named_arg("stdout", expr(ExprKind::Bool(true))),
            ],
            "`start` expects `process.Stdio`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("svc".to_string()))),
                arg(expr(ExprKind::List(vec![expr(ExprKind::String(
                    "/bin/echo".to_string(),
                ))]))),
                named_arg("stderr", expr(ExprKind::Bool(true))),
            ],
            "`start` expects `process.Stdio`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("svc".to_string()))),
                arg(expr(ExprKind::List(vec![expr(ExprKind::String(
                    "/bin/echo".to_string(),
                ))]))),
                named_arg("backoff", expr(ExprKind::Bool(true))),
            ],
            "`start` expects `Duration`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("svc".to_string()))),
                arg(expr(ExprKind::List(vec![expr(ExprKind::String(
                    "/bin/echo".to_string(),
                ))]))),
                named_arg("max_restarts", expr(ExprKind::Bool(true))),
            ],
            "`start` expects `int32`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("svc".to_string()))),
                arg(expr(ExprKind::List(vec![expr(ExprKind::String(
                    "/bin/echo".to_string(),
                ))]))),
                named_arg("group", expr(ExprKind::Int(1))),
            ],
            "`start` expects `bool`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "wait".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`wait(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "wait_or_none".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`wait_or_none(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_listener".to_string()))),
                field: "accept".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`accept(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`write_all` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("file".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`write_all` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("file".to_string()))),
                field: "write_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::Name("bad_bytes".to_string())))],
            "`write_bytes` expects `list[uint8]`, found `list[bool]`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "split".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`split` expects `str`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("text".to_string()))),
                field: "strip_prefix".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`strip_prefix` expects `str`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "get".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`get` expects `str`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "remove".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`remove` expects `str`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                field: "update".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`update` expects `dict[str, int32]`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("items".to_string()))),
                field: "add".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`add` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("items".to_string()))),
                field: "remove".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`remove` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "read_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::String("bad".to_string())))],
            "`read_bytes` expects `int32`, found `str`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "read_all".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`read_all(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "read_line".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`read_line(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "read_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), arg(expr(ExprKind::Int(1)))],
            "`read_bytes(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "read_exact".to_string(),
            }),
            vec![arg(expr(ExprKind::String("bad".to_string())))],
            "`read_exact` expects `int32`, found `str`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "read_exact".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), arg(expr(ExprKind::Int(1)))],
            "`read_exact(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("ok".to_string()))),
                arg(expr(ExprKind::Int(1))),
            ],
            "`write_all(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "write_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::Name("bad_bytes".to_string())))],
            "`write_bytes` expects `list[uint8]`, found `list[bool]`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tcp_stream".to_string()))),
                field: "write_bytes".to_string(),
            }),
            vec![arg(bytes_expr()), arg(expr(ExprKind::Int(1)))],
            "`write_bytes(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "send_text".to_string(),
            }),
            vec![
                arg(expr(ExprKind::Int(1))),
                arg(expr(ExprKind::String("ok".to_string()))),
            ],
            "`send_text` expects `str`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "send_text".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("127.0.0.1:9".to_string()))),
                arg(expr(ExprKind::Bool(true))),
            ],
            "`send_text` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "send_text".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("127.0.0.1:9".to_string()))),
                arg(expr(ExprKind::String("ok".to_string()))),
                arg(expr(ExprKind::Int(1))),
            ],
            "`send_text(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "send_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1))), arg(bytes_expr())],
            "`send_bytes` expects `str`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "send_bytes".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("127.0.0.1:9".to_string()))),
                arg(expr(ExprKind::Name("bad_bytes".to_string()))),
            ],
            "`send_bytes` expects `list[uint8]`, found `list[bool]`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "send_bytes".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("127.0.0.1:9".to_string()))),
                arg(bytes_expr()),
                arg(expr(ExprKind::Int(1))),
            ],
            "`send_bytes(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "recv".to_string(),
            }),
            vec![arg(expr(ExprKind::String("bad".to_string())))],
            "`recv` expects `int32`, found `str`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "recv".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), arg(expr(ExprKind::Int(1)))],
            "`recv(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "recv_from".to_string(),
            }),
            vec![arg(expr(ExprKind::String("bad".to_string())))],
            "`recv_from` expects `int32`, found `str`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("udp_socket".to_string()))),
                field: "recv_from".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), arg(expr(ExprKind::Int(1)))],
            "`recv_from(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_listener".to_string()))),
                field: "accept".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`accept(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "respond_text".to_string(),
            }),
            vec![
                arg(expr(ExprKind::Bool(true))),
                arg(expr(ExprKind::String("ok".to_string()))),
                arg(headers_expr()),
            ],
            "`respond_text` expects `int32`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "respond_text".to_string(),
            }),
            vec![
                arg(expr(ExprKind::Int(200))),
                arg(expr(ExprKind::Bool(true))),
                arg(headers_expr()),
            ],
            "`respond_text` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "respond_text".to_string(),
            }),
            vec![
                arg(expr(ExprKind::Int(200))),
                arg(expr(ExprKind::String("ok".to_string()))),
                arg(expr(ExprKind::Bool(true))),
            ],
            "`respond_text` expects `dict[str, str]`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "respond_bytes".to_string(),
            }),
            vec![
                arg(expr(ExprKind::Bool(true))),
                arg(bytes_expr()),
                arg(headers_expr()),
            ],
            "`respond_bytes` expects `int32`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "respond_bytes".to_string(),
            }),
            vec![
                arg(expr(ExprKind::Int(200))),
                arg(expr(ExprKind::Name("bad_bytes".to_string()))),
                arg(headers_expr()),
            ],
            "`respond_bytes` expects `list[uint8]`, found `list[bool]`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("http_exchange".to_string()))),
                field: "respond_bytes".to_string(),
            }),
            vec![
                arg(expr(ExprKind::Int(200))),
                arg(bytes_expr()),
                arg(expr(ExprKind::Bool(true))),
            ],
            "`respond_bytes` expects `dict[str, str]`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket_listener".to_string()))),
                field: "accept".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`accept(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket".to_string()))),
                field: "send_text".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`send_text` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket".to_string()))),
                field: "send_text".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("ok".to_string()))),
                arg(expr(ExprKind::Int(1))),
            ],
            "`send_text(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket".to_string()))),
                field: "send_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::Name("bad_bytes".to_string())))],
            "`send_bytes` expects `list[uint8]`, found `list[bool]`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket".to_string()))),
                field: "send_bytes".to_string(),
            }),
            vec![arg(bytes_expr()), arg(expr(ExprKind::Int(1)))],
            "`send_bytes(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket".to_string()))),
                field: "recv_text".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`recv_text(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("websocket".to_string()))),
                field: "recv_bytes".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`recv_bytes(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_listener".to_string()))),
                field: "accept".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`accept(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_stream".to_string()))),
                field: "read_line".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`read_line(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_stream".to_string()))),
                field: "read_exact".to_string(),
            }),
            vec![arg(expr(ExprKind::String("bad".to_string())))],
            "`read_exact` expects `int32`, found `str`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_stream".to_string()))),
                field: "read_exact".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), arg(expr(ExprKind::Int(1)))],
            "`read_exact(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_stream".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`write_all` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("unix_stream".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("ok".to_string()))),
                arg(expr(ExprKind::Int(1))),
            ],
            "`write_all(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_listener".to_string()))),
                field: "accept".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`accept(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_stream".to_string()))),
                field: "read_line".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(1)))],
            "`read_line(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_stream".to_string()))),
                field: "read_exact".to_string(),
            }),
            vec![arg(expr(ExprKind::String("bad".to_string())))],
            "`read_exact` expects `int32`, found `str`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_stream".to_string()))),
                field: "read_exact".to_string(),
            }),
            vec![arg(expr(ExprKind::Int(8))), arg(expr(ExprKind::Int(1)))],
            "`read_exact(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_stream".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![arg(expr(ExprKind::Bool(true)))],
            "`write_all` expects `str`, found `bool`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("tls_stream".to_string()))),
                field: "write_all".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("ok".to_string()))),
                arg(expr(ExprKind::Int(1))),
            ],
            "`write_all(timeout=...)` expects `Duration`, found `int64`",
        ),
        (
            expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("supervisor".to_string()))),
                field: "start".to_string(),
            }),
            vec![
                arg(expr(ExprKind::String("svc".to_string()))),
                arg(expr(ExprKind::List(vec![expr(ExprKind::String(
                    "/bin/echo".to_string(),
                ))]))),
                named_arg("restart", expr(ExprKind::Bool(true))),
            ],
            "`start` expects `process.RestartPolicy`, found `bool`",
        ),
    ] {
        let error = match checker.type_of_call(&callee, &args, span, &mut locals, None) {
            Ok(actual) => {
                panic!("member call should report `{expected}`, but type checked as `{actual}`")
            }
            Err(error) => error,
        };
        assert!(
            error.message.contains(expected),
            "expected diagnostic containing `{expected}`, got `{}`",
            error.message
        );
    }

    for field in [
        "append", "pop", "set", "remove", "swap", "extend", "insert", "clear", "reverse",
    ] {
        let args = match field {
            "append" => vec![arg(expr(ExprKind::Int(1)))],
            "pop" | "clear" | "reverse" => Vec::new(),
            "set" => vec![
                named_arg("index", expr(ExprKind::Int(0))),
                named_arg("value", expr(ExprKind::Int(1))),
            ],
            "remove" => vec![arg(expr(ExprKind::Int(0)))],
            "swap" => vec![arg(expr(ExprKind::Int(0))), arg(expr(ExprKind::Int(1)))],
            "extend" => vec![arg(expr(ExprKind::Name("values".to_string())))],
            "insert" => vec![arg(expr(ExprKind::Int(0))), arg(expr(ExprKind::Int(1)))],
            _ => unreachable!(),
        };
        assert!(checker
            .type_of_call(
                &expr(ExprKind::Member {
                    object: Box::new(expr(ExprKind::Name("immutable_values".to_string()))),
                    field: field.to_string(),
                }),
                &args,
                span,
                &mut locals,
                None,
            )
            .expect_err("mutable vector methods should reject immutable receivers")
            .message
            .contains("requires a mutable receiver"));
    }

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "get".to_string(),
            }),
            &[arg(expr(ExprKind::Bool(true)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("list.get() should enforce integer indices")
        .message
        .contains("list indices must have type `int64` or a losslessly narrower integer type"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "set".to_string(),
            }),
            &[
                named_arg("index", expr(ExprKind::Bool(true))),
                named_arg("value", expr(ExprKind::Int(1))),
            ],
            span,
            &mut locals,
            None,
        )
        .expect_err("list.set() should enforce integer indices")
        .message
        .contains("list indices must have type `int64` or a losslessly narrower integer type"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "set".to_string(),
            }),
            &[
                named_arg("index", expr(ExprKind::Int(0))),
                named_arg("value", expr(ExprKind::Bool(true))),
            ],
            span,
            &mut locals,
            None,
        )
        .expect_err("list.set() should enforce element types")
        .message
        .contains("`set` expects `int32`"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "remove".to_string(),
            }),
            &[arg(expr(ExprKind::Bool(true)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("list.remove() should enforce element types")
        .message
        .contains("`remove` expects `int32`"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "swap".to_string(),
            }),
            &[arg(expr(ExprKind::Bool(true))), arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("list.swap() should enforce the first integer index")
        .message
        .contains("list indices must have type `int64` or a losslessly narrower integer type"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "swap".to_string(),
            }),
            &[arg(expr(ExprKind::Int(0))), arg(expr(ExprKind::Bool(true)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("list.swap() should enforce integer indices")
        .message
        .contains("list indices must have type `int64` or a losslessly narrower integer type"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "contains".to_string(),
            }),
            &[arg(expr(ExprKind::Bool(true)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("list.contains() should enforce element types")
        .message
        .contains("`contains` expects `int32`"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "extend".to_string(),
            }),
            &[arg(expr(ExprKind::Bool(true)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("list.extend() should enforce list types")
        .message
        .contains("`extend` expects `list[int32]`"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "insert".to_string(),
            }),
            &[arg(expr(ExprKind::Bool(true))), arg(expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("list.insert() should enforce integer indices")
        .message
        .contains("list indices must have type `int64` or a losslessly narrower integer type"));

    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("values".to_string()))),
                field: "insert".to_string(),
            }),
            &[arg(expr(ExprKind::Int(0))), arg(expr(ExprKind::Bool(true)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("list.insert() should enforce element types")
        .message
        .contains("`insert` expects `int32`"));
}

#[test]
fn copy_and_type_classifier_helpers_cover_builtin_and_user_types() {
    let classes = BTreeMap::from([
        (
            "Pair".to_string(),
            class_info(
                "Pair",
                true,
                vec![
                    ("left", Type::named("int32"), false),
                    ("right", Type::named("bool"), false),
                ],
            ),
        ),
        (
            "Owned".to_string(),
            class_info("Owned", false, vec![("name", Type::named("str"), false)]),
        ),
    ]);
    let enums = BTreeMap::from([
        (
            "MaybeInt".to_string(),
            enum_info("MaybeInt", Some(Type::named("int32"))),
        ),
        (
            "MaybeName".to_string(),
            enum_info("MaybeName", Some(Type::named("str"))),
        ),
    ]);

    assert!(is_builtin_copy_named_type("int32", &[]));
    assert!(!is_builtin_copy_named_type("str", &[]));
    assert!(type_is_copy_in_context(
        &Type::named("Pair"),
        &classes,
        &enums
    ));
    assert!(!type_is_copy_in_context(
        &Type::named("Owned"),
        &classes,
        &enums
    ));
    assert!(type_is_copy_in_context(
        &Type::Named("Option".to_string(), vec![Type::named("int32")]),
        &classes,
        &enums,
    ));
    assert!(!type_is_copy_in_context(
        &Type::Named(
            "Result".to_string(),
            vec![Type::named("int32"), Type::named("str")]
        ),
        &classes,
        &enums,
    ));
    assert!(type_is_copy_in_context(
        &Type::named("MaybeInt"),
        &classes,
        &enums
    ));
    assert!(!type_is_copy_in_context(
        &Type::named("MaybeName"),
        &classes,
        &enums
    ));

    assert!(is_builtin_type("list"));
    assert!(is_integer_type(&Type::named("int64")));
    assert!(is_float_type(&Type::named("float64")));
    assert!(is_string_type(&Type::named("str")));
    assert!(is_numeric_type(&Type::named("float32")));
    assert!(Type::Unit.is_copy());
    assert!(!Type::Module("pkg.tools".to_string()).is_copy());
    assert!(!Type::TypeParam("T".to_string()).is_copy());
    assert!(Type::named("bool").is_copy());
    assert_eq!(Type::Unit.to_string(), "None");
    assert_eq!(
        Type::Module("pkg.tools".to_string()).to_string(),
        "module pkg.tools"
    );
    assert_eq!(
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        )
        .to_string(),
        "dict[str, int32]"
    );
}

#[test]
fn sema_helper_edges_cover_copy_defaults_literal_patterns_and_module_members() {
    let info = class_info(
        "Mixed",
        false,
        vec![
            (
                "module_field",
                Type::Module("pkg.helpers".to_string()),
                false,
            ),
            ("unit_field", Type::Unit, false),
            ("type_param_field", Type::TypeParam("T".to_string()), false),
        ],
    );
    assert!(matches!(
        info.decl.fields[0].ty.named_parts(),
        Some(("pkg.helpers", _))
    ));
    assert!(matches!(
        info.decl.fields[1].ty.named_parts(),
        Some(("None", _))
    ));
    assert!(matches!(
        info.decl.fields[2].ty.named_parts(),
        Some(("T", _))
    ));

    let classes = BTreeMap::from([(
        "Boxed".to_string(),
        class_info("Boxed", true, vec![("value", Type::named("int32"), false)]),
    )]);
    let enums = BTreeMap::from([
        ("Flag".to_string(), enum_info("Flag", None)),
        (
            "Payload".to_string(),
            enum_info("Payload", Some(Type::named("str"))),
        ),
    ]);
    assert!(!type_is_copy_in_context(
        &Type::Module("pkg".to_string()),
        &classes,
        &enums
    ));
    assert!(!type_is_copy_in_context(
        &Type::TypeParam("T".to_string()),
        &classes,
        &enums,
    ));
    assert!(type_is_copy_in_context(
        &Type::named("Flag"),
        &classes,
        &enums
    ));
    assert!(!type_is_copy_in_context(
        &Type::named("Payload"),
        &classes,
        &enums,
    ));
    assert_eq!(
        Type::Named(
            "Pair".to_string(),
            vec![Type::named("int32"), Type::named("bool")]
        )
        .to_string(),
        "Pair[int32, bool]"
    );

    let params = vec!["source".to_string(), "fallback".to_string()];
    let default_expr = expr(ExprKind::Call {
        callee: Box::new(expr(ExprKind::Name("build".to_string()))),
        args: vec![
            Argument {
                name: Some("value".to_string()),
                span: Span::new(1, 1),
                value: expr(ExprKind::Index {
                    object: Box::new(expr(ExprKind::Name("source".to_string()))),
                    index: Box::new(expr(ExprKind::Int(0))),
                }),
            },
            Argument {
                name: Some("fallback".to_string()),
                span: Span::new(1, 1),
                value: expr(ExprKind::Map(vec![MapEntryExpr {
                    key: expr(ExprKind::String("name".to_string())),
                    value: expr(ExprKind::FString(vec![
                        FormatPart::Literal("prefix".to_string()),
                        FormatPart::Expr(expr(ExprKind::Name("fallback".to_string()))),
                    ])),
                }])),
            },
        ],
    });
    assert_eq!(
        default_argument_references_param(&default_expr, &params),
        Some("source".to_string())
    );
    let wait_default = expr(ExprKind::Call {
        callee: Box::new(expr(ExprKind::Name("wait_all".to_string()))),
        args: vec![Argument {
            name: None,
            span: Span::new(1, 1),
            value: expr(ExprKind::Name("fallback".to_string())),
        }],
    });
    assert_eq!(
        default_argument_references_param(&wait_default, &params),
        Some("fallback".to_string())
    );

    let mut imported_modules = BTreeMap::new();
    let mut registry = BTreeMap::new();
    let mut root = namespace("pkg");
    root.modules
        .insert("helpers".to_string(), namespace("pkg.helpers"));
    root.functions.insert(
        "make".to_string(),
        FunctionInfo {
            module_name: "pkg".to_string(),
            decl: function_decl("make"),
            signature: function_signature(Vec::new(), Type::named("None")),
            type_param_bounds: BTreeMap::new(),
        },
    );
    root.classes.insert(
        "Widget".to_string(),
        class_info(
            "Widget",
            false,
            vec![("value", Type::named("int32"), false)],
        ),
    );
    root.enums
        .insert("Status".to_string(), enum_info("Status", None));
    imported_modules.insert("pkg".to_string(), root.clone());
    registry.insert("pkg".to_string(), root);

    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let checker = checker(
        "main",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &registry,
    );
    assert_eq!(
        checker.render_literal_pattern(&LiteralPattern {
            kind: LiteralPatternKind::String("aura".to_string()),
            span: Span::new(1, 1),
        }),
        "\"aura\""
    );
    assert_eq!(
        checker
            .resolve_member_type(&Type::Module("pkg".to_string()), "helpers", Span::new(1, 1))
            .expect("child module should resolve"),
        Type::Module("pkg.helpers".to_string())
    );
    assert!(checker
        .resolve_member_type(&Type::Module("pkg".to_string()), "make", Span::new(1, 1))
        .expect_err("functions require call syntax")
        .message
        .contains("must be called"));
    assert!(checker
        .resolve_member_type(&Type::Module("pkg".to_string()), "Widget", Span::new(1, 1))
        .expect_err("classes require construction")
        .message
        .contains("must be constructed"));
    assert_eq!(
        checker
            .resolve_member_type(&Type::Module("pkg".to_string()), "Status", Span::new(1, 1))
            .expect("module enums should resolve to enum types for qualified variant access"),
        Type::Named("pkg.Status".to_string(), Vec::new())
    );
    assert!(checker
        .resolve_member_type(&Type::Module("pkg".to_string()), "missing", Span::new(1, 1))
        .expect_err("missing member should fail")
        .message
        .contains("has no member"));
    assert!(checker
        .resolve_member_type(&Type::TypeParam("T".to_string()), "value", Span::new(1, 1))
        .expect_err("type params without traits cannot expose members")
        .message
        .contains("cannot access field"));
    assert!(checker
        .resolve_member_type(&Type::Unit, "value", Span::new(1, 1))
        .expect_err("unit has no members")
        .message
        .contains("cannot access field"));

    let pkg_widget = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Name("pkg".to_string()))),
        field: "Widget".to_string(),
    });
    let mut call_locals = HashMap::new();
    checker.seed_imported_modules(&mut call_locals);
    assert_eq!(
        checker
            .type_of_call(
                &pkg_widget,
                &[named_arg("value", expr(ExprKind::Int(1)))],
                Span::new(1, 1),
                &mut call_locals,
                None,
            )
            .expect("module class constructors should type check"),
        Type::named("pkg.Widget")
    );
    for (args, expected) in [
        (
            vec![
                named_arg("value", expr(ExprKind::Int(1))),
                arg(expr(ExprKind::Int(2))),
            ],
            "positional class constructor arguments must come before named arguments",
        ),
        (
            vec![arg(expr(ExprKind::Int(1))), arg(expr(ExprKind::Int(2)))],
            "class constructor `Widget` received too many positional arguments",
        ),
        (
            vec![named_arg("missing", expr(ExprKind::Int(1)))],
            "class `Widget` has no field named `missing`",
        ),
        (
            vec![
                named_arg("value", expr(ExprKind::Int(1))),
                named_arg("value", expr(ExprKind::Int(2))),
            ],
            "field `value` was provided more than once",
        ),
    ] {
        assert!(
            checker
                .type_of_call(&pkg_widget, &args, Span::new(1, 1), &mut call_locals, None,)
                .expect_err("module class constructor diagnostics should be reported")
                .message
                .contains(expected),
            "expected module constructor diagnostic containing `{expected}`"
        );
    }
}

#[test]
fn sema_render_and_builtin_enum_hint_helpers_cover_remaining_paths() {
    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();
    let classes = BTreeMap::new();
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<test>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );

    let place = expr(ExprKind::Name("item".to_string()));
    let member = expr(ExprKind::Member {
        object: Box::new(place.clone()),
        field: "value".to_string(),
    });
    let index = expr(ExprKind::Index {
        object: Box::new(member.clone()),
        index: Box::new(expr(ExprKind::Int(0))),
    });
    let grouped = expr(ExprKind::Group(Box::new(index.clone())));

    assert_eq!(checker.render_place_expr(&place), "item");
    assert_eq!(checker.render_place_expr(&member), "item.value");
    assert_eq!(checker.render_place_expr(&grouped), "item.value[..]");
    assert_eq!(checker.render_member_target(&place, "value"), "item.value");
    assert_eq!(checker.render_index_target(&member), "item.value[..]");
    assert_eq!(
        render_literal_pattern_key(&LiteralPatternKey::Int(IntegerValue::from_signed(-3))),
        "-3"
    );
    assert_eq!(
        render_literal_pattern_key(&LiteralPatternKey::Bool(true)),
        "true"
    );
    assert_eq!(
        render_literal_pattern_key(&LiteralPatternKey::String("aura".to_string())),
        "\"aura\""
    );

    assert_eq!(
        set_element_type(&Type::Named("set".to_string(), vec![Type::named("str")])),
        Some(&Type::named("str"))
    );
    assert_eq!(set_element_type(&Type::named("str")), None);

    let option_name = expr(ExprKind::Name("Option".to_string()));
    let specialized_option = expr(ExprKind::Specialize {
        expr: Box::new(option_name.clone()),
        type_args: vec![type_ref("int32")],
    });
    let constructor_member = expr(ExprKind::Member {
        object: Box::new(specialized_option.clone()),
        field: "Some".to_string(),
    });
    let constructor_call = expr(ExprKind::Call {
        callee: Box::new(constructor_member.clone()),
        args: vec![arg(expr(ExprKind::Int(1)))],
    });

    assert!(checker.is_builtin_enum_constructor_expr(&option_name));
    assert!(checker.is_builtin_enum_constructor_expr(&specialized_option));
    assert!(!checker.is_builtin_enum_constructor_expr(&place));
    assert!(checker.expr_can_use_partial_expected_hint(&constructor_member));
    assert!(checker.expr_can_use_partial_expected_hint(&constructor_call));
    assert!(!checker.expr_can_use_partial_expected_hint(&place));
}

#[test]
fn checker_helper_paths_cover_imported_modules_type_args_and_binding_consumption() {
    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();
    let classes = BTreeMap::new();
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let trait_impls = Vec::new();
    let imported_modules = BTreeMap::from([("helpers".to_string(), namespace("pkg.helpers"))]);
    let mut current_namespace = namespace("pkg.current");
    current_namespace
        .imported_modules
        .insert("math".to_string(), namespace("pkg.current.math"));
    let module_registry = BTreeMap::from([("pkg.current".to_string(), current_namespace.clone())]);
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &trait_impls,
        &imported_modules,
        &module_registry,
    );
    let span = Span::new(3, 4);

    let mut top_level_locals = HashMap::new();
    checker.seed_imported_modules(&mut top_level_locals);
    assert_eq!(
        top_level_locals
            .get("helpers")
            .map(|binding| binding.ty.clone()),
        Some(Type::Module("pkg.helpers".to_string()))
    );

    let mut scoped_locals = HashMap::new();
    checker
        .with_module_name("pkg.current")
        .seed_imported_modules(&mut scoped_locals);
    assert_eq!(
        scoped_locals.get("math").map(|binding| binding.ty.clone()),
        Some(Type::Module("pkg.current.math".to_string()))
    );

    let plain_expr = expr(ExprKind::Name("Box".to_string()));
    let specialized_expr = expr(ExprKind::Specialize {
        expr: Box::new(plain_expr.clone()),
        type_args: vec![type_ref("int32")],
    });
    let (peeled, explicit_args) = checker.peel_specialization(&specialized_expr);
    assert!(matches!(peeled.kind, ExprKind::Name(ref name) if name == "Box"));
    assert_eq!(explicit_args.map(|args| args.len()), Some(1));
    let (plain_peeled, plain_args) = checker.peel_specialization(&plain_expr);
    assert!(matches!(plain_peeled.kind, ExprKind::Name(ref name) if name == "Box"));
    assert!(plain_args.is_none());

    let substitutions = checker
        .explicit_type_substitutions(
            &["T".to_string(), "U".to_string()],
            &[type_ref("int32"), type_ref("str")],
            span,
            "Pair",
        )
        .expect("matching explicit type arguments should lower");
    assert_eq!(substitutions.get("T"), Some(&Type::named("int32")));
    assert_eq!(substitutions.get("U"), Some(&Type::named("str")));
    let mismatch = checker
        .explicit_type_substitutions(
            &["T".to_string(), "U".to_string()],
            &[type_ref("int32")],
            span,
            "Pair",
        )
        .expect_err("arity mismatch should fail");
    assert!(mismatch
        .message
        .contains("Pair expects 2 type arguments, found 1"));

    checker
        .validate_negative_integer_literal(7, &Type::named("str"), span)
        .expect("non-integer targets should be ignored");
    let neg_overflow = checker
        .validate_negative_integer_literal(u128::MAX, &Type::named("int128"), span)
        .expect_err("unrepresentable negative literals should fail");
    assert!(neg_overflow.message.contains("does not fit in `int128`"));
    let int8_overflow = checker
        .validate_negative_integer_literal(129, &Type::named("int8"), span)
        .expect_err("out-of-bounds negative literals should fail");
    assert!(int8_overflow.message.contains("does not fit in `int8`"));

    let mut locals = HashMap::from([(
        "count".to_string(),
        LocalBinding {
            ty: Type::named("int32"),
            assignable: true,
            mutable_place: true,
            managed_resource: false,
            passing: ReceiverKind::Value,
            borrow_origin: None,
            borrowed_at: None,
            match_borrow_place: None,
            stale_match_borrow_place: None,
            shared_match_scrutinee: None,
            moved: false,
            moved_at: None,
            moved_fields: BTreeMap::new(),
            frozen_places: BTreeMap::new(),
            shared_match_places: BTreeMap::new(),
            captured: false,
            view: None,
            closure_loans: Vec::new(),
        },
    )]);
    checker
        .consume_binding("count", span, &mut locals)
        .expect("copy types should not be consumed");
    assert!(!locals["count"].moved);

    let unknown = checker
        .consume_binding("missing", span, &mut HashMap::new())
        .expect_err("unknown names should fail");
    assert!(unknown.message.contains("unknown name `missing`"));

    let mut borrowed_locals = HashMap::from([(
        "borrowed".to_string(),
        LocalBinding {
            ty: Type::named("str"),
            assignable: true,
            mutable_place: false,
            managed_resource: false,
            passing: ReceiverKind::Borrow,
            borrow_origin: Some("borrowed".to_string()),
            borrowed_at: None,
            match_borrow_place: None,
            stale_match_borrow_place: None,
            shared_match_scrutinee: None,
            moved: false,
            moved_at: None,
            moved_fields: BTreeMap::new(),
            frozen_places: BTreeMap::new(),
            shared_match_places: BTreeMap::new(),
            captured: false,
            view: None,
            closure_loans: Vec::new(),
        },
    )]);
    let borrowed_error = checker
        .consume_binding("borrowed", span, &mut borrowed_locals)
        .expect_err("borrowed values cannot be moved");
    assert!(borrowed_error
        .message
        .contains("cannot move borrowed value `borrowed`"));

    let mut moved_locals = HashMap::from([(
        "text".to_string(),
        LocalBinding {
            ty: Type::named("str"),
            assignable: true,
            mutable_place: true,
            managed_resource: false,
            passing: ReceiverKind::Value,
            borrow_origin: None,
            borrowed_at: None,
            match_borrow_place: None,
            stale_match_borrow_place: None,
            shared_match_scrutinee: None,
            moved: true,
            moved_at: Some(span),
            moved_fields: BTreeMap::new(),
            frozen_places: BTreeMap::new(),
            shared_match_places: BTreeMap::new(),
            captured: false,
            view: None,
            closure_loans: Vec::new(),
        },
    )]);
    let moved_error = checker
        .consume_binding("text", span, &mut moved_locals)
        .expect_err("moved values should be rejected");
    assert!(moved_error.message.contains("use of moved value `text`"));
}

#[test]
fn checker_move_consumption_helpers_cover_managed_specialized_member_and_match_paths() {
    let span = Span::new(1, 1);
    let classes = BTreeMap::from([(
        "Holder".to_string(),
        class_info("Holder", false, vec![("text", Type::named("str"), false)]),
    )]);
    let type_names = BTreeMap::from([("Holder".to_string(), span)]);
    let type_arities = BTreeMap::from([("Holder".to_string(), 0usize)]);
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );

    let mut managed_locals = HashMap::from([(
        "resource".to_string(),
        LocalBinding {
            managed_resource: true,
            ..local_binding(
                Type::named("str"),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            )
        },
    )]);
    let managed_error = checker
        .consume_binding("resource", span, &mut managed_locals)
        .expect_err("managed resources should not move out by value");
    assert!(managed_error
        .message
        .contains("cannot move managed `with` resource `resource`"));

    let mut specialized_locals = HashMap::from([(
        "owned".to_string(),
        local_binding(
            Type::named("str"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    checker
        .consume_value_expr(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("owned".to_string()))),
                type_args: vec![type_ref("str")],
            }),
            &mut specialized_locals,
        )
        .expect("specialized value expressions should consume their base value");
    assert!(specialized_locals["owned"].moved);

    let mut member_locals = HashMap::from([(
        "holder".to_string(),
        local_binding(
            Type::named("Holder"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    checker
        .consume_value_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("holder".to_string()))),
                field: "text".to_string(),
            }),
            &mut member_locals,
        )
        .expect("moving a non-copy field from an owned binding should be tracked");
    assert!(member_locals["holder"]
        .moved_fields
        .contains_key(&projection_path("text")));

    let mut match_locals = HashMap::from([
        (
            "flag".to_string(),
            local_binding(
                Type::named("bool"),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "owned".to_string(),
            local_binding(
                Type::named("str"),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
    ]);
    checker
        .consume_value_expr(
            &expr(ExprKind::Match {
                scrutinee: Box::new(expr(ExprKind::Name("flag".to_string()))),
                capability: ReceiverKind::Borrow,
                arms: vec![MatchExprArm {
                    guard: None,
                    pattern: Pattern::Wildcard(span),
                    value: expr(ExprKind::Name("owned".to_string())),
                    span,
                }],
            }),
            &mut match_locals,
        )
        .expect("match expression arms should merge consumed value state");
    assert!(match_locals["owned"].moved);

    let mut group_locals = HashMap::from([(
        "flag".to_string(),
        local_binding(
            Type::named("bool"),
            false,
            false,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    checker
        .consume_match_scrutinee_expr(
            &expr(ExprKind::Group(Box::new(expr(ExprKind::Name(
                "flag".to_string(),
            ))))),
            &mut group_locals,
        )
        .expect("grouped match scrutinees should be consumed through their inner expression");

    let mut borrowed_holder_locals = HashMap::from([(
        "holder".to_string(),
        LocalBinding {
            passing: ReceiverKind::Borrow,
            borrow_origin: Some("holder".to_string()),
            ..local_binding(
                Type::named("Holder"),
                false,
                false,
                ReceiverKind::Borrow,
                false,
                &[],
            )
        },
    )]);
    let borrowed_field_error = checker
        .consume_match_scrutinee_expr(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("holder".to_string()))),
                field: "text".to_string(),
            }),
            &mut borrowed_holder_locals,
        )
        .expect_err("match scrutinees should reject moving non-copy fields out of borrows");
    assert!(borrowed_field_error
        .message
        .contains("cannot move non-copy field `text` out of borrowed value `holder`"));

    let grouped_borrowed_field_error = checker
        .consume_match_scrutinee_expr(
            &expr(ExprKind::Group(Box::new(expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("holder".to_string()))),
                field: "text".to_string(),
            })))),
            &mut borrowed_holder_locals,
        )
        .expect_err("grouped match scrutinees should still reject borrowed field moves");
    assert!(grouped_borrowed_field_error
        .message
        .contains("cannot move non-copy field `text` out of borrowed value `holder`"));

    let mut merged_locals = HashMap::from([(
        "holder".to_string(),
        local_binding(
            Type::named("Holder"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    let mut branch_with_stale_match_borrow = merged_locals.clone();
    let branch_binding = branch_with_stale_match_borrow
        .get_mut("holder")
        .expect("branch binding should exist");
    branch_binding.moved = true;
    branch_binding
        .moved_fields
        .insert(projection_path("text"), Span::new(1, 1));
    branch_binding.stale_match_borrow_place = Some(place_path("holder.text"));
    let branch_without_binding = HashMap::new();
    checker.merge_control_flow_moves(
        &mut merged_locals,
        &[&branch_with_stale_match_borrow, &branch_without_binding],
    );
    assert!(merged_locals["holder"].moved);
    assert!(merged_locals["holder"]
        .moved_fields
        .contains_key(&projection_path("text")));
    assert_eq!(
        merged_locals["holder"].stale_match_borrow_place,
        Some(place_path("holder.text"))
    );

    assert_eq!(
        checker.const_bool_value(&expr(ExprKind::Unary {
            op: UnaryOp::Not,
            expr: Box::new(expr(ExprKind::Group(Box::new(expr(ExprKind::Bool(false)))))),
        })),
        Some(true)
    );

    checker
        .reject_loop_carried_moves(
            &HashMap::from([(
                "outer".to_string(),
                local_binding(
                    Type::named("str"),
                    true,
                    true,
                    ReceiverKind::Value,
                    false,
                    &[],
                ),
            )]),
            &HashMap::new(),
            "while",
            span,
        )
        .expect("bindings absent from the loop body state should be ignored");
}

#[test]
fn vec_literal_consumes_non_copy_elements_only_once() {
    crate::check_source(
        "class Box:\n    value: int32\n\ndef main() -> int32:\n    b = Box(value=1)\n    values: list[Box] = [b]\n    return 0\n",
    )
    .expect("Vec literals should accept the first move of a non-copy element");
}

#[test]
fn namespace_and_type_parameter_helpers_cover_registration_lookup_and_collection() {
    let mut child = namespace("pkg.inner");
    child.classes.insert(
        "Thing".to_string(),
        class_info("Thing", false, vec![("value", Type::named("int32"), false)]),
    );
    let mut imported = namespace("pkg.external");
    imported
        .traits
        .insert("Named".to_string(), trait_info("Named", vec!["T"]));
    let mut root = namespace("pkg");
    root.enums.insert(
        "State".to_string(),
        enum_info("State", Some(Type::named("bool"))),
    );
    root.modules.insert("inner".to_string(), child.clone());
    root.imported_modules
        .insert("external".to_string(), imported.clone());

    let mut type_names = BTreeMap::new();
    let mut type_arities = BTreeMap::new();
    register_module_namespace_types(&root, &mut type_names, &mut type_arities);

    assert!(type_names.contains_key("pkg.State"));
    assert!(type_names.contains_key("pkg.inner.Thing"));
    assert!(type_names.contains_key("pkg.external.Named"));
    assert_eq!(type_arities.get("pkg.external.Named"), Some(&1));
    assert_eq!(
        find_namespace_in_modules(&BTreeMap::from([("pkg".to_string(), root)]), "pkg.inner")
            .map(|found| found.path.clone()),
        Some("pkg.inner".to_string())
    );
    let mut root = namespace("pkg");
    root.imported_modules
        .insert("external".to_string(), imported.clone());
    assert_eq!(
        find_namespace_in_modules(&BTreeMap::from([("pkg".to_string(), root)]), "pkg.external")
            .map(|found| found.path.clone()),
        Some("pkg.external".to_string())
    );

    validate_type_params(
        &["T".to_string(), "U".to_string()],
        Span::new(1, 1),
        "class Box",
    )
    .expect("unique type params should validate");
    let duplicate = validate_type_params(
        &["T".to_string(), "T".to_string()],
        Span::new(1, 1),
        "class Box",
    )
    .expect_err("duplicate type params should fail");
    assert!(duplicate.message.contains("duplicate type parameter `T`"));
    let reserved_self = validate_type_params(&["Self".to_string()], Span::new(1, 1), "class Box")
        .expect_err("Self cannot be used as a type parameter");
    assert!(reserved_self.message.contains("`Self` is reserved"));

    let parent = type_param_scope(&["T".to_string()]);
    let merged = merged_type_param_scope(&parent, &["U".to_string()]);
    assert!(merged.contains_key("T"));
    assert!(merged.contains_key("U"));

    let mut collected = BTreeSet::new();
    collect_type_ref_type_params(
        &nested_type_ref("list", vec![nested_type_ref("Boxed", vec![type_ref("T")])]),
        &BTreeMap::from([("list".to_string(), Span::new(1, 1))]),
        &mut collected,
        false,
    );
    assert!(collected.contains("T"));
}

#[test]
fn default_argument_and_trait_bound_helpers_cover_nested_expression_cases() {
    let param_names = vec!["source".to_string(), "fallback".to_string()];
    let default_expr = expr(ExprKind::Call {
        callee: Box::new(expr(ExprKind::Member {
            object: Box::new(expr(ExprKind::FString(vec![
                FormatPart::Literal("value=".to_string()),
                FormatPart::Expr(expr(ExprKind::Name("fallback".to_string()))),
            ]))),
            field: "replace".to_string(),
        })),
        args: vec![Argument {
            name: Some("value".to_string()),
            value: expr(ExprKind::Map(vec![MapEntryExpr {
                key: expr(ExprKind::String("x".to_string())),
                value: expr(ExprKind::Name("source".to_string())),
            }])),
            span: Span::new(1, 1),
        }],
    });
    assert_eq!(
        default_argument_references_param(&default_expr, &param_names),
        Some("fallback".to_string())
    );

    let traits = BTreeMap::from([
        ("Named".to_string(), trait_info("Named", vec![])),
        ("Mapper".to_string(), trait_info("Mapper", vec!["T"])),
    ]);
    let type_names = BTreeMap::from([
        ("str".to_string(), Span::new(1, 1)),
        ("int32".to_string(), Span::new(1, 1)),
    ]);
    let type_arities = BTreeMap::from([("str".to_string(), 0), ("int32".to_string(), 0)]);
    let lowered = lower_trait_bounds(
        &BTreeMap::from([(
            "T".to_string(),
            vec![
                type_ref("Named"),
                nested_type_ref("Mapper", vec![type_ref("str")]),
            ],
        )]),
        &traits,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &type_param_scope(&["T".to_string()]),
    )
    .expect("trait bounds should lower");
    assert_eq!(
        lowered.get("T"),
        Some(&vec![
            TraitBound {
                trait_name: "Named".to_string(),
                trait_args: Vec::new(),
            },
            TraitBound {
                trait_name: "Mapper".to_string(),
                trait_args: vec![Type::named("str")],
            },
        ])
    );
    let merged = merge_trait_bounds(
        &lowered,
        &BTreeMap::from([(
            "T".to_string(),
            vec![TraitBound {
                trait_name: "Extra".to_string(),
                trait_args: Vec::new(),
            }],
        )]),
    );
    assert_eq!(merged.get("T").map(Vec::len), Some(3));
}

#[test]
fn type_pattern_and_collection_helpers_cover_recursive_and_error_paths() {
    let classes = BTreeMap::from([
        (
            "Leaf".to_string(),
            class_info("Leaf", false, vec![("value", Type::named("int32"), false)]),
        ),
        (
            "Node".to_string(),
            class_info("Node", false, vec![("next", Type::named("Leaf"), false)]),
        ),
        (
            "Tree".to_string(),
            class_info("Tree", false, vec![("child", Type::named("Tree"), true)]),
        ),
    ]);

    assert!(type_contains_named(
        &Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("Leaf")]
        ),
        "Leaf"
    ));
    assert!(type_reaches_class_through_non_indirect_fields(
        &Type::named("Node"),
        "Leaf",
        &classes,
        &mut BTreeSet::new()
    ));
    assert!(!type_reaches_class_through_non_indirect_fields(
        &Type::named("Tree"),
        "Leaf",
        &classes,
        &mut BTreeSet::new()
    ));

    let substitutions = HashMap::from([("T".to_string(), Type::named("str"))]);
    assert_eq!(
        substitute_type(
            &Type::Named("list".to_string(), vec![Type::TypeParam("T".to_string())]),
            &substitutions,
        ),
        Type::Named("list".to_string(), vec![Type::named("str")])
    );
    let substituted_bounds = substitute_trait_bounds(
        &BTreeMap::from([(
            "T".to_string(),
            vec![TraitBound {
                trait_name: "Mapper".to_string(),
                trait_args: vec![Type::TypeParam("T".to_string())],
            }],
        )]),
        &substitutions,
    );
    assert_eq!(
        substituted_bounds.get("T"),
        Some(&vec![TraitBound {
            trait_name: "Mapper".to_string(),
            trait_args: vec![Type::named("str")],
        }])
    );

    let mut collected = BTreeSet::new();
    collect_type_params_from_type(
        &Type::Named(
            "Result".to_string(),
            vec![
                Type::TypeParam("T".to_string()),
                Type::TypeParam("E".to_string()),
            ],
        ),
        &mut collected,
    );
    assert_eq!(
        collected,
        BTreeSet::from(["E".to_string(), "T".to_string()])
    );

    let mut substitutions = HashMap::new();
    assert!(type_pattern_matches(
        &Type::Named("list".to_string(), vec![Type::TypeParam("T".to_string())]),
        &Type::Named("list".to_string(), vec![Type::named("int32")]),
        &BTreeSet::from(["T".to_string()]),
        &mut substitutions,
    ));
    assert_eq!(substitutions.get("T"), Some(&Type::named("int32")));
    assert!(has_unresolved_type_params(&Type::TypeParam(
        "T".to_string()
    )));
    assert_eq!(
        substitutions_from_decl_type_args(
            &["K".to_string(), "V".to_string()],
            &[Type::named("str"), Type::named("int32")],
        ),
        HashMap::from([
            ("K".to_string(), Type::named("str")),
            ("V".to_string(), Type::named("int32")),
        ])
    );

    let mut unify = HashMap::new();
    unify_type_pattern(
        &Type::Named(
            "dict".to_string(),
            vec![
                Type::TypeParam("K".to_string()),
                Type::TypeParam("V".to_string()),
            ],
        ),
        &Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        ),
        &mut unify,
    )
    .expect("type pattern should unify");
    assert_eq!(unify.get("K"), Some(&Type::named("str")));
    assert_eq!(unify.get("V"), Some(&Type::named("int32")));
    let conflict = unify_type_pattern(
        &Type::TypeParam("T".to_string()),
        &Type::named("str"),
        &mut HashMap::from([("T".to_string(), Type::named("int32"))]),
    )
    .expect_err("conflicting substitutions should fail");
    assert!(conflict.message.contains("conflicting inferred types"));

    assert_eq!(
        render_literal_pattern_key(&LiteralPatternKey::Int(IntegerValue::from_signed(7))),
        "7"
    );
    assert_eq!(
        render_literal_pattern_key(&LiteralPatternKey::Bool(true)),
        "true"
    );
    assert_eq!(
        render_literal_pattern_key(&LiteralPatternKey::String("aura".to_string())),
        "\"aura\""
    );
    assert_eq!(
        vec_element_type(&Type::Named("list".to_string(), vec![Type::named("str")])),
        Some(&Type::named("str"))
    );
    assert_eq!(
        set_element_type(&Type::Named("set".to_string(), vec![Type::named("bool")])),
        Some(&Type::named("bool"))
    );
    assert_eq!(
        map_key_value_types(&Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        )),
        Some((&Type::named("str"), &Type::named("int32")))
    );
    assert!(place_path("counter.value").overlaps(&place_path("counter")));
    assert!(place_path("counter").overlaps(&place_path("counter.value")));
    assert!(!place_path("counter.left").overlaps(&place_path("counter.right")));
    assert!(!place_path("left.value").overlaps(&place_path("right.value")));
}

#[test]
fn operator_trait_helpers_map_supported_operators() {
    assert_eq!(unary_operator_trait(UnaryOp::Neg), Some(("Neg", "neg")));
    assert_eq!(unary_operator_trait(UnaryOp::Not), Some(("Not", "not")));
    assert_eq!(binary_operator_trait(BinaryOp::Add), Some(("Add", "add")));
    assert_eq!(binary_operator_trait(BinaryOp::Sub), Some(("Sub", "sub")));
    assert_eq!(binary_operator_trait(BinaryOp::Mul), Some(("Mul", "mul")));
    assert_eq!(binary_operator_trait(BinaryOp::Div), Some(("Div", "div")));
    assert_eq!(
        binary_operator_trait(BinaryOp::FloorDiv),
        Some(("FloorDiv", "floor_div"))
    );
    assert_eq!(binary_operator_trait(BinaryOp::Mod), Some(("Mod", "mod")));
    assert_eq!(binary_operator_trait(BinaryOp::Less), Some(("Ord", "lt")));
    assert_eq!(binary_operator_trait(BinaryOp::LessEq), Some(("Ord", "le")));
    assert_eq!(
        binary_operator_trait(BinaryOp::Greater),
        Some(("Ord", "gt"))
    );
    assert_eq!(
        binary_operator_trait(BinaryOp::GreaterEq),
        Some(("Ord", "ge"))
    );
    assert_eq!(binary_operator_trait(BinaryOp::Eq), None);
    assert_eq!(
        TraitBound {
            trait_name: "Mapper".to_string(),
            trait_args: vec![Type::named("str")],
        }
        .to_string(),
        "Mapper[str]"
    );
}

#[test]
fn duration_static_conversions_and_builtin_operator_surface_type_checks() {
    crate::check_source(
        r#"
def duration_surface(count: int64, left: Duration, right: Duration) -> float64:
    from_ms: Duration = Duration.ms(count)
    from_seconds: Duration = Duration.seconds(value=count)
    from_minutes: Duration = Duration.minutes(count)
    added: Duration = left + right
    subtracted: Duration = left - right
    scaled_right: Duration = left * count
    scaled_left: Duration = count * right
    divided: Duration = left // count
    equal: bool = left == right
    unequal: bool = left != right
    less: bool = left < right
    less_equal: bool = left <= right
    greater: bool = left > right
    greater_equal: bool = left >= right
    return from_ms.to_ms() + from_seconds.to_seconds()

def main() -> int32:
    return 0
"#,
    )
    .expect("Duration constructors, conversions, and builtin operators should type-check");

    let duration = Type::named("Duration");
    let int64 = Type::named("int64");
    assert!(FunctionChecker::binary_uses_builtin_value_semantics(
        BinaryOp::Add,
        &duration,
        &duration,
    ));
    assert!(FunctionChecker::binary_uses_builtin_value_semantics(
        BinaryOp::Mul,
        &duration,
        &int64,
    ));
    assert!(FunctionChecker::binary_uses_builtin_value_semantics(
        BinaryOp::Mul,
        &int64,
        &duration,
    ));
    assert!(FunctionChecker::binary_uses_builtin_value_semantics(
        BinaryOp::FloorDiv,
        &duration,
        &int64,
    ));
    assert!(FunctionChecker::binary_uses_builtin_value_semantics(
        BinaryOp::FloorDiv,
        &int64,
        &int64,
    ));
}

#[test]
fn builtin_omitted_marker_is_valid_only_while_checking_generated_defaults() {
    let program = crate::check_source("def main() -> int32:\n    return 0\n")
        .expect("baseline program should type-check");
    let (type_names, type_arities) = type_maps_from_program(&program);
    let checker = checker(
        &program.module_name,
        &type_names,
        &type_arities,
        &program.classes,
        &program.enums,
        &program.functions,
        &program.traits,
        &program.trait_impls,
        &program.imported_modules,
        &program.module_registry,
    );
    let omitted = Expr {
        kind: ExprKind::BuiltinOmitted,
        span: Span::new(1, 1),
    };
    let param = Param {
        name: "timeout".to_string(),
        mode: ParamMode::Default,
        ty: type_ref("Option"),
        default: Some(omitted.clone()),
        span: omitted.span,
    };
    let mut option_param = param;
    option_param.ty = nested_type_ref("Option", vec![type_ref("Duration")]);
    checker
        .check_param_defaults(
            &[option_param],
            &BTreeMap::new(),
            None,
            true,
            "builtin function",
        )
        .expect("generated omitted defaults should match their declared parameter type");

    let error = checker
        .type_of_expr(&omitted, &mut HashMap::new())
        .expect_err("the omitted marker must not behave like a source expression");
    assert!(error
        .message
        .contains("internal builtin omitted-default marker"));
}

#[test]
fn duration_static_and_mixed_operand_errors_name_the_supported_types() {
    for (expression, expected_types) in [
        ("1ms + 1", "Duration` and `int64"),
        ("1ms - 1.0", "Duration` and `float64"),
        ("1ms * 1ms", "Duration` and `Duration"),
        ("1.0 * 1ms", "float64` and `Duration"),
        ("1ms // 1.0", "Duration` and `float64"),
        ("1 // 1ms", "int64` and `Duration"),
        ("1ms / 1ms", "Duration` and `Duration"),
        ("1ms < 1", "Duration` and `int64"),
    ] {
        let source = format!(
            "def invalid():\n    value = {expression}\n\ndef main() -> int32:\n    return 0\n"
        );
        let error = crate::check_source(&source)
            .expect_err("unsupported Duration operand combinations should fail");
        assert_eq!(error.code, "AU2003", "unexpected code for {expression}");
        assert!(
            error.message.contains("unsupported Duration operands"),
            "unexpected diagnostic for {expression}: {}",
            error.message
        );
        assert!(
            error.message.contains(expected_types),
            "diagnostic for {expression} should name {expected_types}: {}",
            error.message
        );
    }

    let constructor_error = crate::check_source(
        "def invalid():\n    value = Duration.seconds(true)\n\ndef main() -> int32:\n    return 0\n",
    )
    .expect_err("Duration constructors should require int64");
    assert!(constructor_error
        .message
        .contains("`Duration.seconds` expects `int64`, found `bool`"));

    let specialized_constructor = crate::check_source(
        "def invalid():\n    value = Duration.seconds[int64](1)\n\ndef main() -> int32:\n    return 0\n",
    )
    .expect_err("Duration constructors should reject explicit type arguments");
    assert!(specialized_constructor
        .message
        .contains("`Duration.seconds` does not take explicit type arguments"));

    let unresolved_constructor_argument = crate::check_source(
        "def invalid():\n    value = Duration.seconds(missing)\n\ndef main() -> int32:\n    return 0\n",
    )
    .expect_err("Duration constructors should preserve argument diagnostics");
    assert!(unresolved_constructor_argument
        .message
        .contains("unknown name `missing`"));
}

#[test]
fn floor_div_operator_dispatches_to_the_floor_div_trait_after_builtins() {
    crate::check_source(
        r#"
trait FloorDiv[Rhs, Out]:
    def floor_div(self, rhs: Rhs) -> Out

class Counter:
    value: int32

impl FloorDiv[Counter, Counter] for Counter:
    def floor_div(self, rhs: Counter) -> Counter:
        return Counter(value=self.value // rhs.value)

def divide(left: Counter, right: Counter) -> Counter:
    return left // right

def main() -> int32:
    return 0
"#,
    )
    .expect("non-builtin floor division should dispatch through FloorDiv.floor_div");
}

#[test]
fn borrowed_copy_return_assignments_bind_as_plain_values() {
    crate::check_source(
        "def id_ref(value: int32) -> int32:\n    return value\n\n\
def main() -> int32:\n    value: int32 = 7\n    mirrored = id_ref(value)\n    return mirrored\n",
    )
    .expect("copy-typed borrowed returns should be bindable as plain values");
}

#[test]
fn default_argument_reference_detection_walks_nested_expression_shapes() {
    let params = vec!["left".to_string(), "right".to_string()];
    let name_left = expr(ExprKind::Name("left".to_string()));
    let name_right = expr(ExprKind::Name("right".to_string()));
    let unrelated = expr(ExprKind::Name("other".to_string()));

    assert_eq!(
        default_argument_references_param(
            &expr(ExprKind::Unary {
                op: UnaryOp::Neg,
                expr: Box::new(name_left.clone()),
            }),
            &params,
        ),
        Some("left".to_string())
    );
    assert_eq!(
        default_argument_references_param(
            &expr(ExprKind::Cast {
                expr: Box::new(name_right.clone()),
                ty: type_ref("int32"),
            }),
            &params,
        ),
        Some("right".to_string())
    );
    assert_eq!(
        default_argument_references_param(
            &expr(ExprKind::Specialize {
                expr: Box::new(name_left.clone()),
                type_args: vec![type_ref("str")],
            }),
            &params,
        ),
        Some("left".to_string())
    );
    assert_eq!(
        default_argument_references_param(
            &expr(ExprKind::Member {
                object: Box::new(name_left.clone()),
                field: "len".to_string(),
            }),
            &params,
        ),
        Some("left".to_string())
    );
    assert_eq!(
        default_argument_references_param(
            &expr(ExprKind::Index {
                object: Box::new(unrelated.clone()),
                index: Box::new(name_right.clone()),
            }),
            &params,
        ),
        Some("right".to_string())
    );
    assert_eq!(
        default_argument_references_param(
            &expr(ExprKind::Call {
                callee: Box::new(unrelated.clone()),
                args: vec![named_arg("value", name_left.clone())],
            }),
            &params,
        ),
        Some("left".to_string())
    );
    assert_eq!(
        default_argument_references_param(
            &expr(ExprKind::Map(vec![MapEntryExpr {
                key: unrelated.clone(),
                value: name_right.clone(),
            }])),
            &params,
        ),
        Some("right".to_string())
    );
    assert_eq!(
        default_argument_references_param(
            &expr(ExprKind::FString(vec![
                FormatPart::Literal("prefix".to_string()),
                FormatPart::Expr(name_left.clone()),
            ])),
            &params,
        ),
        Some("left".to_string())
    );
    assert_eq!(
        default_argument_references_param(
            &expr(ExprKind::Match {
                scrutinee: Box::new(unrelated.clone()),
                capability: ReceiverKind::Borrow,
                arms: vec![MatchExprArm {
                    guard: None,
                    pattern: Pattern::Wildcard(Span::new(1, 1)),
                    value: name_right.clone(),
                    span: Span::new(1, 1),
                }],
            }),
            &params,
        ),
        Some("right".to_string())
    );
    assert_eq!(
        default_argument_references_param(
            &expr(ExprKind::Binary {
                op: BinaryOp::Add,
                left: Box::new(unrelated),
                right: Box::new(name_left),
            }),
            &params,
        ),
        Some("left".to_string())
    );
}

#[test]
fn reserved_type_names_are_rejected() {
    let error = reject_reserved_type_name("Task", Span::new(1, 1))
        .expect_err("reserved built-in type names should fail");
    assert!(error.message.contains("reserved built-in type name"));

    for source in [
        "class Task:\n    value: int32\n\ndef main():\n    pass\n",
        "enum Result:\n    Ok\n\ndef main():\n    pass\n",
        "trait Queue:\n    def label(self) -> str\n\ndef main():\n    pass\n",
    ] {
        let error = crate::check_source(source).expect_err("reserved built-in names should fail");
        assert!(error.message.contains("reserved built-in type name"));
    }

    crate::check_source(
        "class MapEntry:\n    key: int64\n\ndef main():\n    value = MapEntry(key=1)\n    print(value.key)\n",
    )
    .expect("a former helper name must be available as an ordinary user type");

    let unknown = crate::check_source("def inspect(value: MapEntry[int64, str]):\n    pass\n")
        .expect_err("a former helper type must receive the ordinary unknown-type diagnostic");
    assert!(unknown.message.contains("unknown type `MapEntry`"));
}

#[test]
fn check_reports_duplicate_type_params_across_top_level_item_kinds() {
    let cases = [
            (
                "trait Box[T, T]:\n    def show(self) -> str\n\ndef main():\n    pass\n",
                "duplicate type parameter `T` on trait",
            ),
            (
                "trait Box:\n    def show[T, T](self) -> str\n\ndef main():\n    pass\n",
                "duplicate type parameter `T` on trait method",
            ),
            (
                "enum Maybe[T, T]:\n    Some(T)\n\ndef main():\n    pass\n",
                "duplicate type parameter `T` on enum",
            ),
            (
                "class Box[T, T]:\n    value: T\n\ndef main():\n    pass\n",
                "duplicate type parameter `T` on class",
            ),
            (
                "class Box:\n    def show[T, T](self) -> str:\n        return \"x\"\n\ndef main():\n    pass\n",
                "duplicate type parameter `T` on method",
            ),
            (
                "def identity[T, T](value: T) -> T:\n    return value\n\ndef main():\n    pass\n",
                "duplicate type parameter `T` on function",
            ),
            (
                "trait Show:\n    def show(self) -> str\n\nclass Box:\n    value: int32\n\nimpl[T, T] Show for Box:\n    def show(self) -> str:\n        return \"x\"\n\ndef main():\n    pass\n",
                "duplicate type parameter `T` on impl",
            ),
            (
                "trait Mapper[T]:\n    def map(self, value: T) -> T\n\nclass Box[T]:\n    value: T\n\nimpl[T] Mapper[T] for Box[T]:\n    def map[U, U](self, value: T) -> T:\n        return value\n\ndef main():\n    pass\n",
                "duplicate type parameter `U` on impl method",
            ),
        ];

    for (source, expected) in cases {
        let error = crate::check_source(source).expect_err("duplicate type params should fail");
        assert!(
            error.message.contains(expected),
            "expected `{expected}` in `{}`",
            error.message
        );
    }
}

#[test]
fn check_rejects_duplicate_ordinary_parameter_names() {
    let cases = [
        (
            "def choose(value: int32, value: int32) -> int32:\n    return value\n",
            "duplicate parameter `value` on function `choose`",
        ),
        (
            "class Counter:\n    value: int32\n\n    def add(self, amount: int32, amount: int32) -> int32:\n        return self.value + amount\n",
            "duplicate parameter `amount` on method `add`",
        ),
        (
            "trait Combine:\n    def combine(self, other: int32, other: int32) -> int32\n",
            "duplicate parameter `other` on trait method `combine`",
        ),
        (
            "trait Combine:\n    def combine(self, left: int32, right: int32) -> int32\n\nclass Counter:\n    value: int32\n\nimpl Combine for Counter:\n    def combine(self, value: int32, value: int32) -> int32:\n        return self.value + value\n",
            "duplicate parameter `value` on impl method `combine`",
        ),
        (
            "class Counter:\n    value: int32\n\n    def add(self, self: int32) -> int32:\n        return self\n",
            "parameter `self` conflicts with the receiver on method `add`",
        ),
    ];

    for (source, expected) in cases {
        let error = crate::check_source(source).expect_err("duplicate parameters should fail");
        assert!(
            error.message.contains(expected),
            "expected `{expected}` in `{}`",
            error.message
        );
    }
}

#[test]
fn lower_type_covers_builtin_generic_and_error_paths() {
    let type_names = BTreeMap::from([
        ("Box".to_string(), Span::new(1, 1)),
        ("pkg.Counter".to_string(), Span::new(1, 1)),
    ]);
    let type_arities = BTreeMap::from([
        ("Box".to_string(), 1usize),
        ("pkg.Counter".to_string(), 0usize),
    ]);
    let type_params = type_param_scope(&["T".to_string()]);
    let canonical_type_names = BTreeMap::new();

    assert_eq!(
        lower_type(
            &type_ref("str"),
            &type_names,
            &type_arities,
            &canonical_type_names,
            &type_params,
        )
        .expect("str should canonicalize to str"),
        Type::named("str")
    );
    assert_eq!(
        lower_type(
            &nested_type_ref("Option", vec![type_ref("int32")]),
            &type_names,
            &type_arities,
            &canonical_type_names,
            &type_params,
        )
        .expect("Option should lower"),
        Type::Named("Option".to_string(), vec![Type::named("int32")])
    );
    assert_eq!(
        lower_type(
            &nested_type_ref("dict", vec![type_ref("str"), type_ref("int32")]),
            &type_names,
            &type_arities,
            &canonical_type_names,
            &type_params,
        )
        .expect("Map should lower"),
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        )
    );
    assert_eq!(
        lower_type(
            &nested_type_ref("Box", vec![type_ref("T")]),
            &type_names,
            &type_arities,
            &canonical_type_names,
            &type_params,
        )
        .expect("user generic should lower"),
        Type::Named("Box".to_string(), vec![Type::TypeParam("T".to_string())])
    );
    assert_eq!(
        lower_type(
            &type_ref("pkg.Counter"),
            &type_names,
            &type_arities,
            &canonical_type_names,
            &type_params
        )
        .expect("qualified user type should lower"),
        Type::named("pkg.Counter")
    );
    assert_eq!(
        lower_type(
            &type_ref("T"),
            &type_names,
            &type_arities,
            &canonical_type_names,
            &type_params,
        )
        .expect("type param should lower"),
        Type::TypeParam("T".to_string())
    );
    assert_eq!(
        lower_type(
            &type_ref("None"),
            &type_names,
            &type_arities,
            &canonical_type_names,
            &type_params,
        )
        .expect("None should lower to unit"),
        Type::Unit
    );
    assert_eq!(
        lower_type_with_self(
            &type_ref("Self"),
            &type_names,
            &type_arities,
            &canonical_type_names,
            &type_params,
            Some(&Type::named("Counter"))
        )
        .expect("Self should lower with an explicit self type"),
        Type::named("Counter")
    );

    let unknown = lower_type(
        &type_ref("Unknown"),
        &type_names,
        &type_arities,
        &canonical_type_names,
        &type_params,
    )
    .expect_err("unknown types should fail");
    assert!(unknown.message.contains("unknown type `Unknown`"));
    let option_arity = lower_type(
        &nested_type_ref("Option", vec![]),
        &type_names,
        &type_arities,
        &canonical_type_names,
        &type_params,
    )
    .expect_err("Option arity mismatch should fail");
    assert!(option_arity
        .message
        .contains("expects exactly one type argument"));
    let task_group_args = lower_type(
        &nested_type_ref("TaskGroup", vec![type_ref("int32")]),
        &type_names,
        &type_arities,
        &canonical_type_names,
        &type_params,
    )
    .expect_err("TaskGroup should reject type args");
    assert!(task_group_args
        .message
        .contains("does not take type arguments"));
    let self_type_args = lower_type_with_self(
        &nested_type_ref("Self", vec![type_ref("int32")]),
        &type_names,
        &type_arities,
        &canonical_type_names,
        &type_params,
        Some(&Type::named("Counter")),
    )
    .expect_err("Self should reject explicit type arguments");
    assert!(self_type_args
        .message
        .contains("`Self` does not take generic arguments"));
    let self_without_context = lower_type_with_self(
        &type_ref("Self"),
        &type_names,
        &type_arities,
        &canonical_type_names,
        &type_params,
        None,
    )
    .expect_err("Self requires an enclosing self type");
    assert!(self_without_context
        .message
        .contains("`Self` is only available"));
    let type_param_args = lower_type(
        &nested_type_ref("T", vec![type_ref("int32")]),
        &type_names,
        &type_arities,
        &canonical_type_names,
        &type_params,
    )
    .expect_err("type params should reject generic args");
    assert!(type_param_args
        .message
        .contains("type parameter `T` does not take type arguments"));
}

#[test]
fn lower_trait_bounds_reports_unknown_traits_and_arity_mismatches() {
    let traits = BTreeMap::from([
        ("Named".to_string(), trait_info("Named", vec![])),
        ("Mapper".to_string(), trait_info("Mapper", vec!["T"])),
    ]);
    let type_names = BTreeMap::from([("str".to_string(), Span::new(1, 1))]);
    let type_arities = BTreeMap::from([("str".to_string(), 0usize)]);
    let scope = type_param_scope(&["T".to_string()]);

    let unknown = lower_trait_bounds(
        &BTreeMap::from([("T".to_string(), vec![type_ref("Missing")])]),
        &traits,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &scope,
    )
    .expect_err("unknown traits should fail");
    assert!(unknown.message.contains("unknown trait `Missing`"));

    let arity = lower_trait_bounds(
        &BTreeMap::from([("T".to_string(), vec![nested_type_ref("Mapper", vec![])])]),
        &traits,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &scope,
    )
    .expect_err("trait arity mismatch should fail");
    assert!(arity.message.contains("expects 1 type arguments, found 0"));

    let tuple_bound = lower_trait_bounds(
        &BTreeMap::from([(
            "T".to_string(),
            vec![TypeRef::tuple(
                vec![type_ref("Named")],
                false,
                Span::new(2, 8),
            )],
        )]),
        &traits,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &scope,
    )
    .expect_err("a tuple type cannot stand in for a named trait bound");
    assert_eq!(
        tuple_bound.message,
        "a type parameter bound must be a named trait type"
    );
}

#[test]
fn lower_supertraits_reports_unknown_arity_and_lowers_self_args() {
    let traits = BTreeMap::from([
        ("Base".to_string(), trait_info("Base", vec![])),
        ("Mapper".to_string(), trait_info("Mapper", vec!["T"])),
    ]);
    let type_names = BTreeMap::from([
        ("str".to_string(), Span::new(1, 1)),
        ("Widget".to_string(), Span::new(1, 1)),
    ]);
    let type_arities = BTreeMap::from([("str".to_string(), 0usize), ("Widget".to_string(), 0)]);
    let scope = type_param_scope(&["T".to_string()]);

    let unknown = lower_supertraits(
        &[type_ref("Missing")],
        &traits,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &scope,
        Some(&Type::named("Widget")),
    )
    .expect_err("unknown supertraits should fail");
    assert!(unknown.message.contains("unknown trait `Missing`"));

    let arity = lower_supertraits(
        &[nested_type_ref("Mapper", vec![])],
        &traits,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &scope,
        Some(&Type::named("Widget")),
    )
    .expect_err("supertrait arity mismatches should fail");
    assert!(arity
        .message
        .contains("trait `Mapper` expects 1 type arguments, found 0"));

    let lowered = lower_supertraits(
        &[
            type_ref("Base"),
            nested_type_ref("Mapper", vec![type_ref("Self")]),
        ],
        &traits,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &scope,
        Some(&Type::named("Widget")),
    )
    .expect("valid supertraits should lower");
    assert_eq!(
        lowered,
        vec![
            TraitBound {
                trait_name: "Base".to_string(),
                trait_args: Vec::new(),
            },
            TraitBound {
                trait_name: "Mapper".to_string(),
                trait_args: vec![Type::named("Widget")],
            },
        ]
    );

    let bad_arg = lower_supertraits(
        &[nested_type_ref("Mapper", vec![type_ref("MissingType")])],
        &traits,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &scope,
        Some(&Type::named("Widget")),
    )
    .expect_err("supertrait arguments should be type-checked");
    assert!(bad_arg.message.contains("unknown type `MissingType`"));

    let tuple_supertrait = lower_supertraits(
        &[TypeRef::tuple(
            vec![type_ref("Base")],
            false,
            Span::new(3, 7),
        )],
        &traits,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &scope,
        Some(&Type::named("Widget")),
    )
    .expect_err("a tuple type cannot stand in for a named supertrait");
    assert_eq!(
        tuple_supertrait.message,
        "a supertrait must be a named trait type"
    );
}

#[test]
fn function_signature_helper_constructor_is_used() {
    let signature = function_signature(vec![Type::named("int32")], Type::named("bool"));
    assert_eq!(signature.params, vec![Type::named("int32")]);
    assert_eq!(signature.return_type, Type::named("bool"));
    let decl = function_decl("ready");
    assert_eq!(decl.name, "ready");
}

#[test]
fn structured_wait_helpers_cover_valid_and_error_paths() {
    let valid = crate::check_source(
            "def worker(value: int32) -> int32:\n    return value\n\ndef notify(value: int32):\n    print(value)\n\ndef main() -> int32:\n    jobs = Queue[int32]()\n    with TaskGroup() as group:\n        mut tasks = list[Task[int32]]()\n        tasks.append(group.start(worker, 1))\n        group.start_soon(notify, 2)\n        print(wait_any(tasks, timeout=1ms))\n        print(wait_all(tasks))\n    match jobs.get(timeout=1ms):\n        case QueueReceive.TimedOut:\n            pass\n        case _:\n            pass\n    return 0\n",
        )
        .expect("structured wait helpers should type check");
    assert!(valid.functions.contains_key("main"));

    let wait_non_tasks =
        crate::check_source("def main() -> int32:\n    return wait_any(tasks=true)\n")
            .expect_err("wait_any should reject non-task vectors");
    assert!(wait_non_tasks.message.contains("expects `list[Task[T]]`"));

    let wait_timeout = crate::check_source(
            "def worker(value: int32) -> int32:\n    return value\n\ndef main() -> int32:\n    with TaskGroup() as group:\n        mut tasks = list[Task[int32]]()\n        tasks.append(group.start(worker, 1))\n        return wait_all(tasks, timeout=1)\n",
        )
        .expect_err("wait_all timeout should require Duration");
    assert!(wait_timeout
        .message
        .contains("`wait_all(timeout=...)` expects `Duration`, found `int64`"));

    let recv_timeout = crate::check_source(
        "def main() -> int32:\n    jobs = Queue[int32]()\n    return jobs.get(timeout=1)\n",
    )
    .expect_err("queue.get timeout should require Duration");
    assert!(recv_timeout
        .message
        .contains("`get(timeout=...)` expects `Duration`, found `int64`"));

    let send_wrong_type = crate::check_source(
        "def main() -> int32:\n    jobs = Queue[int32]()\n    return jobs.put(\"bad\")\n",
    )
    .expect_err("queue.put should enforce payload types");
    assert!(send_wrong_type.message.contains("`put` expects `int32`"));

    let start_soon_target = crate::check_source(
            "def main() -> int32:\n    with TaskGroup() as group:\n        group.start_soon(1)\n    return 0\n",
        )
        .expect_err("start_soon requires a callable");
    assert!(start_soon_target.message.contains(
        "task starting currently supports named functions and associated methods without `self`"
    ));
}

#[test]
fn typed_select_infers_source_categories_and_builtin_outcome_payloads() {
    crate::check_source(
        r#"
def ready() -> int32:
    return 7

def main():
    jobs = Queue[str]()
    with TaskGroup() as group:
        task = group.start(ready)
        deadline_only: SelectOutcome[None, None] = select(1ms)
        queue_only: SelectOutcome[str, None] = select(jobs)
        task_only: SelectOutcome[None, int32] = select(task)
        mixed: SelectOutcome[str, int32] = select(jobs, task, 1ms)
        match mixed:
            case SelectOutcome.Queue(index, outcome):
                expected_index: int64 = index
                expected_queue: QueueReceive[str] = outcome
            case SelectOutcome.Task(index, outcome):
                expected_index: int64 = index
                expected_task: TaskResult[int32] = outcome
            case SelectOutcome.Deadline(index):
                expected_index: int64 = index
            case SelectOutcome.Cancelled:
                pass
"#,
    )
    .expect("select should infer absent categories as None and type every outcome payload");

    let non_exhaustive = crate::check_source(
        r#"
def main():
    match select(0ms):
        case SelectOutcome.Deadline(index):
            print(index)
"#,
    )
    .expect_err("SelectOutcome matches must cover all outer variants");
    assert_eq!(non_exhaustive.code, "AU2999", "{non_exhaustive:?}");
    assert!(non_exhaustive
        .message
        .contains("non-exhaustive match over `SelectOutcome`"));
    assert!(non_exhaustive.message.contains("Queue"));
    assert!(non_exhaustive.message.contains("Task"));
    assert!(non_exhaustive.message.contains("Cancelled"));
}

#[test]
fn typed_select_rejects_invalid_call_shapes_and_inconsistent_sources() {
    for (source, code, message) in [
        (
            "def main():\n    print(select())\n",
            "AU2004",
            "`select` expects at least one positional source",
        ),
        (
            "def main():\n    jobs = Queue[int32]()\n    print(select(source=jobs))\n",
            "AU2004",
            "`select` does not take keyword arguments",
        ),
        (
            "def main():\n    print(select(1))\n",
            "AU2002",
            "`select` sources must be `Queue[Q]`, `Task[T]`, or `Duration`",
        ),
        (
            "def main():\n    left = Queue[int32]()\n    right = Queue[str]()\n    print(select(left, right))\n",
            "AU2002",
            "all Queue sources in one `select` call must have the same payload type",
        ),
    ] {
        let rejected = crate::check_source(source).expect_err("invalid select should be rejected");
        assert_eq!(rejected.code, code, "{source}: {rejected:?}");
        assert!(rejected.message.contains(message), "{source}: {rejected:?}");
    }

    let mixed_tasks = crate::check_source(
        r#"
def number() -> int32:
    return 1

def text() -> str:
    return "one"

def main():
    with TaskGroup() as group:
        left = group.start(number)
        right = group.start(text)
        print(select(left, right))
"#,
    )
    .expect_err("mixed task result types should be rejected");
    assert_eq!(mixed_tasks.code, "AU2002");
    assert!(mixed_tasks
        .message
        .contains("all Task sources in one `select` call must have the same result type"));
}

#[test]
fn typed_select_consumes_each_nonrepeatable_task_and_rejects_visible_duplicates() {
    crate::check_source(
        "def observe(task: Task[Task[int32]]):\n    print(select(task, task))\n    print(task)\n",
    )
    .expect("recursively repeatable task sources may be duplicated and remain reusable");

    let shared = crate::check_source("def observe(task: Task[str]):\n    print(select(task))\n")
        .expect_err("shared access cannot consume a non-repeatable task source");
    assert_eq!(shared.code, "AU3002");
    assert!(shared
        .message
        .contains("`select` consumes every non-repeatable Task source at call entry"));

    let moved = crate::check_source(
        "def observe(task: own Task[str]):\n    print(select(task))\n    print(task)\n",
    )
    .expect_err("a consumed select source must be moved");
    assert_eq!(moved.code, "AU3001");

    let duplicate =
        crate::check_source("def observe(task: own Task[str]):\n    print(select(task, task))\n")
            .expect_err("one select cannot duplicate a single-consumer observation right");
    assert_eq!(duplicate.code, "AU3009");
    assert_eq!(
        duplicate.message,
        "one `select` call cannot use the same non-repeatable Task source `task` more than once"
    );
}

#[test]
fn typed_select_rejects_non_cloneable_results_and_accepts_inline_task_rights() {
    let non_cloneable = crate::check_source(
        "import random\n\ndef observe(task: own Task[random.Rng]):\n    print(select(task))\n",
    )
    .expect_err("select must not clone a random.Rng task result");
    assert_eq!(non_cloneable.code, "AU3007");
    assert!(non_cloneable.message.contains("select"));
    assert!(non_cloneable.message.contains("non-cloneable `random.Rng`"));

    crate::check_source(
        r#"
def text() -> str:
    return "ready"

def main():
    with TaskGroup() as group:
        outcome: SelectOutcome[None, str] = select(group.start(text))
        print(outcome)
"#,
    )
    .expect("an inline non-repeatable task expression transfers its observation right once");
}

#[test]
fn typed_select_names_are_reserved_builtins() {
    let function = crate::check_source("def select(value: int32):\n    pass\n")
        .expect_err("select builtin function cannot be redefined");
    assert_eq!(function.code, "AU2007");

    let outcome = crate::check_source("enum SelectOutcome:\n    Cancelled\n")
        .expect_err("SelectOutcome builtin enum cannot be redefined");
    assert_eq!(outcome.code, "AU2002");
    assert!(outcome
        .message
        .contains("`SelectOutcome` is a reserved built-in type name"));

    let arity =
        crate::check_source("def observe(outcome: SelectOutcome[int32]):\n    print(outcome)\n")
            .expect_err("SelectOutcome requires queue and task type arguments");
    assert_eq!(arity.code, "AU2002");
    assert!(arity
        .message
        .contains("`SelectOutcome` expects exactly two type arguments"));
}

#[test]
fn checker_function_default_loop_and_resource_validation_cover_additional_branches() {
    for source in [
        "class Job:\n    label: str\n\ndef main() -> None:\n    jobs = Queue[Job]()\n    for job in jobs:\n        pass\n",
        "def main() -> None:\n    jobs = Queue[int32]()\n    for job in jobs:\n        pass\n",
        "class Job:\n    label: str\n\ndef main() -> None:\n    jobs: set[Job] = set[Job]()\n    for job in jobs:\n        pass\n",
    ] {
        crate::check_source(source).expect("supported loop source should type check");
    }

    for (source, expected) in [
            (
                "def helper(value: mut int32 = 1) -> None:\n    pass\n\ndef main() -> None:\n    pass\n",
                "`mut` parameter `value` cannot have a default: the default creates a caller-invisible temporary, so mutations through it would be silently lost; require the caller to pass a value, or take the parameter as `own T` and return the result",
            ),
            (
                "def helper(value: int32 = true) -> int32:\n    return value\n\ndef main() -> None:\n    pass\n",
                "default argument for parameter `value` has type `bool`, expected `int32`",
            ),
            (
                "def helper(left: int32 = 1, right: int32) -> int32:\n    return right\n\ndef main() -> None:\n    pass\n",
                "parameters with default arguments must come after required parameters",
            ),
            (
                "def helper() -> int32:\n    pass\n\ndef main() -> None:\n    pass\n",
                "function `helper` is missing a return",
            ),
            (
                "class Counter:\n    def current(self) -> int32:\n        pass\n",
                "method `current` is missing a return",
            ),
            (
                "trait Show:\n    def show(value: int32) -> int32\n\nclass Box:\n    pass\n\nimpl Show for Box:\n    def show(value: int32 = 1) -> int32:\n        return value\n",
                "default arguments are not allowed in impl method declarations",
            ),
            (
                "trait Show:\n    def show() -> int32\n\nclass Box:\n    pass\n\nimpl Show for Box:\n    def show() -> int32:\n        pass\n",
                "method `show` is missing a return",
            ),
            (
                "def main() -> None:\n    values = {1}\n    for value in mut values:\n        pass\n",
                "`for value in mut ...:` is not supported for `set[T]`; use `add`/`remove` on the set directly",
            ),
            (
                "def main() -> None:\n    values = [1]\n    for value in mut values:\n        pass\n",
                "`for value in mut ...:` requires a mutable `list[T]` place",
            ),
            (
                "def main() -> None:\n    flag = true\n    for value in flag:\n        pass\n",
                "`for` currently requires a `Range`, `Queue[T]`, `list[T]`, or `set[T]` iterable, found `bool`",
            ),
            (
                "def main() -> None:\n    value = 1\n    for value in range(3):\n        pass\n",
                "loop binding `value` would shadow an existing name",
            ),
            (
                "class Resource:\n    def close(mut self):\n        pass\n\ndef main() -> None:\n    resource = Resource()\n    with resource as resource:\n        pass\n",
                "with binding `resource` would shadow an existing name",
            ),
        ] {
            let error = crate::check_source(source).expect_err("source should fail checking");
            assert!(
                error.message.contains(expected),
                "expected `{expected}` in diagnostic, got `{}`",
                error.message
            );
        }
}

#[test]
fn checker_loop_move_helper_reports_full_and_partial_repeated_moves() {
    let program = crate::check_source("class Name:\n    value: str\n\ndef main():\n    pass\n")
        .expect("helper program should type check");
    let (type_names, type_arities) = type_maps_from_program(&program);
    let checker = FunctionChecker::new(
        &program.module_name,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &program.classes,
        &program.enums,
        &program.functions,
        &program.constants,
        &program.traits,
        &program.trait_impls,
        &program.imported_modules,
        &program.module_registry,
    );
    let span = Span::new(2, 3);

    let locals = HashMap::from([(
        "name".to_string(),
        local_binding(
            Type::named("Name"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    let moved_body = HashMap::from([(
        "name".to_string(),
        local_binding(
            Type::named("Name"),
            true,
            true,
            ReceiverKind::Value,
            true,
            &[],
        ),
    )]);
    let moved_error = checker
        .reject_loop_carried_moves(&locals, &moved_body, "while", span)
        .expect_err("repeated full moves from a loop body should be rejected");
    assert!(moved_error
        .message
        .contains("`while` loop body moves `name` and may execute more than once"));

    let partial_body = HashMap::from([(
        "name".to_string(),
        local_binding(
            Type::named("Name"),
            true,
            true,
            ReceiverKind::Value,
            false,
            &["value"],
        ),
    )]);
    let partial_error = checker
        .reject_loop_carried_moves(&locals, &partial_body, "for", span)
        .expect_err("repeated partial moves from a loop body should be rejected");
    assert!(partial_error
        .message
        .contains("`for` loop body partially moves `name` and may execute more than once"));
}

#[test]
fn checker_loop_const_bool_conditions_cover_grouped_and_negated_forms() {
    crate::check_source(
        "class Name:\n    value: str\n\ndef main():\n    name = Name(value=\"aura\")\n    while (false):\n        moved = name.value\n    later = name.value\n",
    )
    .expect("grouped false loops should not merge move state from unreachable bodies");

    let repeated_move = crate::check_source(
        "class Name:\n    value: str\n\ndef main():\n    name = Name(value=\"aura\")\n    while not false:\n        moved = name.value\n",
    )
    .expect_err("negated false loops may execute and should reject repeated moves");
    assert!(repeated_move
        .message
        .contains("`while` loop body partially moves `name` and may execute more than once"));
}

#[test]
fn checker_direct_entrypoints_cover_top_level_function_method_and_impl_paths() {
    let classes = BTreeMap::from([(
        "Counter".to_string(),
        class_info(
            "Counter",
            false,
            vec![("value", Type::named("int32"), false)],
        ),
    )]);
    let type_names = BTreeMap::from([("Counter".to_string(), Span::new(1, 1))]);
    let type_arities = BTreeMap::from([("Counter".to_string(), 0usize)]);
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let span = Span::new(1, 1);

    checker
        .check_top_level(&[Stmt::Pass(PassStmt { span })])
        .expect("top-level pass should be allowed");

    let top_level_return = checker
        .check_top_level(&[Stmt::Return(ReturnStmt {
            value: Some(expr(ExprKind::Int(1))),
            view: None,
            span,
        })])
        .expect_err("top-level return should be rejected");
    assert!(top_level_return
        .message
        .contains("`return` is only allowed inside a function body"));

    let top_level_break = checker
        .check_top_level(&[Stmt::Break(BreakStmt { span })])
        .expect_err("top-level break should be rejected");
    assert!(top_level_break
        .message
        .contains("`break` is only allowed inside a loop"));

    let top_level_continue = checker
        .check_top_level(&[Stmt::Continue(ContinueStmt { span })])
        .expect_err("top-level continue should be rejected");
    assert!(top_level_continue
        .message
        .contains("`continue` is only allowed inside a loop"));

    // ADR-0022 Q6 removed borrowed returns, so an owned local now returns
    // cleanly where the borrowed-source check used to reject it.
    let return_checker = checker.with_return_type(Type::named("str"));
    let mut owned_return_locals = HashMap::from([(
        "owned".to_string(),
        local_binding(
            Type::named("str"),
            true,
            false,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    return_checker
        .check_block(
            &[Stmt::Return(ReturnStmt {
                value: Some(expr(ExprKind::Name("owned".to_string()))),
                view: None,
                span,
            })],
            &mut owned_return_locals,
            &Type::named("str"),
            0,
            true,
        )
        .expect("an owned local is an ordinary owned return");

    let mut function_ok = function_decl("helper");
    function_ok.return_type = type_ref("int32");
    function_ok.body = vec![Stmt::Return(ReturnStmt {
        value: Some(expr(ExprKind::Int(7))),
        view: None,
        span,
    })];
    let function_ok = FunctionInfo {
        module_name: "<test>".to_string(),
        signature: function_signature(Vec::new(), Type::named("int32")),
        type_param_bounds: BTreeMap::new(),
        decl: function_ok,
    };
    checker
        .check_function(&function_ok)
        .expect("ordinary functions with matching returns should pass");

    let mut function_missing_return = function_decl("missing");
    function_missing_return.return_type = type_ref("int32");
    function_missing_return.body = vec![Stmt::Pass(PassStmt { span })];
    let function_missing_return = FunctionInfo {
        module_name: "<test>".to_string(),
        signature: function_signature(Vec::new(), Type::named("int32")),
        type_param_bounds: BTreeMap::new(),
        decl: function_missing_return,
    };
    let function_error = checker
        .check_function(&function_missing_return)
        .expect_err("non-unit functions without returns should fail");
    assert!(function_error
        .message
        .contains("function `missing` is missing a return"));

    let class_decl = classes
        .get("Counter")
        .expect("Counter class info should exist")
        .decl
        .clone();
    let mut method_ok = function_decl("read");
    method_ok.receiver = Some(ReceiverKind::Borrow);
    method_ok.return_type = type_ref("int32");
    method_ok.body = vec![Stmt::Return(ReturnStmt {
        value: Some(expr(ExprKind::Member {
            object: Box::new(expr(ExprKind::Name("self".to_string()))),
            field: "value".to_string(),
        })),
        view: None,
        span,
    })];
    let method_ok = MethodInfo {
        signature: function_signature(Vec::new(), Type::named("int32")),
        type_param_bounds: BTreeMap::new(),
        decl: method_ok,
    };
    checker
        .check_method(&class_decl, &method_ok)
        .expect("class methods should be checked with an implicit self binding");

    let mut method_missing_return = function_decl("stuck");
    method_missing_return.receiver = Some(ReceiverKind::Borrow);
    method_missing_return.return_type = type_ref("int32");
    method_missing_return.body = vec![Stmt::Pass(PassStmt { span })];
    let method_missing_return = MethodInfo {
        signature: function_signature(Vec::new(), Type::named("int32")),
        type_param_bounds: BTreeMap::new(),
        decl: method_missing_return,
    };
    let method_error = checker
        .check_method(&class_decl, &method_missing_return)
        .expect_err("non-unit methods without returns should fail");
    assert!(method_error
        .message
        .contains("method `stuck` is missing a return"));

    let mut impl_method_ok = function_decl("touch");
    impl_method_ok.receiver = Some(ReceiverKind::Borrow);
    impl_method_ok.return_type = type_ref("int32");
    impl_method_ok.body = vec![Stmt::Return(ReturnStmt {
        value: Some(expr(ExprKind::Int(1))),
        view: None,
        span,
    })];
    let impl_method_ok = TraitImplMethodInfo {
        signature: function_signature(Vec::new(), Type::named("int32")),
        type_param_bounds: BTreeMap::new(),
        decl: impl_method_ok,
    };
    checker
        .check_trait_impl_method(
            "Readable",
            &Type::named("Counter"),
            &[],
            &BTreeMap::new(),
            &impl_method_ok,
        )
        .expect("impl methods without defaults should type check");

    let mut impl_method_with_default = function_decl("touch");
    impl_method_with_default.receiver = Some(ReceiverKind::Borrow);
    impl_method_with_default.return_type = type_ref("int32");
    impl_method_with_default.params = vec![Param {
        name: "value".to_string(),
        ty: type_ref("int32"),
        mode: ParamMode::Default,
        default: Some(expr(ExprKind::Int(1))),
        span,
    }];
    impl_method_with_default.body = vec![Stmt::Return(ReturnStmt {
        value: Some(expr(ExprKind::Int(1))),
        view: None,
        span,
    })];
    let impl_method_with_default = TraitImplMethodInfo {
        signature: function_signature(vec![Type::named("int32")], Type::named("int32")),
        type_param_bounds: BTreeMap::new(),
        decl: impl_method_with_default,
    };
    let impl_default_error = checker
        .check_trait_impl_method(
            "Readable",
            &Type::named("Counter"),
            &[],
            &BTreeMap::new(),
            &impl_method_with_default,
        )
        .expect_err("impl methods should still reject default arguments");
    assert!(impl_default_error
        .message
        .contains("default arguments are not allowed in impl method declarations"));
}

#[test]
fn checker_select_and_assignment_direct_helpers_cover_remaining_error_and_success_paths() {
    let classes = BTreeMap::from([(
        "Counter".to_string(),
        class_info(
            "Counter",
            false,
            vec![("value", Type::named("int32"), false)],
        ),
    )]);
    let type_names = BTreeMap::from([("Counter".to_string(), Span::new(1, 1))]);
    let type_arities = BTreeMap::from([("Counter".to_string(), 0usize)]);
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let _span = Span::new(1, 1);

    let mut locals = HashMap::from([
        (
            "values".to_string(),
            local_binding(
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "mapping".to_string(),
            local_binding(
                Type::Named(
                    "dict".to_string(),
                    vec![Type::named("str"), Type::named("int32")],
                ),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "counter".to_string(),
            local_binding(
                Type::named("Counter"),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "jobs".to_string(),
            local_binding(
                Type::Named("Queue".to_string(), vec![Type::named("int32")]),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
    ]);

    let invalid_wait_any = checker
        .type_of_expr(
            &expr(ExprKind::Call {
                callee: Box::new(expr(ExprKind::Name("wait_any".to_string()))),
                args: vec![arg(expr(ExprKind::Bool(true)))],
            }),
            &mut locals,
        )
        .expect_err("wait_any should reject non-task vectors");
    assert!(invalid_wait_any.message.contains("expects `list[Task[T]]`"));

    let index_mut_error = checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Index {
                    object: Box::new(expr(ExprKind::Name("values".to_string()))),
                    index: Box::new(expr(ExprKind::Int(0))),
                },
                true,
                None,
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("index assignments should reject `mut`");
    assert!(index_mut_error
        .message
        .contains("`mut` can only be used when introducing a new binding"));

    let index_annotation_error = checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Index {
                    object: Box::new(expr(ExprKind::Name("values".to_string()))),
                    index: Box::new(expr(ExprKind::Int(0))),
                },
                false,
                Some(type_ref("int32")),
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("index assignments should reject annotations");
    assert!(index_annotation_error
        .message
        .contains("index assignment cannot include a type annotation"));

    checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Index {
                    object: Box::new(expr(ExprKind::Name("values".to_string()))),
                    index: Box::new(expr(ExprKind::Int(0))),
                },
                false,
                None,
                Some(BinaryOp::Add),
                expr(ExprKind::Int(4)),
            ),
            &mut locals,
        )
        .expect("compound vector assignment should type check");

    checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Index {
                    object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                    index: Box::new(expr(ExprKind::String("count".to_string()))),
                },
                false,
                None,
                Some(BinaryOp::Add),
                expr(ExprKind::Int(4)),
            ),
            &mut locals,
        )
        .expect("compound map assignment should type check");

    let member_mut_error = checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Member {
                    object: Box::new(expr(ExprKind::Name("counter".to_string()))),
                    field: "value".to_string(),
                },
                true,
                None,
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("member assignments should reject `mut`");
    assert!(member_mut_error
        .message
        .contains("`mut` can only be used when introducing a new binding"));

    let member_annotation_error = checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Member {
                    object: Box::new(expr(ExprKind::Name("counter".to_string()))),
                    field: "value".to_string(),
                },
                false,
                Some(type_ref("int32")),
                None,
                expr(ExprKind::Int(1)),
            ),
            &mut locals,
        )
        .expect_err("member assignments should reject annotations");
    assert!(member_annotation_error
        .message
        .contains("member assignment cannot include a type annotation"));

    checker
        .check_assign(
            &assign_stmt(
                AssignTarget::Member {
                    object: Box::new(expr(ExprKind::Name("counter".to_string()))),
                    field: "value".to_string(),
                },
                false,
                None,
                Some(BinaryOp::Add),
                expr(ExprKind::Int(4)),
            ),
            &mut locals,
        )
        .expect("compound member assignment should type check");
}

#[test]
fn builtin_call_and_member_resolution_surface_type_checks() {
    let program = crate::check_source(
            "def main() -> int32:\n    text = \"  Aura  \"\n    pieces: list[str] = text.split(\"u\")\n    replaced: str = text.replace(\"Aura\", \"language\")\n    lowered: str = text.to_lower()\n    raised: str = text.to_upper()\n    prefix: Option[str] = text.strip_prefix(\"  \")\n    suffix: Option[str] = text.strip_suffix(\"  \")\n    text_len: int64 = text.len()\n    text_byte_len: int64 = text.byte_len()\n    text_has: bool = text.contains(\"Aur\")\n    text_start: bool = text.starts_with(\"  A\")\n    text_end: bool = text.ends_with(\"  \")\n    parsed_i32: Result[int32, str] = parse_int32(text=\"7\")\n    parsed_i64: Result[int64, str] = parse_int64(text=\"9\")\n    parsed_f64: Result[float64, str] = parse_float64(text=\"3.5\")\n    negative: int32 = -7\n    one: int32 = 1\n    two: int32 = 2\n    abs_i32: int32 = abs(value=negative)\n    min_i32: int32 = min(left=one, right=two)\n    max_i32: int32 = max(left=one, right=two)\n    root: float64 = sqrt(value=9.0)\n    mut values: list[int32] = [1, 2, 3]\n    values_len: int64 = values.len()\n    popped: int32 = values.pop()\n    gotten: Option[int32] = values.get(index=0)\n    values.insert(index=0, value=9)\n    mut counts: dict[str, int32] = {\"a\": 1}\n    counts_len: int64 = counts.len()\n    keys: list[str] = counts.keys()\n    vals: list[int32] = counts.values()\n    items: list[(str, int32)] = counts.items()\n    mut names = {\"ada\"}\n    names_len: int64 = names.len()\n    has_name: bool = \"ada\" in names\n    names.add(\"bob\")\n    names.remove(\"ada\")\n    return (text_len as int32) + abs_i32 + min_i32 + max_i32 + (root as int32)\n",
        )
        .expect("builtin call/member surface should type check");
    assert!(program.functions.contains_key("main"));
}

#[test]
fn checker_builtin_function_success_surface_infers_expected_types() {
    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();
    let classes = BTreeMap::new();
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let span = Span::new(1, 1);
    let string_ty = Type::named("str");
    let float_ty = Type::named("float64");
    let int_ty = Type::named("int32");
    let channel_ty = Type::Named("Queue".to_string(), vec![Type::named("str")]);
    let mut locals = HashMap::from([
        (
            "text".to_string(),
            local_binding(
                string_ty.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "ratio".to_string(),
            local_binding(
                float_ty.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "count".to_string(),
            local_binding(
                int_ty.clone(),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
    ]);

    for (label, callee, args, expected, expected_hint) in [
        (
            "print",
            expr(ExprKind::Name("print".to_string())),
            vec![arg(expr(ExprKind::Name("text".to_string())))],
            Type::Unit,
            None,
        ),
        (
            "range",
            expr(ExprKind::Name("range".to_string())),
            vec![
                named_arg("start", expr(ExprKind::Int(1))),
                named_arg("stop", expr(ExprKind::Int(3))),
            ],
            Type::named("Range"),
            None,
        ),
        (
            "Queue",
            expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("Queue".to_string()))),
                type_args: vec![type_ref("str")],
            }),
            Vec::new(),
            channel_ty.clone(),
            None,
        ),
        (
            "TaskGroup",
            expr(ExprKind::Name("TaskGroup".to_string())),
            Vec::new(),
            Type::named("TaskGroup"),
            None,
        ),
        (
            "TaskGroup[]",
            expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("TaskGroup".to_string()))),
                type_args: Vec::new(),
            }),
            Vec::new(),
            Type::named("TaskGroup"),
            None,
        ),
        (
            "cancelled",
            expr(ExprKind::Name("cancelled".to_string())),
            Vec::new(),
            Type::named("bool"),
            None,
        ),
        (
            "sleep",
            expr(ExprKind::Name("sleep".to_string())),
            vec![arg(expr(ExprKind::DurationNanos(1_000_000)))],
            Type::Unit,
            None,
        ),
        (
            "abs",
            expr(ExprKind::Name("abs".to_string())),
            vec![named_arg(
                "value",
                expr(ExprKind::Name("count".to_string())),
            )],
            int_ty.clone(),
            None,
        ),
        (
            "min",
            expr(ExprKind::Name("min".to_string())),
            vec![
                named_arg("left", expr(ExprKind::Int(1))),
                named_arg("right", expr(ExprKind::Int(2))),
            ],
            Type::named("int64"),
            None,
        ),
        (
            "max",
            expr(ExprKind::Name("max".to_string())),
            vec![
                named_arg("left", expr(ExprKind::Name("ratio".to_string()))),
                named_arg("right", expr(ExprKind::Name("ratio".to_string()))),
            ],
            float_ty.clone(),
            None,
        ),
        (
            "sqrt",
            expr(ExprKind::Name("sqrt".to_string())),
            vec![named_arg(
                "value",
                expr(ExprKind::Name("ratio".to_string())),
            )],
            float_ty.clone(),
            None,
        ),
        (
            "parse_int32",
            expr(ExprKind::Name("parse_int32".to_string())),
            vec![named_arg("text", expr(ExprKind::Name("text".to_string())))],
            Type::Named(
                "Result".to_string(),
                vec![Type::named("int32"), string_ty.clone()],
            ),
            None,
        ),
        (
            "parse_int64",
            expr(ExprKind::Name("parse_int64".to_string())),
            vec![named_arg("text", expr(ExprKind::Name("text".to_string())))],
            Type::Named(
                "Result".to_string(),
                vec![Type::named("int64"), string_ty.clone()],
            ),
            None,
        ),
        (
            "parse_float64",
            expr(ExprKind::Name("parse_float64".to_string())),
            vec![named_arg("text", expr(ExprKind::Name("text".to_string())))],
            Type::Named(
                "Result".to_string(),
                vec![Type::named("float64"), string_ty.clone()],
            ),
            None,
        ),
        (
            "list",
            expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("list".to_string()))),
                type_args: vec![type_ref("int32")],
            }),
            Vec::new(),
            Type::Named("list".to_string(), vec![Type::named("int32")]),
            None,
        ),
        (
            "set",
            expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("set".to_string()))),
                type_args: vec![type_ref("str")],
            }),
            Vec::new(),
            Type::Named("set".to_string(), vec![string_ty.clone()]),
            None,
        ),
        (
            "dict",
            expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("dict".to_string()))),
                type_args: vec![type_ref("str"), type_ref("int32")],
            }),
            Vec::new(),
            Type::Named(
                "dict".to_string(),
                vec![string_ty.clone(), Type::named("int32")],
            ),
            None,
        ),
    ] {
        assert_eq!(
            checker
                .type_of_call(&callee, &args, span, &mut locals, expected_hint.as_ref())
                .unwrap_or_else(|error| panic!(
                    "{label} builtin constructor/call should type check: {error:?}"
                )),
            expected
        );
    }
}

#[test]
fn checker_builtin_constructor_and_variant_error_edges_cover_direct_paths() {
    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();
    let classes = BTreeMap::new();
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let span = Span::new(1, 1);
    let int_ty = Type::named("int32");
    let string_ty = Type::named("str");
    let bool_ty = Type::named("bool");
    let vec_int_ty = Type::Named("list".to_string(), vec![int_ty.clone()]);
    let vec_type_param_ty = Type::Named("list".to_string(), vec![Type::TypeParam("T".to_string())]);
    let vec_task_int_ty = Type::Named(
        "list".to_string(),
        vec![Type::Named("Task".to_string(), vec![int_ty.clone()])],
    );
    let queue_int_ty = Type::Named("Queue".to_string(), vec![int_ty.clone()]);
    let mut locals = HashMap::from([
        (
            "numbers".to_string(),
            local_binding(vec_int_ty, false, false, ReceiverKind::Value, false, &[]),
        ),
        (
            "generic_values".to_string(),
            local_binding(
                vec_type_param_ty,
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "tasks".to_string(),
            local_binding(
                vec_task_int_ty,
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "queue".to_string(),
            local_binding(queue_int_ty, false, false, ReceiverKind::Value, false, &[]),
        ),
        (
            "text".to_string(),
            local_binding(
                string_ty.clone(),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "flag".to_string(),
            local_binding(bool_ty, false, false, ReceiverKind::Value, false, &[]),
        ),
    ]);

    let name = |name: &str| expr(ExprKind::Name(name.to_string()));
    let specialize = |name: &str, type_args: Vec<TypeRef>| {
        expr(ExprKind::Specialize {
            expr: Box::new(expr(ExprKind::Name(name.to_string()))),
            type_args,
        })
    };
    let member = |object: Expr, field: &str| {
        expr(ExprKind::Member {
            object: Box::new(object),
            field: field.to_string(),
        })
    };
    let mut expect_error =
        |callee: Expr, args: Vec<Argument>, expected: Option<Type>, text: &str| {
            let error = checker
                .type_of_call(&callee, &args, span, &mut locals, expected.as_ref())
                .expect_err("checker direct path should report a diagnostic");
            assert!(
                error.message.contains(text),
                "expected diagnostic containing `{text}`, got `{}`",
                error.message
            );
        };

    for (callee, args, expected) in [
        (
            name("TaskGroup"),
            vec![arg(expr(ExprKind::Int(1)))],
            "`TaskGroup` does not take constructor arguments",
        ),
        (
            specialize("TaskGroup", vec![type_ref("int32")]),
            Vec::new(),
            "`TaskGroup` does not take type arguments",
        ),
        (
            specialize("TaskGroup", Vec::new()),
            vec![arg(expr(ExprKind::Int(1)))],
            "`TaskGroup` does not take constructor arguments",
        ),
        (
            specialize("Queue", vec![type_ref("int32"), type_ref("str")]),
            Vec::new(),
            "class `Queue` expects exactly one type argument, found 2",
        ),
        (
            specialize("Queue", vec![type_ref("int32")]),
            vec![named_arg("capacity", expr(ExprKind::Bool(true)))],
            "field `capacity` expects `int32`, found `bool`",
        ),
        (
            specialize("list", vec![type_ref("int32"), type_ref("str")]),
            Vec::new(),
            "class `list` expects exactly one type argument, found 2",
        ),
        (
            specialize("list", vec![type_ref("int32")]),
            vec![arg(expr(ExprKind::Int(1)))],
            "class `list` does not take constructor arguments",
        ),
        (
            specialize("set", vec![type_ref("str"), type_ref("int32")]),
            Vec::new(),
            "class `set` expects exactly one type argument, found 2",
        ),
        (
            specialize("set", vec![type_ref("str")]),
            vec![arg(expr(ExprKind::String("ada".to_string())))],
            "class `set` does not take constructor arguments",
        ),
        (
            specialize("dict", vec![type_ref("str")]),
            Vec::new(),
            "class `dict` expects exactly two type arguments, found 1",
        ),
        (
            specialize("dict", vec![type_ref("str"), type_ref("int32")]),
            vec![arg(expr(ExprKind::Int(1)))],
            "class `dict` does not take constructor arguments",
        ),
    ] {
        expect_error(callee, args, None, expected);
    }

    for (callee, args, expected) in [
        (
            name("wait_any"),
            vec![arg(name("queue"))],
            "`wait_any` expects `list[Task[T]]`, found `Queue[int32]`",
        ),
        (
            name("wait_all"),
            vec![arg(name("numbers"))],
            "`wait_all` expects `list[Task[T]]`, found `list[int32]`",
        ),
        (
            name("wait_any"),
            vec![arg(name("generic_values"))],
            "`wait_any` expects `list[Task[T]]`, found `list[T]`",
        ),
        (
            name("wait_all"),
            vec![
                arg(name("tasks")),
                named_arg("timeout", expr(ExprKind::Int(1))),
            ],
            "`wait_all(timeout=...)` expects `Duration`, found `int64`",
        ),
    ] {
        expect_error(callee, args, None, expected);
    }

    expect_error(
        name("Some"),
        vec![arg(expr(ExprKind::Int(1)))],
        None,
        "bare enum variants require an expected enum type",
    );
    expect_error(
        name("Some"),
        vec![arg(expr(ExprKind::Int(1)))],
        Some(Type::Unit),
        "bare enum variants require an expected enum type",
    );
    expect_error(
        name("Closed"),
        Vec::new(),
        Some(Type::Named("Option".to_string(), vec![int_ty.clone()])),
        "bare enum variants require an expected enum type",
    );
    expect_error(
        member(name("Option"), "None"),
        Vec::new(),
        None,
        "cannot infer type parameter `T` for enum variant `Option.None`",
    );
    drop(expect_error);

    assert_eq!(
        checker
            .type_of_call(
                &member(name("Option"), "Some"),
                &[arg(name("text"))],
                span,
                &mut locals,
                None,
            )
            .expect("bare Option.Some should infer from payload"),
        Type::Named("Option".to_string(), vec![string_ty])
    );
}

#[test]
fn checker_class_constructor_direct_errors_cover_field_binding_edges() {
    let span = Span::new(1, 1);
    let type_names = BTreeMap::from([("Pair".to_string(), span), ("Widget".to_string(), span)]);
    let type_arities = BTreeMap::from([("Pair".to_string(), 0usize), ("Widget".to_string(), 0)]);
    let mut widget = class_info(
        "Widget",
        false,
        vec![
            ("public_value", Type::named("int32"), false),
            ("secret", Type::named("int32"), false),
        ],
    );
    widget.module_name = "pkg".to_string();
    widget.fields.get_mut("secret").unwrap().public = false;
    widget.decl.fields[1].public = false;
    let classes = BTreeMap::from([
        (
            "Pair".to_string(),
            class_info(
                "Pair",
                true,
                vec![
                    ("left", Type::named("int32"), false),
                    ("right", Type::named("int32"), false),
                ],
            ),
        ),
        ("Widget".to_string(), widget),
    ]);
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "main",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let mut locals = HashMap::from([(
        "pkg".to_string(),
        local_binding(
            Type::Module("pkg".to_string()),
            false,
            false,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    let pair = expr(ExprKind::Name("Pair".to_string()));
    let widget = expr(ExprKind::Name("Widget".to_string()));

    for (callee, args, expected) in [
        (
            pair.clone(),
            vec![
                named_arg("left", expr(ExprKind::Int(1))),
                arg(expr(ExprKind::Int(2))),
            ],
            "positional class constructor arguments must come before named arguments",
        ),
        (
            pair.clone(),
            vec![
                arg(expr(ExprKind::Int(1))),
                arg(expr(ExprKind::Int(2))),
                arg(expr(ExprKind::Int(3))),
            ],
            "class constructor `Pair` received too many positional arguments",
        ),
        (
            pair.clone(),
            vec![named_arg("missing", expr(ExprKind::Int(1)))],
            "class `Pair` has no field named `missing`",
        ),
        (
            pair.clone(),
            vec![
                named_arg("left", expr(ExprKind::Int(1))),
                named_arg("left", expr(ExprKind::Int(2))),
            ],
            "field `left` was provided more than once",
        ),
        (
            pair.clone(),
            vec![
                named_arg("left", expr(ExprKind::Bool(true))),
                named_arg("right", expr(ExprKind::Int(2))),
            ],
            "field `left` expects `int32`, found `bool`",
        ),
        (
            pair,
            vec![named_arg("left", expr(ExprKind::Int(1)))],
            "class constructor `Pair` is missing required field `right`",
        ),
        (
            widget.clone(),
            vec![named_arg("public_value", expr(ExprKind::Int(1)))],
            "class constructor `Widget` cannot initialize private field `secret` from another module",
        ),
        (
            widget,
            vec![
                named_arg("public_value", expr(ExprKind::Int(1))),
                named_arg("secret", expr(ExprKind::Int(2))),
            ],
            "field `secret` is private on `Widget`",
        ),
    ] {
        let error = checker
            .type_of_call(&callee, &args, span, &mut locals, None)
            .expect_err("class constructor should report a diagnostic");
        assert!(
            error.message.contains(expected),
            "expected diagnostic containing `{expected}`, got `{}`",
            error.message
        );
    }
}

#[test]
fn method_receiver_borrow_aliasing_checks_overlap_and_distinct_places() {
    let overlap = crate::check_source(
            "class Acc:\n    value: int32\n\n    def add_from(mut self, source: mut Acc):\n        self.value += source.value\n\ndef main() -> int32:\n    mut acc = Acc(value=1)\n    acc.add_from(source=acc)\n    return 0\n",
        )
        .expect_err("overlapping receiver and argument borrows should fail");
    assert!(overlap.message.contains(
            "argument for parameter `source` in method `add_from` overlaps mutable borrow for parameter `self`"
        ));

    let distinct = crate::check_source(
            "class Acc:\n    value: int32\n\n    def add_from(mut self, source: mut Acc):\n        self.value += source.value\n\ndef main() -> int32:\n    mut left = Acc(value=1)\n    mut right = Acc(value=2)\n    left.add_from(source=right)\n    return 0\n",
        )
        .expect("distinct receiver and argument borrows should type check");
    assert!(distinct.functions.contains_key("main"));
}

#[test]
fn shared_self_projection_mutations_and_mutable_borrow_then_consume_are_actionable() {
    for (source, operation) in [
        (
            "class Bucket:\n    values: list[int32]\n\n    def replace(self):\n        self.values[0] = 1\n",
            "indexed assignment",
        ),
        (
            "class Bucket:\n    values: list[int32]\n\n    def append(self):\n        self.values.append(1)\n",
            "mutating member call",
        ),
    ] {
        let diagnostic = crate::check_source(source)
            .expect_err("mutating through a shared self projection should fail");
        assert_eq!(diagnostic.code, "AU3003", "{operation}");
        assert!(
            diagnostic
                .message
                .contains("cannot mutate through shared receiver `self`"),
            "{operation}: {}",
            diagnostic.message
        );
        assert_eq!(diagnostic.secondary_spans.len(), 1, "{operation}");
        assert!(
            diagnostic.secondary_spans[0]
                .label
                .contains("shared receiver `self` is declared here"),
            "{operation}: {:?}",
            diagnostic.secondary_spans
        );
        assert!(
            diagnostic
                .help
                .iter()
                .any(|help| help.contains("declare the receiver as `mut self`")),
            "{operation}: {:?}",
            diagnostic.help
        );
    }

    let overlap = crate::check_source(
        "class Box:\n    value: int32\n\ndef mutate_then_take(first: mut Box, second: own Box):\n    first.value += second.value\n\ndef main() -> int32:\n    mut value = Box(value=1)\n    mutate_then_take(value, value)\n    return 0\n",
    )
    .expect_err("consuming a value while it is mutably borrowed should fail");
    assert_eq!(overlap.code, "AU3002");
    assert!(overlap.message.contains(
        "argument for parameter `second` in function `mutate_then_take` overlaps mutable borrow for parameter `first`; consumed values must be exclusive"
    ));
    assert_eq!(overlap.secondary_spans.len(), 1);
    assert!(overlap.secondary_spans[0]
        .label
        .contains("mutable borrow for parameter `first` begins here"));
    assert!(overlap
        .help
        .iter()
        .any(|help| help.contains("pass non-overlapping places")));
}

#[test]
fn match_borrow_mut_requires_a_mutable_place_scrutinee() {
    for (source, scrutinee_kind) in [
        (
            "enum Opt:\n    Some(int32)\n    None\n\ndef main() -> int32:\n    match mut Opt.Some(1):\n        case Some(value):\n            print(value)\n        case None:\n            pass\n    return 0\n",
            "temporary enum value",
        ),
        (
            "enum Opt:\n    Some(int32)\n    None\n\ndef main() -> int32:\n    value: Opt = Opt.Some(1)\n    match mut value:\n        case Some(found):\n            print(found)\n        case None:\n            pass\n    return 0\n",
            "immutable local place",
        ),
    ] {
        let diagnostic = crate::check_source(source)
            .expect_err("match mut should require a mutable place scrutinee");
        assert_eq!(diagnostic.code, "AU3002", "{scrutinee_kind}");
        assert_eq!(
            diagnostic.message,
            "`match mut` requires a mutable place scrutinee",
            "{scrutinee_kind}"
        );
        assert!(diagnostic.span.is_some(), "{scrutinee_kind}");
    }
}

#[test]
fn checker_match_and_builtin_error_surfaces_cover_remaining_branches() {
    for (source, expected) in [
            (
                "def main() -> int32:\n    match 1:\n        case 1:\n            return 1\n        case 1:\n            return 2\n        case _:\n            return 3\n",
                "duplicate match arm for literal `1`",
            ),
            (
                "def main() -> int32:\n    match true:\n        case 1:\n            return 1\n        case _:\n            return 0\n",
                "literal pattern `1` does not match scrutinee type `bool`",
            ),
            (
                "def main() -> int32:\n    match 1:\n        case 1.0:\n            return 1\n        case _:\n            return 0\n",
                "does not match scrutinee type `int64`",
            ),
            (
                "def main() -> int32:\n    match 1:\n        case _:\n            return 1\n        case 2:\n            return 2\n",
                "wildcard match arm must be the final `case`",
            ),
            (
                "def main() -> int32:\n    match 1:\n        case true:\n            return 1\n        case _:\n            return 0\n",
                "literal pattern `true` does not match scrutinee type `int64`",
            ),
            (
                "def main() -> int32:\n    match 1:\n        case \"aura\":\n            return 1\n        case _:\n            return 0\n",
                "literal pattern \"aura\" does not match scrutinee type `int64`",
            ),
            (
                "def main() -> int32:\n    match true:\n        case true:\n            return 1\n",
                "non-exhaustive match over `bool`: missing `false`",
            ),
            (
                "def main() -> int32:\n    match true:\n        case true:\n            return 1\n        case false:\n            return 0\n        case _:\n            return 2\n",
                "unreachable match arm",
            ),
            (
                "def main() -> int32:\n    match 1:\n        case 1:\n            return 1\n",
                "`match` over `int64` with literal patterns requires a final `case _:` arm",
            ),
            (
                "def main() -> int32:\n    value: int8 = 1\n    match value:\n        case 200:\n            return 1\n        case _:\n            return 0\n",
                "literal pattern `200` does not fit scrutinee type `int8`",
            ),
            (
                "enum Status:\n    Ready\n\ndef main() -> int32:\n    status = Status.Ready\n    match status:\n        case 1:\n            return 1\n        case _:\n            return 0\n",
                "match over `Status` expects enum variant patterns, not literal `1`",
            ),
            (
                "enum Status:\n    Ready\n    Done\n\ndef main() -> int32:\n    status = Status.Ready\n    match status:\n        case _:\n            return 0\n        case Status.Done:\n            return 1\n",
                "wildcard match arm must be the final `case`",
            ),
            (
                "enum Status:\n    Ready\n\ndef main() -> int32:\n    status = Status.Ready\n    match status:\n        case Other.Ready:\n            return 1\n        case _:\n            return 0\n",
                "unknown enum `Other` in match pattern",
            ),
            (
                "enum Status:\n    Ready\n\ndef main() -> int32:\n    status = Status.Ready\n    match status:\n        case Status.Missing:\n            return 1\n        case _:\n            return 0\n",
                "enum `Status` has no variant `Missing`",
            ),
            (
                "enum Status:\n    Ready\n\ndef main() -> int32:\n    status = Status.Ready\n    match status:\n        case Status.Ready:\n            return 1\n        case Status.Ready:\n            return 2\n",
                "duplicate match arm for `Status.Ready`",
            ),
            (
                "enum Status:\n    Done(int32)\n\ndef main() -> int32:\n    status = Status.Done(1)\n    match status:\n        case Status.Done:\n            return 1\n        case _:\n            return 0\n",
                "variant `Status.Done` carries a payload and must bind it",
            ),
            (
                "enum Status:\n    Ready\n\ndef main() -> int32:\n    status = Status.Ready\n    match status:\n        case Status.Ready(value):\n            return 1\n        case _:\n            return 0\n",
                "variant `Status.Ready` does not carry a payload",
            ),
            (
                "enum Status:\n    Done(int32)\n\ndef main() -> int32:\n    value = 1\n    status = Status.Done(1)\n    match status:\n        case Status.Done(value):\n            return value\n        case _:\n            return 0\n",
                "pattern binding `value` would shadow an existing name",
            ),
            (
                "enum Leaf:\n    Value(int32)\n\nenum Outer:\n    Wrap(Leaf)\n\ndef main() -> int32:\n    value = Outer.Wrap(value=Leaf.Value(value=1))\n    match value:\n        case Outer.Wrap(Leaf.Value(left, right)):\n            return left\n        case _:\n            return 0\n",
                "variant `Leaf.Value` expects 1 pattern payload, found 2",
            ),
            (
                "enum Status:\n    Ready\n    Done\n\ndef main() -> int32:\n    status = Status.Ready\n    match status:\n        case Status.Ready:\n            return 1\n",
                "non-exhaustive match over `Status`: missing `Done`",
            ),
            (
                "enum Status:\n    Ready\n\ndef main() -> int32:\n    match 1:\n        case Status.Ready:\n            return 1\n        case _:\n            return 0\n",
                "match over `int64` only supports literal patterns and `_`",
            ),
            (
                "def main() -> int32:\n    return range(start=true, stop=3)\n",
                "`range` arguments must have type `int64` or a losslessly narrower integer type, found `bool`",
            ),
            (
                "def main() -> int32:\n    return wait_any(tasks=true)\n",
                "`wait_any` expects `list[Task[T]]`, found `bool`",
            ),
            (
                "def main() -> int32:\n    jobs = Queue[int32]()\n    return jobs.get(timeout=1)\n",
                "`get(timeout=...)` expects `Duration`, found `int64`",
            ),
            (
                "def main() -> int32:\n    return sleep(duration=1)\n",
                "`sleep(...)` expects a `Duration`, found `int64`",
            ),
            (
                "def main() -> int32:\n    return abs(value=\"x\")\n",
                "`abs(...)` expects an integer or float value, found `str`",
            ),
            (
                "def main() -> int32:\n    return min(left=true, right=false)\n",
                "`min` expects numeric arguments, found `bool`",
            ),
            (
                "def main() -> int32:\n    return max(left=1, right=2.0)\n",
                "`max` arguments must match, found `int64` and `float64`",
            ),
            (
                "def main() -> int32:\n    return sqrt(value=9)\n",
                "`sqrt(...)` expects `float32` or `float64`, found `int64`",
            ),
            (
                "def main() -> int32:\n    return parse_int32(text=1)\n",
                "`parse_int32(...)` expects `str`, found `int64`",
            ),
            (
                "def main() -> int32:\n    return parse_int64(text=1)\n",
                "`parse_int64(...)` expects `str`, found `int64`",
            ),
            (
                "def main() -> int32:\n    return parse_float64(text=1)\n",
                "`parse_float64(...)` expects `str`, found `int64`",
            ),
            (
                "def main() -> int32:\n    text = \"aura\"\n    ok: bool = text.contains(1)\n    return 0\n",
                "`contains` expects `str`, found `int64`",
            ),
            (
                "def main() -> int32:\n    text = \"aura\"\n    replaced: str = text.replace(1, \"x\")\n    return 0\n",
                "`replace` expects `str` for `from`, found `int64`",
            ),
            (
                "def main() -> int32:\n    text = \"aura\"\n    replaced: str = text.replace(\"a\", 1)\n    return 0\n",
                "`replace` expects `str` for `to`, found `int64`",
            ),
            (
                "def main() -> None:\n    mut values = [1]\n    values.append(\"x\")\n",
                "`append` expects `int64`, found `str`",
            ),
            (
                "import fs\n\ndef main() -> int32:\n    file = fs.File()\n    return 0\n",
                "builtin resource `fs.File` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    stream = net.TcpStream()\n    return 0\n",
                "builtin resource `net.TcpStream` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    listener = net.TcpListener()\n    return 0\n",
                "builtin resource `net.TcpListener` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    socket = net.UdpSocket()\n    return 0\n",
                "builtin resource `net.UdpSocket` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    datagram = net.UdpDatagram()\n    return 0\n",
                "builtin resource `net.UdpDatagram` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    listener = net.HttpListener()\n    return 0\n",
                "builtin resource `net.HttpListener` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    exchange = net.HttpExchange()\n    return 0\n",
                "builtin resource `net.HttpExchange` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    response = net.HttpResponse()\n    return 0\n",
                "builtin resource `net.HttpResponse` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    listener = net.WebSocketListener()\n    return 0\n",
                "builtin resource `net.WebSocketListener` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    socket = net.WebSocket()\n    return 0\n",
                "builtin resource `net.WebSocket` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    listener = net.UnixListener()\n    return 0\n",
                "builtin resource `net.UnixListener` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    stream = net.UnixStream()\n    return 0\n",
                "builtin resource `net.UnixStream` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    listener = net.TlsListener()\n    return 0\n",
                "builtin resource `net.TlsListener` must be created through its module functions",
            ),
            (
                "import net\n\ndef main() -> int32:\n    stream = net.TlsStream()\n    return 0\n",
                "builtin resource `net.TlsStream` must be created through its module functions",
            ),
            (
                "class Resource[T]:\n    value: T\n\n    def close(mut self):\n        pass\n\ndef main() -> None:\n    resource = Resource[int32](value=1)\n    with resource as handle:\n        pass\n",
                "`with` does not yet support generic resource types",
            ),
            (
                "class Resource:\n    value: int32\n\ndef main() -> None:\n    resource = Resource(value=1)\n    with resource as handle:\n        pass\n",
                "does not define `close(mut self)`",
            ),
            (
                "class Resource:\n    value: int32\n\n    def close(self) -> int32:\n        return 0\n\ndef main() -> None:\n    resource = Resource(value=1)\n    with resource as handle:\n        pass\n",
                "`with` resources must define `close(mut self)` returning `None`",
            ),
            (
                "def make() -> Option[int32]:\n    return Option[int32].Missing()\n\ndef main():\n    pass\n",
                "enum `Option` has no variant `Missing`",
            ),
            (
                "def make() -> Option[int32]:\n    return Option[int32].None(1)\n\ndef main():\n    pass\n",
                "variant `None` of enum `Option` does not take a payload",
            ),
            (
                "def make() -> Option[int32]:\n    return Option[int32].Some()\n\ndef main():\n    pass\n",
                "variant `Some` of enum `Option` expects 1 payload argument, found 0",
            ),
            (
                "def make() -> Option[int32]:\n    return Option[int32].Some(\"x\")\n\ndef main():\n    pass\n",
                "variant `Some` of enum `Option` expects `int32`, found `str`",
            ),
            (
                "def make() -> Option[int32]:\n    return Option.None(1)\n\ndef main():\n    pass\n",
                "variant `None` of enum `Option` does not take a payload",
            ),
            (
                "def make() -> Option[int32]:\n    return Option[int32].Some(value=1, extra=2)\n\ndef main():\n    pass\n",
                "variant `Some` of enum `Option` expects 1 payload argument, found 2",
            ),
            (
                "enum Pair:\n    Both(int32, int32)\n\ndef make() -> Pair:\n    return Pair.Both(left=1, right=2)\n\ndef main():\n    pass\n",
                "variant `Both` of enum `Pair` uses positional payloads and cannot be constructed with named arguments",
            ),
        ] {
            let error = crate::check_source(source)
                .expect_err("checker surface case should report a diagnostic");
            assert!(
                error.message.contains(expected),
                "expected diagnostic containing `{expected}`, got `{}` for source:\n{}",
                error.message,
                source
            );
        }

    for (source, expected) in [
        (
            "enum Status:\n    Ready\n\ndef main() -> int32:\n    return match 1:\n        case Status.Ready: 1\n        case _: 0\n",
            "match over `int64` only supports literal patterns and `_`",
        ),
        (
            "def main() -> int32:\n    return match 1:\n        case _: 0\n        case 1: 1\n",
            "wildcard match arm must be the final `case`",
        ),
        (
            "def main() -> int32:\n    return match 1:\n        case 1: 1\n        case 1: 2\n        case _: 3\n",
            "unreachable match arm",
        ),
        (
            "def main() -> int32:\n    return match true:\n        case true: 1\n",
            "non-exhaustive bool match: missing `false`",
        ),
        (
            "def main() -> int32:\n    return match 1:\n        case 1: 1\n",
            "match over `int64` requires a final wildcard arm because the domain is open-ended",
        ),
        (
            "def main() -> int32:\n    return match true:\n        case true: 1\n        case false: \"no\"\n",
            "match arm expression expects `int32`, found `str`",
        ),
        (
            "enum Status:\n    Ready\n\ndef main() -> int32:\n    status = Status.Ready\n    return match status:\n        case 1: 1\n        case _: 0\n",
            "match over `Status` expects enum variant patterns, not literal `1`",
        ),
        (
            "enum Status:\n    Ready\n\ndef main() -> int32:\n    status = Status.Ready\n    return match status:\n        case Other.Ready: 1\n        case _: 0\n",
            "unknown enum `Other` in match pattern",
        ),
        (
            "enum Status:\n    Ready\n\nenum Other:\n    Ready\n\ndef main() -> int32:\n    status = Status.Ready\n    return match status:\n        case Other.Ready: 1\n        case _: 0\n",
            "match arm expects enum `Status`, found pattern for `Other`",
        ),
        (
            "enum Status:\n    Ready\n    Done\n\ndef main() -> int32:\n    status = Status.Ready\n    return match status:\n        case _: 0\n        case Status.Done: 1\n",
            "wildcard match arm must be the final `case`",
        ),
        (
            "enum Status:\n    Ready\n\ndef main() -> int32:\n    status = Status.Ready\n    return match status:\n        case Status.Missing: 1\n        case _: 0\n",
            "enum `Status` has no variant `Missing`",
        ),
        (
            "enum Status:\n    Done(int32)\n\ndef main() -> int32:\n    status = Status.Done(1)\n    return match status:\n        case Status.Done: 1\n        case _: 0\n",
            "variant `Status.Done` carries a payload and must bind it",
        ),
        (
            "enum Status:\n    Ready\n\ndef main() -> int32:\n    status = Status.Ready\n    return match status:\n        case Status.Ready(value): 1\n        case _: 0\n",
            "variant `Status.Ready` does not carry a payload",
        ),
        (
            "enum Status:\n    Ready\n\ndef main() -> int32:\n    status = Status.Ready\n    return match status:\n        case Status.Ready: 1\n        case Status.Ready: 2\n",
            "unreachable match arm",
        ),
        (
            "enum Status:\n    Ready\n    Done\n\ndef main() -> int32:\n    status = Status.Ready\n    return match status:\n        case Status.Ready: 1\n        case Status.Done: \"no\"\n",
            "match arm expression expects `int32`, found `str`",
        ),
        (
            "enum Status:\n    Ready\n    Done\n\ndef main() -> int32:\n    status = Status.Ready\n    return match status:\n        case Status.Ready: 1\n",
            "non-exhaustive match over `Status`: missing `Done`",
        ),
    ] {
        let error = crate::check_source(source)
            .expect_err("checker match expression surface should report a diagnostic");
        assert!(
            error.message.contains(expected),
            "expected diagnostic containing `{expected}`, got `{}` for source:\n{}",
            error.message,
            source
        );
    }

    for source in [
        "class Packet:\n    value: int32\n\ndef main() -> int32:\n    packet = Packet(value=1)\n    match packet:\n        case _:\n            return 0\n",
        "class Packet:\n    value: int32\n\ndef main() -> int32:\n    packet = Packet(value=1)\n    return match packet:\n        case _: 0\n",
    ] {
        crate::check_source(source)
            .expect("a wildcard-only match may cover a class value without destructuring it");
    }

    let span = Span::new(1, 1);
    let enums = BTreeMap::from([
        ("Other".to_string(), enum_info("Other", None)),
        (
            "PayloadStatus".to_string(),
            enum_info("PayloadStatus", Some(Type::named("int32"))),
        ),
        ("Status".to_string(), enum_info("Status", None)),
    ]);
    let type_names = BTreeMap::from([
        ("Other".to_string(), span),
        ("PayloadStatus".to_string(), span),
        ("Status".to_string(), span),
    ]);
    let type_arities = BTreeMap::from([
        ("Other".to_string(), 0usize),
        ("PayloadStatus".to_string(), 0usize),
        ("Status".to_string(), 0usize),
    ]);
    let classes = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let mut locals = HashMap::from([(
        "status".to_string(),
        local_binding(
            Type::named("Status"),
            false,
            false,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    let empty_match_expr = expr(ExprKind::Match {
        scrutinee: Box::new(expr(ExprKind::Name("status".to_string()))),
        capability: ReceiverKind::Borrow,
        arms: Vec::new(),
    });
    assert!(checker
        .type_of_expr(&empty_match_expr, &mut locals)
        .expect_err("empty enum match expression should be rejected")
        .message
        .contains("`match` requires at least one `case` arm"));

    let variant_pattern =
        |enum_name: Option<&str>, variant_name: &str, subpatterns: Vec<Pattern>| {
            Pattern::Variant(crate::ast::VariantPattern {
                enum_name: enum_name.map(str::to_string),
                variant_name: variant_name.to_string(),
                subpatterns,
                span,
            })
        };
    for (pattern, expected_ty, expected) in [
        (
            variant_pattern(None, "Value", Vec::new()),
            Type::named("int32"),
            "pattern `Value` expects an enum scrutinee, found `int32`",
        ),
        (
            variant_pattern(Some("Other"), "Value", Vec::new()),
            Type::named("Status"),
            "match arm expects enum `Status`, found pattern for `Other`",
        ),
        (
            variant_pattern(None, "Missing", Vec::new()),
            Type::named("Status"),
            "enum `Status` has no variant `Missing`",
        ),
        (
            variant_pattern(None, "Value", Vec::new()),
            Type::named("PayloadStatus"),
            "variant `PayloadStatus.Value` carries a payload and must bind it",
        ),
        (
            variant_pattern(None, "Value", vec![Pattern::Wildcard(span)]),
            Type::named("Status"),
            "variant `Status.Value` does not carry a payload",
        ),
    ] {
        assert!(
            checker
                .bind_pattern_locals(
                    &pattern,
                    &expected_ty,
                    &mut locals,
                    ReceiverKind::Borrow,
                    None,
                    None
                )
                .expect_err("direct pattern binding diagnostic should be reported")
                .message
                .contains(expected),
            "expected direct pattern diagnostic containing `{expected}`"
        );
    }

    let empty_match = crate::ast::Stmt::Match(crate::ast::MatchStmt {
        scrutinee: expr(ExprKind::Name("status".to_string())),
        capability: ReceiverKind::Borrow,
        arms: Vec::new(),
        span,
    });
    assert!(checker
        .check_block(&[empty_match], &mut locals, &Type::named("int32"), 0, true)
        .expect_err("empty enum match should be rejected")
        .message
        .contains("`match` requires at least one `case` arm"));
}

#[test]
fn checker_module_member_type_edges_cover_private_and_uncalled_members() {
    let span = Span::new(1, 1);
    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let mut imported_modules = BTreeMap::new();
    let mut module_registry = BTreeMap::new();
    let mut root = namespace("pkg");
    root.functions.insert(
        "make".to_string(),
        FunctionInfo {
            module_name: "pkg".to_string(),
            decl: function_decl("make"),
            signature: function_signature(Vec::new(), Type::Unit),
            type_param_bounds: BTreeMap::new(),
        },
    );
    let mut widget = class_info(
        "Widget",
        false,
        vec![
            ("value", Type::named("int32"), false),
            ("secret", Type::named("str"), false),
        ],
    );
    widget.module_name = "pkg".to_string();
    widget.fields.get_mut("secret").unwrap().public = false;
    widget.decl.fields[1].public = false;
    let mut hidden = function_decl("hidden");
    hidden.public = false;
    hidden.receiver = Some(ReceiverKind::Borrow);
    widget.methods.insert(
        "hidden".to_string(),
        MethodInfo {
            decl: hidden,
            signature: function_signature(Vec::new(), Type::named("str")),
            type_param_bounds: BTreeMap::new(),
        },
    );
    root.classes.insert("Widget".to_string(), widget.clone());
    root.enums
        .insert("Status".to_string(), enum_info("Status", None));
    imported_modules.insert("pkg".to_string(), root.clone());
    module_registry.insert("pkg".to_string(), root);
    let classes = BTreeMap::from([("Widget".to_string(), widget)]);
    let checker = checker(
        "main",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );

    for (object_ty, field, expected) in [
        (
            Type::Module("pkg".to_string()),
            "make",
            "function `make` from module `pkg` must be called with `(...)`",
        ),
        (
            Type::Module("pkg".to_string()),
            "Widget",
            "class `Widget` from module `pkg` must be constructed with `(...)`",
        ),
        (
            Type::Module("pkg".to_string()),
            "missing",
            "module `pkg` has no member `missing`",
        ),
        (
            Type::Named("Option".to_string(), vec![Type::named("int32")]),
            "Some",
            "variant `Some` of enum `Option` requires a payload",
        ),
        (
            Type::named("Widget"),
            "secret",
            "field `secret` is private on `Widget`",
        ),
        (
            Type::named("Widget"),
            "hidden",
            "method `hidden` is private on `Widget`",
        ),
    ] {
        let error = checker
            .resolve_member_type(&object_ty, field, span)
            .expect_err("member type lookup should report the expected diagnostic");
        assert!(
            error.message.contains(expected),
            "expected diagnostic containing `{expected}`, got `{}`",
            error.message
        );
    }
}

#[test]
fn operator_trait_and_bound_helpers_cover_checker_resolution_paths() {
    let bad_ord = crate::check_source(
        "\
trait Ord[Rhs]:
    def lt(self, rhs: Rhs) -> Score

class Score:
    value: int32

impl Ord[Score] for Score:
    def lt(self, rhs: Score) -> Score:
        return self

def main() -> int32:
    left = Score(value=1)
    right = Score(value=2)
    if left < right:
        return 1
    return 0
",
    )
    .expect_err("ordering operator traits must return bool");
    assert!(bad_ord
        .message
        .contains("operator trait `Ord` for `lt` must return `bool`"));

    let program = crate::check_source(
        "\
trait Named:
    def name(self) -> str

trait Add[Rhs, Out]:
    def add(self, rhs: own Rhs) -> Out

trait Neg[Out]:
    def neg(self) -> Out

class User:
    label: str

class Point:
    x: int32

class Box[T]:
    value: T

impl Named for User:
    def name(self) -> str:
        return self.label.clone()

impl Add[Point, Point] for Point:
    def add(self, rhs: own Point) -> Point:
        return Point(x=self.x + rhs.x)

impl Neg[Point] for Point:
    def neg(self) -> Point:
        return Point(x=0 - self.x)

impl[T: Named] Add[Box[T], Box[T]] for Box[T]:
    def add(self, rhs: own Box[T]) -> Box[T]:
        return rhs

def main():
    pass
",
    )
    .expect("operator trait program should type-check");
    let (type_names, type_arities) = type_maps_from_program(&program);
    let span = Span::new(1, 1);
    let base_checker = checker(
        &program.module_name,
        &type_names,
        &type_arities,
        &program.classes,
        &program.enums,
        &program.functions,
        &program.traits,
        &program.trait_impls,
        &program.imported_modules,
        &program.module_registry,
    );

    assert_eq!(
        base_checker
            .type_of_unary_operator_via_trait(span, UnaryOp::Neg, &Type::named("Point"))
            .expect("neg trait lookup should succeed"),
        Some(ResolvedUnaryOperatorAccess {
            return_type: Type::named("Point"),
            receiver_passing: ReceiverKind::Borrow,
        })
    );
    assert_eq!(
        base_checker
            .type_of_binary_operator_via_trait(
                span,
                BinaryOp::Add,
                &Type::named("Point"),
                &Type::named("Point"),
            )
            .expect("add trait lookup should succeed"),
        Some(ResolvedBinaryOperatorAccess {
            return_type: Type::named("Point"),
            receiver_passing: ReceiverKind::Borrow,
            rhs_passing: ReceiverKind::Value,
        })
    );
    let box_user = Type::Named("Box".to_string(), vec![Type::named("User")]);
    assert_eq!(
        base_checker
            .type_of_binary_operator_via_trait(span, BinaryOp::Add, &box_user, &box_user)
            .expect("generic add impl should satisfy its Named bound for User"),
        Some(ResolvedBinaryOperatorAccess {
            return_type: box_user.clone(),
            receiver_passing: ReceiverKind::Borrow,
            rhs_passing: ReceiverKind::Value,
        })
    );
    let box_point = Type::Named("Box".to_string(), vec![Type::named("Point")]);
    assert_eq!(
        base_checker
            .type_of_binary_operator_via_trait(span, BinaryOp::Add, &box_point, &box_point)
            .expect("generic add impl with unsatisfied bounds should be ignored"),
        None
    );
    assert_eq!(
        base_checker
            .type_of_binary_operator_via_trait(
                span,
                BinaryOp::And,
                &Type::named("bool"),
                &Type::named("bool"),
            )
            .expect("boolean operators do not resolve through traits"),
        None
    );
    let trait_method_value = base_checker
        .resolve_member_type(&Type::named("User"), "name", span)
        .expect_err("trait-dispatched method values are explicitly out of scope");
    assert_eq!(trait_method_value.code, "AU2005");
    assert!(trait_method_value
        .message
        .contains("trait-dispatched method values are not supported"));
    base_checker
        .assert_type_satisfies_bounds(
            &Type::named("User"),
            &[TraitBound {
                trait_name: "Named".to_string(),
                trait_args: Vec::new(),
            }],
            span,
        )
        .expect("User should satisfy Named");
    let concrete_bound_error = base_checker
        .assert_type_satisfies_bounds(
            &Type::named("Point"),
            &[TraitBound {
                trait_name: "Named".to_string(),
                trait_args: Vec::new(),
            }],
            span,
        )
        .expect_err("Point should not satisfy Named");
    assert!(concrete_bound_error
        .message
        .contains("type `Point` does not implement trait `Named`"));

    let type_param_checker = base_checker.with_type_params(
        BTreeMap::from([("T".to_string(), ()), ("U".to_string(), ())]),
        BTreeMap::from([
            (
                "T".to_string(),
                vec![TraitBound {
                    trait_name: "Add".to_string(),
                    trait_args: vec![Type::named("Point"), Type::named("Point")],
                }],
            ),
            (
                "U".to_string(),
                vec![TraitBound {
                    trait_name: "Neg".to_string(),
                    trait_args: vec![Type::named("Point")],
                }],
            ),
        ]),
    );
    assert_eq!(
        type_param_checker
            .type_of_binary_operator_via_trait(
                span,
                BinaryOp::Add,
                &Type::TypeParam("T".to_string()),
                &Type::named("Point"),
            )
            .expect("type-param add bound should resolve"),
        Some(ResolvedBinaryOperatorAccess {
            return_type: Type::named("Point"),
            receiver_passing: ReceiverKind::Borrow,
            rhs_passing: ReceiverKind::Value,
        })
    );
    assert_eq!(
            type_param_checker
                .type_of_unary_operator_via_trait(
                    span,
                    UnaryOp::Neg,
                    &Type::TypeParam("U".to_string()),
                )
                .expect("type-param neg bound should resolve"),
            Some(ResolvedUnaryOperatorAccess {
                return_type: Type::named("Point"),
                receiver_passing: ReceiverKind::Borrow,
            })
        );
    let type_param_bound_error = type_param_checker
        .assert_type_satisfies_bounds(
            &Type::TypeParam("T".to_string()),
            &[TraitBound {
                trait_name: "Named".to_string(),
                trait_args: Vec::new(),
            }],
            span,
        )
        .expect_err("type parameter without Named bound should fail");
    assert!(type_param_bound_error
        .message
        .contains("type parameter `T` does not satisfy trait bound `Named`"));

    let no_traits = BTreeMap::new();
    let no_trait_impls = Vec::new();
    let no_trait_checker = checker(
        &program.module_name,
        &type_names,
        &type_arities,
        &program.classes,
        &program.enums,
        &program.functions,
        &no_traits,
        &no_trait_impls,
        &program.imported_modules,
        &program.module_registry,
    )
    .with_type_params(BTreeMap::from([("T".to_string(), ())]), BTreeMap::new());
    assert!(no_trait_checker
        .operator_method_from_type_param("T", "Add", "add", Some(&Type::named("Point")))
        .expect("missing operator traits should be ignored for type params")
        .is_none());

    let mut broken_add_trait = program.traits["Add"].clone();
    broken_add_trait.methods.clear();
    let broken_traits = BTreeMap::from([("Add".to_string(), broken_add_trait)]);
    let broken_trait_checker = checker(
        &program.module_name,
        &type_names,
        &type_arities,
        &program.classes,
        &program.enums,
        &program.functions,
        &broken_traits,
        &program.trait_impls,
        &program.imported_modules,
        &program.module_registry,
    )
    .with_type_params(BTreeMap::from([("T".to_string(), ())]), BTreeMap::new());
    let missing_method = match broken_trait_checker.operator_method_from_type_param(
        "T",
        "Add",
        "add",
        Some(&Type::named("Point")),
    ) {
        Ok(_) => panic!("operator traits must expose the expected method"),
        Err(error) => error,
    };
    assert!(missing_method
        .message
        .contains("operator trait `Add` must define method `add`"));

    let named_only_checker = base_checker.with_type_params(
        BTreeMap::from([("T".to_string(), ())]),
        BTreeMap::from([(
            "T".to_string(),
            vec![TraitBound {
                trait_name: "Named".to_string(),
                trait_args: Vec::new(),
            }],
        )]),
    );
    assert!(named_only_checker
        .operator_method_from_type_param("T", "Add", "add", Some(&Type::named("Point")))
        .expect("unrelated type-param bounds should not match Add")
        .is_none());

    let wrong_rhs_checker = base_checker.with_type_params(
        BTreeMap::from([("T".to_string(), ())]),
        BTreeMap::from([(
            "T".to_string(),
            vec![TraitBound {
                trait_name: "Add".to_string(),
                trait_args: vec![Type::named("str"), Type::named("Point")],
            }],
        )]),
    );
    assert!(wrong_rhs_checker
        .operator_method_from_type_param("T", "Add", "add", Some(&Type::named("Point")))
        .expect("type-param Add bounds with the wrong rhs should not match")
        .is_none());

    let add_point_impl = program
        .trait_impls
        .iter()
        .find(|trait_impl| {
            trait_impl.trait_name == "Add" && trait_impl.for_type == Type::named("Point")
        })
        .expect("Point Add impl should be present")
        .clone();

    let mut missing_impl_method = add_point_impl.clone();
    missing_impl_method.methods.clear();
    let missing_impl_methods = vec![missing_impl_method];
    let missing_impl_checker = checker(
        &program.module_name,
        &type_names,
        &type_arities,
        &program.classes,
        &program.enums,
        &program.functions,
        &program.traits,
        &missing_impl_methods,
        &program.imported_modules,
        &program.module_registry,
    );
    assert!(missing_impl_checker
        .operator_method_for_concrete_type(
            span,
            &Type::named("Point"),
            "Add",
            "add",
            Some(&Type::named("Point")),
        )
        .expect("impls missing the operator method should be skipped")
        .is_none());

    let mut wrong_rhs_impl = add_point_impl.clone();
    wrong_rhs_impl.trait_args = vec![Type::named("str"), Type::named("Point")];
    let wrong_rhs_impls = vec![wrong_rhs_impl];
    let wrong_rhs_impl_checker = checker(
        &program.module_name,
        &type_names,
        &type_arities,
        &program.classes,
        &program.enums,
        &program.functions,
        &program.traits,
        &wrong_rhs_impls,
        &program.imported_modules,
        &program.module_registry,
    );
    assert!(wrong_rhs_impl_checker
        .operator_method_for_concrete_type(
            span,
            &Type::named("Point"),
            "Add",
            "add",
            Some(&Type::named("Point")),
        )
        .expect("impls with mismatched rhs patterns should be skipped")
        .is_none());
    assert!(base_checker
        .operator_method_for_concrete_type(span, &Type::named("Point"), "Add", "add", None)
        .expect("binary traits should not match unary lookup shapes")
        .is_none());

    let mut unbound_generic_impl = add_point_impl;
    unbound_generic_impl.type_param_bounds = BTreeMap::from([(
        "T".to_string(),
        vec![TraitBound {
            trait_name: "Named".to_string(),
            trait_args: Vec::new(),
        }],
    )]);
    let unbound_generic_impls = vec![unbound_generic_impl];
    let unbound_generic_checker = checker(
        &program.module_name,
        &type_names,
        &type_arities,
        &program.classes,
        &program.enums,
        &program.functions,
        &program.traits,
        &unbound_generic_impls,
        &program.imported_modules,
        &program.module_registry,
    );
    assert!(unbound_generic_checker
        .operator_method_for_concrete_type(
            span,
            &Type::named("Point"),
            "Add",
            "add",
            Some(&Type::named("Point")),
        )
        .expect("impl bounds for unbound type params should invalidate the impl")
        .is_none());
}

#[test]
fn concrete_operator_trait_resolution_reports_ambiguity_for_equal_specificity_impls() {
    let error = crate::check_source(
        "\
trait Add[Rhs, Out]:
    def add(self, rhs: Rhs) -> Out

class Pair[A, B]:
    left: A
    right: B

impl[T] Add[Pair[int32, T], Pair[int32, T]] for Pair[int32, T]:
    def add(self, rhs: Pair[int32, T]) -> Pair[int32, T]:
        return Pair(left=self.left + rhs.left, right=rhs.right)

impl[T] Add[Pair[T, int32], Pair[T, int32]] for Pair[T, int32]:
    def add(self, rhs: Pair[T, int32]) -> Pair[T, int32]:
        return Pair(left=rhs.left, right=self.right + rhs.right)

def main() -> int32:
    left: Pair[int32, int32] = Pair(left=1, right=2)
    right: Pair[int32, int32] = Pair(left=3, right=4)
    total = left + right
    return total.left
",
    )
    .expect_err("equally specific concrete operator impls should be ambiguous");
    assert!(error
        .message
        .contains("operator trait `Add` is ambiguous for type `Pair[int32, int32]`"));
}

#[test]
fn operator_method_from_type_param_reports_ambiguity_when_multiple_bounds_match() {
    let mut add_trait = trait_info("Add", vec!["Rhs", "Out"]);
    let mut add_decl = function_decl("add");
    add_decl.receiver = Some(ReceiverKind::Borrow);
    add_decl.params = vec![Param {
        name: "rhs".to_string(),
        mode: ParamMode::Default,
        ty: type_ref("Rhs"),
        default: None,
        span: Span::new(1, 1),
    }];
    add_decl.return_type = type_ref("Out");
    add_trait.methods.insert(
        "add".to_string(),
        TraitMethodInfo {
            decl: add_decl,
            signature: function_signature(
                vec![Type::TypeParam("Rhs".to_string())],
                Type::TypeParam("Out".to_string()),
            ),
            type_param_bounds: BTreeMap::new(),
        },
    );

    let type_names = BTreeMap::from([("Add".to_string(), Span::new(1, 1))]);
    let type_arities = BTreeMap::from([("Add".to_string(), 2usize)]);
    let classes = BTreeMap::new();
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::from([("Add".to_string(), add_trait)]);
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    )
    .with_type_params(
        BTreeMap::from([("T".to_string(), ())]),
        BTreeMap::from([(
            "T".to_string(),
            vec![
                TraitBound {
                    trait_name: "Add".to_string(),
                    trait_args: vec![Type::named("int32"), Type::named("str")],
                },
                TraitBound {
                    trait_name: "Add".to_string(),
                    trait_args: vec![Type::named("int32"), Type::named("bool")],
                },
            ],
        )]),
    );

    let error = match checker.operator_method_from_type_param(
        "T",
        "Add",
        "add",
        Some(&Type::named("int32")),
    ) {
        Ok(_) => panic!("multiple matching Add bounds should be ambiguous"),
        Err(error) => error,
    };
    assert!(error
        .message
        .contains("operator trait `Add` is ambiguous for type parameter `T`"));
}

#[test]
fn module_namespace_and_builtin_enum_helpers_cover_resolution_paths() {
    let span = Span::new(1, 1);
    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();
    let classes = BTreeMap::new();
    let enums = BTreeMap::new();
    let functions = BTreeMap::from([(
        "work".to_string(),
        FunctionInfo {
            module_name: "pkg.tools".to_string(),
            decl: function_decl("work"),
            signature: function_signature(Vec::new(), Type::Unit),
            type_param_bounds: BTreeMap::new(),
        },
    )]);
    let traits = BTreeMap::new();

    let mut tools = namespace("pkg.tools");
    tools.functions.insert(
        "work".to_string(),
        FunctionInfo {
            module_name: "pkg.tools".to_string(),
            decl: function_decl("work"),
            signature: function_signature(Vec::new(), Type::Unit),
            type_param_bounds: BTreeMap::new(),
        },
    );
    tools.all_functions = tools.functions.clone();
    tools.classes.insert(
        "Widget".to_string(),
        class_info(
            "Widget",
            false,
            vec![("value", Type::named("int32"), false)],
        ),
    );
    tools.classes.get_mut("Widget").unwrap().methods.insert(
        "build".to_string(),
        MethodInfo {
            decl: {
                let mut decl = function_decl("build");
                decl.return_type = type_ref("int32");
                decl
            },
            signature: function_signature(Vec::new(), Type::named("int32")),
            type_param_bounds: BTreeMap::new(),
        },
    );
    tools.all_classes = tools.classes.clone();
    tools.enums.insert(
        "Status".to_string(),
        enum_info("Status", Some(Type::named("int32"))),
    );
    tools.all_enums = tools.enums.clone();
    tools
        .modules
        .insert("inner".to_string(), namespace("pkg.tools.inner"));

    let mut pkg = namespace("pkg");
    pkg.modules.insert("tools".to_string(), tools.clone());

    let imported_modules = BTreeMap::from([("pkg".to_string(), pkg.clone())]);
    let module_registry = BTreeMap::from([
        ("pkg".to_string(), pkg),
        ("pkg.tools".to_string(), tools.clone()),
    ]);

    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let mut locals = HashMap::from([(
        "tools_module".to_string(),
        local_binding(
            Type::Module("pkg.tools".to_string()),
            false,
            false,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    let tools_expr = expr(ExprKind::Name("tools_module".to_string()));
    let module_function = expr(ExprKind::Member {
        object: Box::new(tools_expr.clone()),
        field: "work".to_string(),
    });
    let module_widget = expr(ExprKind::Member {
        object: Box::new(tools_expr.clone()),
        field: "Widget".to_string(),
    });

    assert_eq!(
        checker
            .type_of_call(&module_function, &[], span, &mut locals, None)
            .expect("module function calls should resolve"),
        Type::Unit
    );
    assert_eq!(
        checker
            .type_of_call(
                &module_widget,
                &[named_arg("value", expr(ExprKind::Int(1)))],
                span,
                &mut locals,
                None,
            )
            .expect("module class constructors should resolve"),
        Type::named("pkg.tools.Widget")
    );
    assert_eq!(
        checker
            .type_of_call(
                &module_widget,
                &[arg(expr(ExprKind::Int(1)))],
                span,
                &mut locals,
                None,
            )
            .expect("module constructors should now accept positional arguments"),
        Type::named("pkg.tools.Widget")
    );
    assert!(checker
        .type_of_call(
            &module_widget,
            &[named_arg("missing", expr(ExprKind::Int(1)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("module constructors should reject unknown fields")
        .message
        .contains("has no field named `missing`"));
    assert!(checker
        .type_of_call(
            &module_widget,
            &[
                named_arg("value", expr(ExprKind::Int(1))),
                named_arg("value", expr(ExprKind::Int(2))),
            ],
            span,
            &mut locals,
            None,
        )
        .expect_err("module constructors should reject duplicate fields")
        .message
        .contains("provided more than once"));
    assert!(checker
        .type_of_call(
            &module_widget,
            &[named_arg("value", expr(ExprKind::Bool(true)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("module constructors should enforce field types")
        .message
        .contains("field `value` expects `int32`, found `bool`"));
    assert!(checker
        .type_of_call(&module_widget, &[], span, &mut locals, None)
        .expect_err("module constructors should require all required fields")
        .message
        .contains("missing required field `value`"));
    assert!(checker
        .type_of_call(
            &expr(ExprKind::Member {
                object: Box::new(tools_expr.clone()),
                field: "missing".to_string(),
            }),
            &[],
            span,
            &mut locals,
            None,
        )
        .expect_err("unknown module callable members should fail")
        .message
        .contains("module `pkg.tools` has no callable member `missing`"));

    assert_eq!(
        checker
            .module_namespace("pkg.tools")
            .map(|ns| ns.path.as_str()),
        Some("pkg.tools")
    );
    assert_eq!(
        checker
            .resolve_member_type(&Type::Module("pkg".to_string()), "tools", span)
            .expect("child module should resolve"),
        Type::Module("pkg.tools".to_string())
    );

    let fn_error = checker
        .resolve_member_type(&Type::Module("pkg.tools".to_string()), "work", span)
        .expect_err("module functions should require call syntax");
    assert!(fn_error.message.contains("must be called with `(...)`"));

    let class_error = checker
        .resolve_member_type(&Type::Module("pkg.tools".to_string()), "Widget", span)
        .expect_err("module classes should require constructor syntax");
    assert!(class_error
        .message
        .contains("must be constructed with `(...)`"));

    assert_eq!(
        checker
            .resolve_member_type(&Type::Module("pkg.tools".to_string()), "Status", span)
            .expect("module enums should resolve to enum types for qualified variant access"),
        Type::Named("pkg.tools.Status".to_string(), Vec::new())
    );

    let missing_error = checker
        .resolve_member_type(&Type::Module("pkg.tools".to_string()), "missing", span)
        .expect_err("missing module members should fail");
    assert!(missing_error.message.contains("has no member `missing`"));

    assert_eq!(
        checker.builtin_enum_variant_payload(
            &Type::Named("Option".to_string(), vec![Type::named("int32")]),
            "Option",
            "Some",
        ),
        Some(vec![Type::named("int32")])
    );
    assert_eq!(
        checker.builtin_enum_variant_payload(
            &Type::Named("Option".to_string(), vec![Type::named("int32")]),
            "Option",
            "None",
        ),
        Some(Vec::new())
    );
    let string_ty = Type::named("str");
    let int_ty = Type::named("int32");
    let index_ty = Type::named("int64");
    let builtin_payload_cases = [
        (
            Type::Named(
                "Result".to_string(),
                vec![int_ty.clone(), string_ty.clone()],
            ),
            "Result",
            "Ok",
            vec![int_ty.clone()],
        ),
        (
            Type::Named(
                "Result".to_string(),
                vec![int_ty.clone(), string_ty.clone()],
            ),
            "Result",
            "Err",
            vec![string_ty.clone()],
        ),
        (
            Type::Named("SendError".to_string(), vec![int_ty.clone()]),
            "SendError",
            "Closed",
            vec![int_ty.clone()],
        ),
        (
            Type::Named("QueueReceive".to_string(), vec![int_ty.clone()]),
            "QueueReceive",
            "Item",
            vec![int_ty.clone()],
        ),
        (
            Type::Named("QueueReceive".to_string(), vec![int_ty.clone()]),
            "QueueReceive",
            "TimedOut",
            Vec::new(),
        ),
        (
            Type::Named("TaskResult".to_string(), vec![int_ty.clone()]),
            "TaskResult",
            "Ready",
            vec![int_ty.clone()],
        ),
        (
            Type::Named("TaskResult".to_string(), vec![int_ty.clone()]),
            "TaskResult",
            "Error",
            vec![string_ty.clone()],
        ),
        (
            Type::Named("TaskResult".to_string(), vec![int_ty.clone()]),
            "TaskResult",
            "Cancelled",
            Vec::new(),
        ),
        (
            Type::Named("WaitAny".to_string(), vec![int_ty.clone()]),
            "WaitAny",
            "Ready",
            vec![index_ty.clone(), int_ty.clone()],
        ),
        (
            Type::Named("WaitAny".to_string(), vec![int_ty.clone()]),
            "WaitAny",
            "Error",
            vec![index_ty.clone(), string_ty.clone()],
        ),
        (
            Type::Named("WaitAny".to_string(), vec![int_ty.clone()]),
            "WaitAny",
            "TimedOut",
            Vec::new(),
        ),
        (
            Type::Named(
                "WaitAll".to_string(),
                vec![Type::Named("list".to_string(), vec![int_ty.clone()])],
            ),
            "WaitAll",
            "Ready",
            vec![Type::Named("list".to_string(), vec![int_ty.clone()])],
        ),
        (
            Type::Named("WaitAll".to_string(), vec![int_ty.clone()]),
            "WaitAll",
            "Error",
            vec![index_ty.clone(), string_ty.clone()],
        ),
        (
            Type::Named("WaitAll".to_string(), vec![int_ty.clone()]),
            "WaitAll",
            "Cancelled",
            Vec::new(),
        ),
        (
            Type::Named(
                "SelectOutcome".to_string(),
                vec![string_ty.clone(), int_ty.clone()],
            ),
            "SelectOutcome",
            "Queue",
            vec![
                index_ty.clone(),
                Type::Named("QueueReceive".to_string(), vec![string_ty.clone()]),
            ],
        ),
        (
            Type::Named(
                "SelectOutcome".to_string(),
                vec![string_ty.clone(), int_ty.clone()],
            ),
            "SelectOutcome",
            "Task",
            vec![
                index_ty.clone(),
                Type::Named("TaskResult".to_string(), vec![int_ty.clone()]),
            ],
        ),
        (
            Type::Named(
                "SelectOutcome".to_string(),
                vec![string_ty.clone(), int_ty.clone()],
            ),
            "SelectOutcome",
            "Deadline",
            vec![index_ty.clone()],
        ),
        (
            Type::Named(
                "SelectOutcome".to_string(),
                vec![string_ty.clone(), int_ty.clone()],
            ),
            "SelectOutcome",
            "Cancelled",
            Vec::new(),
        ),
    ];
    for (expected, enum_name, variant_name, payload) in builtin_payload_cases {
        assert_eq!(
            checker.builtin_enum_variant_payload(&expected, enum_name, variant_name),
            Some(payload),
            "{enum_name}.{variant_name}"
        );
    }
    assert_eq!(
        checker.builtin_enum_variant_payload(&Type::Unit, "Option", "Some"),
        None
    );
    assert_eq!(
        checker.builtin_enum_variant_payload(
            &Type::Named("Option".to_string(), vec![int_ty.clone()]),
            "Result",
            "Ok",
        ),
        None
    );
    assert_eq!(
        checker
            .explicit_builtin_type("Result", &[Type::named("int32"), Type::named("str")], span,)
            .expect("built-in enum specialization should succeed"),
        Type::Named(
            "Result".to_string(),
            vec![Type::named("int32"), Type::named("str")]
        )
    );
    for (name, args) in [
        ("SendError", vec![int_ty.clone()]),
        ("QueueReceive", vec![int_ty.clone()]),
        ("TaskResult", vec![int_ty.clone()]),
        ("WaitAny", vec![int_ty.clone()]),
        ("WaitAll", vec![int_ty.clone()]),
        ("SelectOutcome", vec![string_ty.clone(), int_ty.clone()]),
    ] {
        assert_eq!(
            checker
                .explicit_builtin_type(name, &args, span)
                .expect("builtin enum specialization should accept the maintained arity"),
            Type::Named(name.to_string(), args),
            "{name}"
        );
    }
    let builtin_arity = checker
        .explicit_builtin_type("Option", &[], span)
        .expect_err("wrong explicit type arg arity should fail");
    assert!(builtin_arity.message.contains("expects 1 type argument"));
    let builtin_missing = checker
        .explicit_builtin_type("Missing", &[], span)
        .expect_err("unknown builtin enum should fail");
    assert!(builtin_missing.message.contains("unknown name `Missing`"));

    let builtin_ctor = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Specialize {
            expr: Box::new(expr(ExprKind::Name("Option".to_string()))),
            type_args: vec![type_ref("int32")],
        })),
        field: "Some".to_string(),
    });
    assert!(checker.expr_can_use_partial_expected_hint(&builtin_ctor));
    assert!(checker.is_builtin_enum_constructor_expr(&expr(ExprKind::Name("Option".to_string()))));

    let mut locals = HashMap::new();
    assert_eq!(
        checker
            .type_check_builtin_enum_variant_constructor(
                "Option",
                "Some",
                &Type::Named("Option".to_string(), vec![Type::named("int32")]),
                &[arg(expr(ExprKind::Int(7)))],
                span,
                &mut locals,
            )
            .expect("builtin Option.Some constructor should type check"),
        Type::Named("Option".to_string(), vec![Type::named("int32")])
    );
    let none_payload = checker
        .type_check_builtin_enum_variant_constructor(
            "Option",
            "None",
            &Type::Named("Option".to_string(), vec![Type::named("int32")]),
            &[arg(expr(ExprKind::Int(7)))],
            span,
            &mut locals,
        )
        .expect_err("Option.None should reject payloads");
    assert!(none_payload.message.contains("does not take a payload"));

    let mut locals = HashMap::from([(
        "pkg".to_string(),
        local_binding(
            Type::Module("pkg".to_string()),
            false,
            false,
            ReceiverKind::Value,
            false,
            &[],
        ),
    )]);
    let qualified_widget_build = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Member {
            object: Box::new(expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pkg".to_string()))),
                field: "tools".to_string(),
            })),
            field: "Widget".to_string(),
        })),
        field: "build".to_string(),
    });
    assert_eq!(
        checker
            .type_of_call(&qualified_widget_build, &[], span, &mut locals, None)
            .expect("qualified module class associated methods should type check"),
        Type::named("int32")
    );

    let qualified_status_value = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Member {
            object: Box::new(expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pkg".to_string()))),
                field: "tools".to_string(),
            })),
            field: "Status".to_string(),
        })),
        field: "Value".to_string(),
    });
    let qualified_status_missing = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Member {
            object: Box::new(expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pkg".to_string()))),
                field: "tools".to_string(),
            })),
            field: "Status".to_string(),
        })),
        field: "Missing".to_string(),
    });
    let qualified_missing_enum_value = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Member {
            object: Box::new(expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("pkg".to_string()))),
                field: "tools".to_string(),
            })),
            field: "NotAnEnum".to_string(),
        })),
        field: "Value".to_string(),
    });

    let missing_variant_expr = checker
        .type_of_expr(&qualified_status_missing, &mut locals)
        .expect_err("missing qualified variants should fail as expressions");
    assert!(missing_variant_expr
        .message
        .contains("enum `Status` has no variant `Missing`"));

    let missing_enum_expr = checker
        .type_of_expr(&qualified_missing_enum_value, &mut locals)
        .expect_err("qualified non-enum members should fall through to module member errors");
    assert!(missing_enum_expr
        .message
        .contains("module `pkg.tools` has no member `NotAnEnum`"));

    let missing_variant = checker
        .type_of_call(&qualified_status_missing, &[], span, &mut locals, None)
        .expect_err("missing qualified variants should fail");
    assert!(missing_variant
        .message
        .contains("enum `Status` has no variant `Missing`"));

    assert_eq!(
        checker
            .type_of_call(
                &qualified_status_value,
                &[named_arg("value", expr(ExprKind::Int(1)))],
                span,
                &mut locals,
                None,
            )
            .expect("qualified enum constructors should accept `value=`"),
        Type::named("pkg.tools.Status")
    );

    let missing_payload = checker
        .type_of_call(&qualified_status_value, &[], span, &mut locals, None)
        .expect_err("qualified payload variants should require one argument");
    assert!(missing_payload.message.contains("payload"));

    let wrong_payload = checker
        .type_of_call(
            &qualified_status_value,
            &[arg(expr(ExprKind::Bool(true)))],
            span,
            &mut locals,
            None,
        )
        .expect_err("qualified payload variants should enforce payload types");
    assert!(wrong_payload
        .message
        .contains("variant `Value` of enum `Status` expects `int32`, found `bool`"));
}

#[test]
fn module_qualified_builtin_io_error_variants_type_check() {
    crate::check_source(
        "import io\n\ndef main() -> int32:\n    err: io.Error = io.Error.NotFound\n    return 0\n",
    )
    .expect("qualified builtin io.Error variants should type-check");
}

#[test]
fn checker_module_resolution_helpers_cover_current_module_and_index_wrappers() {
    let span = Span::new(1, 1);
    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();
    let classes = BTreeMap::new();
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();

    let mut math = namespace("helpers.math");
    math.functions.insert(
        "work".to_string(),
        FunctionInfo {
            module_name: "helpers.math".to_string(),
            decl: function_decl("work"),
            signature: function_signature(Vec::new(), Type::Unit),
            type_param_bounds: BTreeMap::new(),
        },
    );
    math.all_functions = math.functions.clone();
    let mut math_widget = class_info(
        "Widget",
        false,
        vec![("value", Type::named("int32"), false)],
    );
    math_widget.module_name = "helpers.math".to_string();
    let mut merge_decl = function_decl("merge");
    merge_decl.params = vec![
        Param {
            name: "left".to_string(),
            ty: type_ref("Widget"),
            mode: ParamMode::BorrowMut,
            default: None,
            span,
        },
        Param {
            name: "right".to_string(),
            ty: type_ref("Widget"),
            mode: ParamMode::BorrowMut,
            default: None,
            span,
        },
    ];
    math_widget.methods.insert(
        "merge".to_string(),
        MethodInfo {
            decl: merge_decl.clone(),
            signature: FunctionSignature {
                params: vec![Type::named("Widget"), Type::named("Widget")],
                param_passings: vec![ReceiverKind::BorrowMut, ReceiverKind::BorrowMut],
                return_type: Type::Unit,
                rng_clone_safe_type_params: BTreeSet::new(),
                array_equality_safe_type_params: BTreeSet::new(),
            },
            type_param_bounds: BTreeMap::new(),
        },
    );
    math.classes.insert("Widget".to_string(), math_widget);
    let mut math_secret = class_info(
        "SecretBox",
        false,
        vec![("secret", Type::named("int32"), false)],
    );
    math_secret.module_name = "helpers.math".to_string();
    math_secret.decl.fields[0].public = false;
    math_secret
        .fields
        .get_mut("secret")
        .expect("secret field should exist")
        .public = false;
    math.classes.insert("SecretBox".to_string(), math_secret);
    math.all_classes = math.classes.clone();
    let mut math_status = enum_info("Status", Some(Type::named("int32")));
    math_status.module_name = "helpers.math".to_string();
    math.enums.insert("Status".to_string(), math_status);
    math.all_enums = math.enums.clone();

    let mut other = namespace("helpers.other");
    let mut other_widget = class_info("Widget", false, vec![("label", Type::named("str"), false)]);
    other_widget.module_name = "helpers.other".to_string();
    other.classes.insert("Widget".to_string(), other_widget);
    other.all_classes = other.classes.clone();
    let mut other_status = enum_info("Status", Some(Type::named("bool")));
    other_status.module_name = "helpers.other".to_string();
    other.enums.insert("Status".to_string(), other_status);
    other.all_enums = other.enums.clone();

    math.imported_modules
        .insert("other".to_string(), other.clone());

    let mut helpers = namespace("helpers");
    helpers.modules.insert("math".to_string(), math.clone());
    helpers.modules.insert("other".to_string(), other.clone());

    let imported_modules = BTreeMap::from([("helpers".to_string(), helpers.clone())]);
    let module_registry = BTreeMap::from([
        ("helpers".to_string(), helpers),
        ("helpers.math".to_string(), math.clone()),
        ("helpers.other".to_string(), other.clone()),
    ]);

    let root_checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );
    let helpers_expr = expr(ExprKind::Name("helpers".to_string()));
    let math_expr = expr(ExprKind::Member {
        object: Box::new(helpers_expr.clone()),
        field: "math".to_string(),
    });
    let specialized_math = expr(ExprKind::Specialize {
        expr: Box::new(math_expr.clone()),
        type_args: vec![type_ref("int32")],
    });
    let widget_expr = expr(ExprKind::Member {
        object: Box::new(math_expr.clone()),
        field: "Widget".to_string(),
    });
    let secret_expr = expr(ExprKind::Member {
        object: Box::new(math_expr.clone()),
        field: "SecretBox".to_string(),
    });
    let qualified_status_value_expr = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Member {
            object: Box::new(math_expr.clone()),
            field: "Status".to_string(),
        })),
        field: "Value".to_string(),
    });
    let indexed_widget_expr = expr(ExprKind::Index {
        object: Box::new(widget_expr.clone()),
        index: Box::new(expr(ExprKind::Int(0))),
    });

    assert!(root_checker.current_module_namespace().is_none());
    assert_eq!(
        root_checker.infer_module_path(&helpers_expr),
        Some("helpers".to_string())
    );
    assert_eq!(
        root_checker.infer_module_path(&math_expr),
        Some("helpers.math".to_string())
    );
    assert_eq!(
        root_checker.infer_module_path(&specialized_math),
        Some("helpers.math".to_string())
    );
    assert_eq!(
        root_checker.infer_module_path(&expr(ExprKind::Group(Box::new(math_expr.clone())))),
        Some("helpers.math".to_string())
    );
    assert_eq!(
        root_checker.infer_module_path(&expr(ExprKind::Index {
            object: Box::new(math_expr.clone()),
            index: Box::new(expr(ExprKind::Int(0))),
        })),
        Some("helpers.math".to_string())
    );
    assert_eq!(
        root_checker.infer_module_path(&expr(ExprKind::Bool(true))),
        None
    );
    assert_eq!(
        root_checker.qualified_module_item(&indexed_widget_expr),
        Some(("helpers.math".to_string(), "Widget".to_string()))
    );
    assert!(root_checker.imported_class_info("Widget").is_none());
    assert!(root_checker.imported_enum_info("Status").is_none());
    assert!(root_checker.resolve_class_info("Widget").is_none());
    assert!(root_checker.resolve_enum_info("Status").is_none());
    assert_eq!(
        root_checker
            .resolve_class_info("helpers.math.Widget")
            .map(|info| info.module_name.as_str()),
        Some("helpers.math")
    );
    assert_eq!(
        root_checker
            .resolve_enum_info("helpers.math.Status")
            .map(|info| info.module_name.as_str()),
        Some("helpers.math")
    );
    assert!(root_checker
        .resolve_class_info("helpers.math.Missing")
        .is_none());
    assert!(root_checker
        .resolve_enum_info("helpers.math.Missing")
        .is_none());
    assert_eq!(
        root_checker.canonical_enum_name("helpers.math.Missing"),
        "Missing"
    );
    let mut root_locals = HashMap::from([
        (
            "helpers".to_string(),
            local_binding(
                Type::Module("helpers".to_string()),
                false,
                false,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "left".to_string(),
            local_binding(
                Type::named("Widget"),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "right".to_string(),
            local_binding(
                Type::named("Widget"),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
    ]);
    assert_eq!(
        root_checker
            .type_of_call(
                &widget_expr,
                &[arg(expr(ExprKind::Int(1)))],
                span,
                &mut root_locals,
                None,
            )
            .expect("module-qualified class constructors should type check"),
        Type::named("helpers.math.Widget")
    );
    assert!(root_checker
        .type_of_call(
            &widget_expr,
            &[arg(expr(ExprKind::Int(1))), arg(expr(ExprKind::Int(2)))],
            span,
            &mut root_locals,
            None,
        )
        .expect_err("module-qualified constructors should reject extra positional arguments")
        .message
        .contains("received too many positional arguments"));
    assert!(root_checker
        .type_of_call(
            &secret_expr,
            &[named_arg("secret", expr(ExprKind::Int(1)))],
            span,
            &mut root_locals,
            None,
        )
        .expect_err("external module constructors should reject private fields")
        .message
        .contains("field `secret` is private on `SecretBox`"));
    assert!(root_checker
        .type_of_call(&secret_expr, &[], span, &mut root_locals, None)
        .expect_err("external module constructors should not infer private fields")
        .message
        .contains("cannot initialize private field `secret` from another module"));
    assert!(root_checker
        .type_of_expr(&qualified_status_value_expr, &mut root_locals)
        .expect_err("module-qualified payload variants should require construction")
        .message
        .contains("requires a payload"));

    let merge_expr = expr(ExprKind::Member {
        object: Box::new(widget_expr.clone()),
        field: "merge".to_string(),
    });
    let mut borrowed_places = Vec::new();
    root_checker
        .collect_call_borrowed_places(
            &merge_expr,
            &[
                named_arg("left", expr(ExprKind::Name("left".to_string()))),
                named_arg("right", expr(ExprKind::Name("right".to_string()))),
            ],
            &root_locals,
            &mut borrowed_places,
            false,
        )
        .expect("module-qualified class methods should collect borrowed arguments");
    assert_eq!(borrowed_places.len(), 2);
    assert_eq!(borrowed_places[0].path, place_path("left"));
    assert_eq!(borrowed_places[1].path, place_path("right"));

    let module_checker = root_checker.with_module_name("helpers.math");
    assert_eq!(
        module_checker
            .current_module_namespace()
            .map(|namespace| namespace.path.as_str()),
        Some("helpers.math")
    );
    assert_eq!(
        module_checker.infer_module_path(&expr(ExprKind::Name("other".to_string()))),
        Some("helpers.other".to_string())
    );
    assert_eq!(
        module_checker
            .resolve_function_info("work")
            .map(|info| info.module_name.as_str()),
        Some("helpers.math")
    );
    assert_eq!(
        module_checker
            .resolve_class_info("Widget")
            .map(|info| info.module_name.as_str()),
        Some("helpers.math")
    );
    assert_eq!(
        module_checker
            .resolve_enum_info("Status")
            .map(|info| info.module_name.as_str()),
        Some("helpers.math")
    );

    let member_error = module_checker
        .resolve_member_type(&Type::Module("helpers".to_string()), "missing", span)
        .expect_err("missing module members should still fail");
    assert!(member_error.message.contains("has no member `missing`"));
}

#[test]
fn spawn_callable_resolution_covers_module_and_associated_targets() {
    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();
    let mut worker = class_info("Worker", false, vec![]);
    worker.methods.insert(
        "make".to_string(),
        MethodInfo {
            decl: function_decl("make"),
            signature: function_signature(Vec::new(), Type::named("int32")),
            type_param_bounds: BTreeMap::new(),
        },
    );
    let mut touch_decl = function_decl("touch");
    touch_decl.receiver = Some(ReceiverKind::Borrow);
    worker.methods.insert(
        "touch".to_string(),
        MethodInfo {
            decl: touch_decl,
            signature: function_signature(Vec::new(), Type::Unit),
            type_param_bounds: BTreeMap::new(),
        },
    );
    let classes = BTreeMap::from([("Worker".to_string(), worker)]);
    let enums = BTreeMap::new();
    let functions = BTreeMap::from([(
        "job".to_string(),
        FunctionInfo {
            module_name: "<main>".to_string(),
            decl: function_decl("job"),
            signature: function_signature(Vec::new(), Type::named("int32")),
            type_param_bounds: BTreeMap::new(),
        },
    )]);
    let traits = BTreeMap::new();

    let remote_job = FunctionInfo {
        module_name: "pkg.tools".to_string(),
        decl: function_decl("remote_job"),
        signature: function_signature(Vec::new(), Type::Unit),
        type_param_bounds: BTreeMap::new(),
    };
    let mut remote_worker = class_info("RemoteWorker", false, vec![]);
    remote_worker.module_name = "pkg.tools".to_string();
    remote_worker.methods.insert(
        "make".to_string(),
        MethodInfo {
            decl: function_decl("make"),
            signature: function_signature(Vec::new(), Type::named("bool")),
            type_param_bounds: BTreeMap::new(),
        },
    );
    let mut tools = namespace("pkg.tools");
    tools
        .all_functions
        .insert("remote_job".to_string(), remote_job);
    tools
        .classes
        .insert("RemoteWorker".to_string(), remote_worker);
    let mut pkg = namespace("pkg");
    pkg.modules.insert("tools".to_string(), tools.clone());
    let imported_modules = BTreeMap::from([("pkg".to_string(), pkg.clone())]);
    let module_registry =
        BTreeMap::from([("pkg".to_string(), pkg), ("pkg.tools".to_string(), tools)]);

    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );

    let local_job = checker
        .resolve_spawn_callable(&expr(ExprKind::Name("job".to_string())))
        .expect("named local functions should be task-start targets");
    assert_eq!(local_job.display_name, "job");
    assert_eq!(local_job.signature.return_type, Type::named("int32"));

    let local_static = checker
        .resolve_spawn_callable(&expr(ExprKind::Member {
            object: Box::new(expr(ExprKind::Name("Worker".to_string()))),
            field: "make".to_string(),
        }))
        .expect("static associated methods should be task-start targets");
    assert_eq!(local_static.display_name, "Worker.make");
    assert!(checker
        .resolve_spawn_callable(&expr(ExprKind::Member {
            object: Box::new(expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("Worker".to_string()))),
                type_args: Vec::new(),
            })),
            field: "make".to_string(),
        }))
        .is_ok());

    let pkg_tools = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Name("pkg".to_string()))),
        field: "tools".to_string(),
    });
    let remote_function = checker
        .resolve_spawn_callable(&expr(ExprKind::Member {
            object: Box::new(pkg_tools.clone()),
            field: "remote_job".to_string(),
        }))
        .expect("module-qualified functions should be task-start targets");
    assert_eq!(remote_function.display_name, "pkg.tools.remote_job");
    let missing_remote_function = match checker.resolve_spawn_callable(&expr(ExprKind::Member {
        object: Box::new(pkg_tools.clone()),
        field: "missing".to_string(),
    })) {
        Ok(_) => panic!("missing module-qualified functions should not be task-start targets"),
        Err(error) => error,
    };
    assert!(missing_remote_function
        .message
        .contains("task starting currently supports named functions"));

    let remote_static = checker
        .resolve_spawn_callable(&expr(ExprKind::Member {
            object: Box::new(expr(ExprKind::Member {
                object: Box::new(pkg_tools),
                field: "RemoteWorker".to_string(),
            })),
            field: "make".to_string(),
        }))
        .expect("module-qualified static methods should be task-start targets");
    assert_eq!(remote_static.display_name, "RemoteWorker.make");
    assert_eq!(remote_static.signature.return_type, Type::named("bool"));

    let receiver_method = match checker.resolve_spawn_callable(&expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Name("Worker".to_string()))),
        field: "touch".to_string(),
    })) {
        Ok(_) => panic!("receiver methods should not be task-start targets"),
        Err(error) => error,
    };
    assert!(receiver_method
        .message
        .contains("task starting currently supports named functions"));
    let missing_name =
        match checker.resolve_spawn_callable(&expr(ExprKind::Name("missing".to_string()))) {
            Ok(_) => panic!("unknown names should not be task-start targets"),
            Err(error) => error,
        };
    assert!(missing_name
        .message
        .contains("task start target must be a callable function"));
    let non_callable = match checker.resolve_spawn_callable(&expr(ExprKind::Int(1))) {
        Ok(_) => panic!("non-call expressions should not be task-start targets"),
        Err(error) => error,
    };
    assert!(non_callable
        .message
        .contains("task starting currently supports named functions"));
    assert!(checker
        .resolve_spawn_callable(&expr(ExprKind::Specialize {
            expr: Box::new(expr(ExprKind::Name("job".to_string()))),
            type_args: Vec::new(),
        }))
        .is_ok());
}

#[test]
fn place_path_and_resource_helpers_cover_remaining_checker_paths() {
    let span = Span::new(1, 1);
    let type_names = BTreeMap::new();
    let type_arities = BTreeMap::new();

    let mut resource = class_info(
        "Resource",
        false,
        vec![("value", Type::named("int32"), false)],
    );
    let mut close_decl = function_decl("close");
    close_decl.receiver = Some(ReceiverKind::BorrowMut);
    let good_close = MethodInfo {
        decl: close_decl.clone(),
        signature: function_signature(Vec::new(), Type::Unit),
        type_param_bounds: BTreeMap::new(),
    };
    resource.methods.insert("close".to_string(), good_close);

    let mut bad_resource = class_info("BadResource", false, vec![]);
    let mut bad_close_decl = function_decl("close");
    bad_close_decl.receiver = Some(ReceiverKind::Borrow);
    bad_resource.methods.insert(
        "close".to_string(),
        MethodInfo {
            decl: bad_close_decl.clone(),
            signature: function_signature(Vec::new(), Type::named("int32")),
            type_param_bounds: BTreeMap::new(),
        },
    );

    let classes = BTreeMap::from([
        (
            "Counter".to_string(),
            class_info(
                "Counter",
                false,
                vec![("value", Type::named("int32"), false)],
            ),
        ),
        ("Resource".to_string(), resource),
        ("BadResource".to_string(), bad_resource),
    ]);
    let enums = BTreeMap::new();
    let functions = BTreeMap::new();
    let traits = BTreeMap::new();
    let imported_modules = BTreeMap::new();
    let module_registry = BTreeMap::new();
    let checker = checker(
        "<main>",
        &type_names,
        &type_arities,
        &classes,
        &enums,
        &functions,
        &traits,
        &[],
        &imported_modules,
        &module_registry,
    );

    let mut locals = HashMap::from([
        (
            "counter".to_string(),
            LocalBinding {
                ty: Type::named("Counter"),
                assignable: true,
                mutable_place: true,
                managed_resource: false,
                passing: ReceiverKind::BorrowMut,
                borrow_origin: Some("counter".to_string()),
                borrowed_at: None,
                match_borrow_place: None,
                stale_match_borrow_place: None,
                shared_match_scrutinee: None,
                moved: false,
                moved_at: None,
                moved_fields: BTreeMap::from([
                    (projection_path("other"), Span::new(1, 1)),
                    (projection_path("value.inner"), Span::new(1, 1)),
                ]),
                frozen_places: BTreeMap::new(),
                shared_match_places: BTreeMap::new(),
                captured: false,
                view: None,
                closure_loans: Vec::new(),
            },
        ),
        (
            "items".to_string(),
            local_binding(
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "moved".to_string(),
            local_binding(
                Type::named("Counter"),
                true,
                true,
                ReceiverKind::Value,
                true,
                &[],
            ),
        ),
        (
            "borrowed".to_string(),
            LocalBinding {
                ty: Type::named("Counter"),
                assignable: false,
                mutable_place: false,
                managed_resource: false,
                passing: ReceiverKind::Borrow,
                borrow_origin: Some("borrowed".to_string()),
                borrowed_at: None,
                match_borrow_place: None,
                stale_match_borrow_place: None,
                shared_match_scrutinee: None,
                moved: false,
                moved_at: None,
                moved_fields: BTreeMap::new(),
                frozen_places: BTreeMap::new(),
                shared_match_places: BTreeMap::new(),
                captured: false,
                view: None,
                closure_loans: Vec::new(),
            },
        ),
        (
            "self".to_string(),
            LocalBinding {
                ty: Type::named("Counter"),
                assignable: false,
                mutable_place: false,
                managed_resource: false,
                passing: ReceiverKind::Borrow,
                borrow_origin: Some("self".to_string()),
                borrowed_at: None,
                match_borrow_place: None,
                stale_match_borrow_place: None,
                shared_match_scrutinee: None,
                moved: false,
                moved_at: None,
                moved_fields: BTreeMap::new(),
                frozen_places: BTreeMap::new(),
                shared_match_places: BTreeMap::new(),
                captured: false,
                view: None,
                closure_loans: Vec::new(),
            },
        ),
    ]);

    let member_expr = expr(ExprKind::Member {
        object: Box::new(expr(ExprKind::Name("counter".to_string()))),
        field: "value".to_string(),
    });
    assert!(checker
        .is_mutable_place(&member_expr, &mut locals)
        .expect("member place should resolve"));
    assert_eq!(
        checker.borrow_call_place(&member_expr),
        Some(place_path("counter.value"))
    );
    assert_eq!(
        checker.render_member_target(&expr(ExprKind::Name("counter".to_string())), "value"),
        "counter.value"
    );
    assert_eq!(
        checker.render_index_target(&expr(ExprKind::Name("counter".to_string()))),
        "counter[..]"
    );
    assert_eq!(
        checker.render_place_expr(&expr(ExprKind::Index {
            object: Box::new(expr(ExprKind::Name("counter".to_string()))),
            index: Box::new(expr(ExprKind::Int(0))),
        })),
        "counter[..]"
    );
    assert_eq!(
        checker.borrowed_root_binding_name(&member_expr, &locals),
        Some("counter".to_string())
    );
    assert_eq!(
        checker.borrowed_root_binding_name(&expr(ExprKind::Name("self".to_string())), &locals),
        Some("self".to_string())
    );
    assert_eq!(
        checker.member_access_path(&member_expr),
        Some(place_path("counter.value"))
    );
    assert_eq!(
        checker.member_target_path(&expr(ExprKind::Name("counter".to_string())), "value"),
        Some(place_path("counter.value"))
    );
    assert!(FunctionChecker::field_path_is_moved(
        locals.get("counter").unwrap(),
        &projection_path("value")
    ));
    let binding = locals.get_mut("counter").unwrap();
    FunctionChecker::clear_moved_field_path(binding, &projection_path("value"));
    assert!(!FunctionChecker::field_path_is_moved(
        binding,
        &projection_path("value")
    ));
    assert!(binding.moved_fields.contains_key(&projection_path("other")));

    assert_eq!(
        checker
            .type_of_member_object_expr(
                &expr(ExprKind::Group(Box::new(member_expr.clone()))),
                &mut locals
            )
            .expect("grouped member objects should resolve through the inner object"),
        Type::named("int32")
    );
    assert_eq!(
        checker
            .type_of_member_object_expr(
                &expr(ExprKind::Cast {
                    expr: Box::new(expr(ExprKind::Name("counter".to_string()))),
                    ty: type_ref("Counter"),
                }),
                &mut locals,
            )
            .expect("cast member objects should resolve through the inner object"),
        Type::named("Counter")
    );
    assert_eq!(
        checker
            .type_of_member_object_expr(
                &expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name("counter".to_string()))),
                    type_args: Vec::new(),
                }),
                &mut locals,
            )
            .expect("specialized member objects should resolve through the inner object"),
        Type::named("Counter")
    );
    assert_eq!(
        checker
            .type_of_member_object_expr(
                &expr(ExprKind::Index {
                    object: Box::new(expr(ExprKind::Name("items".to_string()))),
                    index: Box::new(expr(ExprKind::Int(0))),
                }),
                &mut locals,
            )
            .expect("indexed member objects should resolve through expression typing"),
        Type::named("int32")
    );
    assert_eq!(
        checker
            .type_of_member_object_expr(&expr(ExprKind::Bool(true)), &mut locals)
            .expect("fallback member objects should resolve through expression typing"),
        Type::named("bool")
    );
    let missing_object = checker
        .type_of_member_object_expr(&expr(ExprKind::Name("missing".to_string())), &mut locals)
        .expect_err("missing member objects should report unknown names");
    assert!(missing_object.message.contains("unknown name `missing`"));
    let moved_object = checker
        .type_of_member_object_expr(&expr(ExprKind::Name("moved".to_string())), &mut locals)
        .expect_err("moved member objects should report moved values");
    assert!(moved_object.message.contains("use of moved value `moved`"));

    let borrowed_receiver = checker
        .prepare_method_receiver_borrows(
            "touch",
            Some(ReceiverKind::Borrow),
            &member_expr,
            span,
            &mut locals,
        )
        .expect("borrowed receiver should resolve");
    assert_eq!(borrowed_receiver.len(), 1);

    let mut immutable_locals = HashMap::from([(
        "counter".to_string(),
        LocalBinding {
            ty: Type::named("Counter"),
            assignable: true,
            mutable_place: false,
            managed_resource: false,
            passing: ReceiverKind::Value,
            borrow_origin: None,
            borrowed_at: None,
            match_borrow_place: None,
            stale_match_borrow_place: None,
            shared_match_scrutinee: None,
            moved: false,
            moved_at: None,
            moved_fields: BTreeMap::new(),
            frozen_places: BTreeMap::new(),
            shared_match_places: BTreeMap::new(),
            captured: false,
            view: None,
            closure_loans: Vec::new(),
        },
    )]);
    let receiver_error = match checker.prepare_method_receiver_borrows(
        "touch",
        Some(ReceiverKind::BorrowMut),
        &expr(ExprKind::Name("counter".to_string())),
        span,
        &mut immutable_locals,
    ) {
        Ok(_) => panic!("mutable receiver should require mutable places"),
        Err(error) => error,
    };
    assert!(receiver_error
        .message
        .contains("requires a mutable receiver"));

    checker
        .require_with_resource(&Type::named("TaskGroup"), span)
        .expect("TaskGroup should be allowed in with");
    checker
        .require_with_resource(&Type::named("Resource"), span)
        .expect("resource with correct close should pass");
    let generic_with = checker
        .require_with_resource(
            &Type::Named("Box".to_string(), vec![Type::named("int32")]),
            span,
        )
        .expect_err("generic resources should be rejected");
    assert!(generic_with
        .message
        .contains("does not yet support generic resource types"));
    let bad_with = checker
        .require_with_resource(&Type::named("BadResource"), span)
        .expect_err("bad close signature should fail");
    assert!(bad_with.message.contains("close(mut self)"));
    let primitive_with = checker
        .require_with_resource(&Type::named("int32"), span)
        .expect_err("primitive values cannot be with resources");
    assert!(primitive_with.message.contains("requires a class resource"));
    let unit_with = checker
        .require_with_resource(&Type::Unit, span)
        .expect_err("unit values cannot be with resources");
    assert!(unit_with.message.contains("requires a class resource"));

    checker
        .require_task_startable_function("work", &[], &[], span)
        .expect("by-value params should be task-startable");
    checker
        .require_task_startable_function(
            "work",
            &[Param {
                name: "value".to_string(),
                ty: type_ref("str"),
                mode: ParamMode::Default,
                default: None,
                span,
            }],
            &[ReceiverKind::Borrow],
            span,
        )
        .expect("shared borrowed parameters should be task-startable");
    let task_start_error = checker
        .require_task_startable_function(
            "work",
            &[Param {
                name: "value".to_string(),
                ty: type_ref("int32"),
                mode: ParamMode::BorrowMut,
                default: None,
                span,
            }],
            &[ReceiverKind::BorrowMut],
            span,
        )
        .expect_err("mutable borrowed params should not be task-startable");
    assert_eq!(task_start_error.code, "AU3002");
    assert_eq!(
        task_start_error.message,
        "task starting does not support mutable parameter `value` on function `work`; child tasks cannot write back through the starting call frame"
    );

    let consumed_then_borrowed = checker
        .reject_overlapping_borrow(
            &[BorrowedCallPlace {
                path: place_path("counter"),
                passing: ReceiverKind::Value,
                param_name: "owned".to_string(),
                origin_span: span,
            }],
            &place_path("counter"),
            ReceiverKind::Borrow,
            "borrowed",
            "function `use_counter`",
            span,
        )
        .expect_err("borrows should not overlap an already consumed argument");
    assert!(consumed_then_borrowed
        .message
        .contains("overlaps consumed argument"));

    let infer_missing = checker
        .type_check_callable_args(
            "function `make`",
            &["T".to_string()],
            &[],
            &[],
            &[],
            &Type::TypeParam("T".to_string()),
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[],
            span,
            &mut HashMap::new(),
            None,
            HashMap::new(),
        )
        .expect_err("generic functions without evidence should report missing inference");
    assert!(infer_missing
        .message
        .contains("cannot infer type parameter `T`"));

    let infer_unresolved = checker
        .type_check_callable_args_seeded(
            "function `make`",
            &["T".to_string()],
            &[],
            &[],
            &[],
            &Type::TypeParam("T".to_string()),
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[],
            span,
            &mut HashMap::new(),
            None,
            HashMap::from([("T".to_string(), Type::TypeParam("T".to_string()))]),
            Vec::new(),
            ClosureArgumentPolicy::Reject,
        )
        .expect_err("self-referential inferred type parameters should be rejected");
    assert!(infer_unresolved
        .message
        .contains("cannot infer type parameter `T`"));

    let payload_arity = checker
        .variant_payload_argument(
            &[
                named_arg("value", expr(ExprKind::Int(1))),
                named_arg("extra", expr(ExprKind::Int(2))),
            ],
            span,
            "Some",
            "Option",
        )
        .expect_err("single-payload helper should reject extra named arguments");
    assert!(payload_arity
        .message
        .contains("expects exactly one payload argument"));
}

#[test]
fn top_level_type_and_trait_helpers_cover_display_and_copy_paths() {
    let bound = TraitBound {
        trait_name: "Mapper".to_string(),
        trait_args: vec![Type::named("str"), Type::named("int32")],
    };
    assert_eq!(bound.to_string(), "Mapper[str, int32]");
    assert_eq!(
        TraitBound {
            trait_name: "Show".to_string(),
            trait_args: Vec::new(),
        }
        .to_string(),
        "Show"
    );

    assert_eq!(unary_operator_trait(UnaryOp::Neg), Some(("Neg", "neg")));
    assert_eq!(unary_operator_trait(UnaryOp::Not), Some(("Not", "not")));
    assert_eq!(binary_operator_trait(BinaryOp::Add), Some(("Add", "add")));
    assert_eq!(binary_operator_trait(BinaryOp::Div), Some(("Div", "div")));
    assert_eq!(binary_operator_trait(BinaryOp::Eq), None);
    assert_eq!(
        binary_operator_trait(BinaryOp::GreaterEq),
        Some(("Ord", "ge"))
    );

    assert_eq!(
        Type::named("int32"),
        Type::Named("int32".to_string(), Vec::new())
    );
    assert!(Type::Unit.is_copy());
    assert!(!Type::Module("pkg.tools".to_string()).is_copy());
    assert!(!Type::TypeParam("T".to_string()).is_copy());
    assert!(Type::named("float64").is_copy());
    assert!(!Type::named("str").is_copy());
    assert_eq!(Type::Unit.to_string(), "None");
    assert_eq!(
        Type::Module("pkg.tools".to_string()).to_string(),
        "module pkg.tools"
    );
    assert_eq!(Type::TypeParam("T".to_string()).to_string(), "T");
    assert_eq!(
        Type::Named("list".to_string(), vec![Type::named("int32")]).to_string(),
        "list[int32]"
    );

    let classes = BTreeMap::from([
        (
            "CopyBox".to_string(),
            class_info(
                "CopyBox",
                true,
                vec![("value", Type::named("int32"), false)],
            ),
        ),
        (
            "Thing".to_string(),
            class_info("Thing", false, vec![("name", Type::named("str"), false)]),
        ),
    ]);
    let enums = BTreeMap::from([
        (
            "MaybeInt".to_string(),
            enum_info("MaybeInt", Some(Type::named("int32"))),
        ),
        (
            "MaybeText".to_string(),
            enum_info("MaybeText", Some(Type::named("str"))),
        ),
    ]);
    assert!(type_is_copy_in_context(
        &Type::named("int32"),
        &classes,
        &enums
    ));
    assert!(type_is_copy_in_context(
        &Type::Named("Option".to_string(), vec![Type::named("int32")]),
        &classes,
        &enums
    ));
    assert!(type_is_copy_in_context(
        &Type::Named(
            "Result".to_string(),
            vec![Type::named("int32"), Type::named("bool")]
        ),
        &classes,
        &enums
    ));
    assert!(type_is_copy_in_context(
        &Type::Named("SendError".to_string(), vec![Type::named("int32")]),
        &classes,
        &enums
    ));
    assert!(type_is_copy_in_context(
        &Type::named("CopyBox"),
        &classes,
        &enums
    ));
    assert!(type_is_copy_in_context(
        &Type::named("MaybeInt"),
        &classes,
        &enums
    ));
    assert!(!type_is_copy_in_context(
        &Type::named("Thing"),
        &classes,
        &enums
    ));
    assert!(!type_is_copy_in_context(
        &Type::named("MaybeText"),
        &classes,
        &enums
    ));
    assert!(!type_is_copy_in_context(
        &Type::Named("Unknown".to_string(), vec![Type::named("int32")]),
        &classes,
        &enums
    ));
}

#[test]
fn check_with_context_covers_imported_binding_registration_and_duplicate_item_paths() {
    let span = Span::new(1, 1);
    let remote_function = FunctionInfo {
        module_name: "pkg.tools".to_string(),
        decl: function_decl("remote_fn"),
        signature: function_signature(vec![Type::named("int32")], Type::named("int32")),
        type_param_bounds: BTreeMap::new(),
    };
    let remote_class = class_info(
        "RemoteBox",
        false,
        vec![("value", Type::named("int32"), false)],
    );
    let remote_enum = enum_info("RemoteStatus", Some(Type::named("int32")));
    let remote_trait = trait_info("RemoteShow", Vec::new());
    let namespace = ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "tools".to_string(),
        path: "pkg.tools".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: BTreeMap::from([("remote_fn".to_string(), remote_function.clone())]),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::from([("RemoteBox".to_string(), remote_class.clone())]),
        enums: BTreeMap::from([("RemoteStatus".to_string(), remote_enum.clone())]),
        traits: BTreeMap::from([("RemoteShow".to_string(), remote_trait.clone())]),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::from([("remote_fn".to_string(), remote_function.clone())]),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::from([("RemoteBox".to_string(), remote_class.clone())]),
        all_enums: BTreeMap::from([("RemoteStatus".to_string(), remote_enum.clone())]),
        all_traits: BTreeMap::from([("RemoteShow".to_string(), remote_trait.clone())]),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    };
    let context = ModuleContext {
        module_name: "<main>".to_string(),
        imported_bindings: BTreeMap::from([
            (
                "remote_fn".to_string(),
                ImportedBinding::Function(remote_function.clone()),
            ),
            (
                "RemoteBox".to_string(),
                ImportedBinding::Class(remote_class.clone()),
            ),
            (
                "RemoteStatus".to_string(),
                ImportedBinding::Enum(remote_enum.clone()),
            ),
            (
                "RemoteShow".to_string(),
                ImportedBinding::Trait(remote_trait.clone()),
            ),
            (
                "tools".to_string(),
                ImportedBinding::Module(namespace.clone()),
            ),
        ]),
        module_registry: BTreeMap::from([("pkg.tools".to_string(), namespace.clone())]),
        is_entry_module: true,
    };
    let program = check_with_context(
        Module {
            constants: Vec::new(),
            imports: Vec::new(),
            items: vec![Item::Function(function_decl("main"))],
            top_level_stmts: Vec::new(),
        },
        context,
    )
    .expect("context-backed program should check");
    assert!(program.imported_modules.contains_key("tools"));
    assert!(program.module_registry.contains_key("pkg.tools"));
    assert!(program.functions.contains_key("main"));

    let duplicate_class = check_with_context(
        Module {
            constants: Vec::new(),
            imports: Vec::new(),
            items: vec![Item::Class(class_decl("RemoteBox", false, Vec::new()))],
            top_level_stmts: Vec::new(),
        },
        ModuleContext {
            module_name: "<main>".to_string(),
            imported_bindings: BTreeMap::from([(
                "RemoteBox".to_string(),
                ImportedBinding::Class(remote_class.clone()),
            )]),
            module_registry: BTreeMap::from([("pkg.tools".to_string(), namespace.clone())]),
            is_entry_module: true,
        },
    )
    .expect_err("duplicate imported class names should fail");
    assert!(duplicate_class
        .message
        .contains("duplicate item `RemoteBox`"));

    let duplicate_enum = check_with_context(
        Module {
            constants: Vec::new(),
            imports: Vec::new(),
            items: vec![Item::Enum(EnumDecl {
                public: true,
                name: "RemoteStatus".to_string(),
                type_params: Vec::new(),
                type_param_bounds: BTreeMap::new(),
                variants: Vec::new(),
                span,
            })],
            top_level_stmts: Vec::new(),
        },
        ModuleContext {
            module_name: "<main>".to_string(),
            imported_bindings: BTreeMap::from([(
                "RemoteStatus".to_string(),
                ImportedBinding::Enum(remote_enum),
            )]),
            module_registry: BTreeMap::from([("pkg.tools".to_string(), namespace.clone())]),
            is_entry_module: true,
        },
    )
    .expect_err("duplicate imported enum names should fail");
    assert!(duplicate_enum
        .message
        .contains("duplicate item `RemoteStatus`"));

    let duplicate_function = check_with_context(
        Module {
            constants: Vec::new(),
            imports: Vec::new(),
            items: vec![Item::Function(function_decl("remote_fn"))],
            top_level_stmts: Vec::new(),
        },
        ModuleContext {
            module_name: "<main>".to_string(),
            imported_bindings: BTreeMap::from([(
                "remote_fn".to_string(),
                ImportedBinding::Function(remote_function),
            )]),
            module_registry: BTreeMap::from([("pkg.tools".to_string(), namespace)]),
            is_entry_module: true,
        },
    )
    .expect_err("duplicate imported function names should fail");
    assert!(duplicate_function
        .message
        .contains("duplicate item `remote_fn`"));
}

#[test]
fn check_reports_field_default_and_trait_impl_validation_errors() {
    let default_mismatch = check(
        crate::parser::parse("class Box:\n    value: int32 = \"oops\"\n")
            .expect("default mismatch snippet should parse"),
    )
    .expect_err("mismatched defaults should fail");
    assert!(default_mismatch
        .message
        .contains("default value for field `value` has type `str`, expected `int32`"));

    let unknown_trait = check(
            crate::parser::parse(
                "class Box:\n    value: int32\n\nimpl Missing for Box:\n    def show(self) -> str:\n        return \"x\"\n",
            )
            .expect("unknown-trait snippet should parse"),
        )
        .expect_err("unknown traits should fail");
    assert!(unknown_trait.message.contains("unknown trait `Missing`"));

    let trait_arity = check(
            crate::parser::parse(
                "trait Mapper[T]:\n    def map(self, value: T) -> T\n\nclass Box:\n    value: int32\n\nimpl Mapper[int32, str] for Box:\n    def map(self, value: int32) -> int32:\n        return value\n",
            )
            .expect("trait-arity snippet should parse"),
        )
        .expect_err("trait arg arity mismatches should fail");
    assert!(trait_arity
        .message
        .contains("trait `Mapper` expects exactly 1 type argument"));

    let type_param_target = check(
            crate::parser::parse(
                "trait Show:\n    def show(self) -> str\n\nimpl[T] Show for T:\n    def show(self) -> str:\n        return \"x\"\n",
            )
            .expect("type-param target snippet should parse"),
        )
        .expect_err("plain type-parameter impl targets should fail");
    assert!(type_param_target
        .message
        .contains("trait impl target must name a concrete or generic outer type"));

    let duplicate_impl = check(
            crate::parser::parse(
                "trait Show:\n    def show(self) -> str\n\nclass Box:\n    value: int32\n\nimpl Show for Box:\n    def show(self) -> str:\n        return \"a\"\n\nimpl Show for Box:\n    def show(self) -> str:\n        return \"b\"\n",
            )
            .expect("duplicate-impl snippet should parse"),
        )
        .expect_err("duplicate impls should fail");
    assert!(duplicate_impl
        .message
        .contains("duplicate impl of trait `Show` for `Box`"));

    let unknown_method = check(
            crate::parser::parse(
                "trait Show:\n    def show(self) -> str\n\nclass Box:\n    value: int32\n\nimpl Show for Box:\n    def other(self) -> str:\n        return \"x\"\n",
            )
            .expect("unknown-method snippet should parse"),
        )
        .expect_err("impl methods outside the trait should fail");
    assert!(unknown_method
        .message
        .contains("method `other` is not part of trait `Show`"));

    let receiver_mismatch = check(
            crate::parser::parse(
                "trait Show:\n    def show(self) -> str\n\nclass Box:\n    value: int32\n\nimpl Show for Box:\n    def show(own self) -> str:\n        return \"x\"\n",
            )
            .expect("receiver-mismatch snippet should parse"),
        )
        .expect_err("receiver mismatches should fail");
    assert!(receiver_mismatch
        .message
        .contains("method `show` receiver does not match trait `Show`"));

    let signature_mismatch = check(
            crate::parser::parse(
                "trait Mapper[T]:\n    def map(self, value: T) -> T\n\nclass Box:\n    value: int32\n\nimpl Mapper[int32] for Box:\n    def map(self, value: str) -> str:\n        return value\n",
            )
            .expect("signature-mismatch snippet should parse"),
        )
        .expect_err("trait signature mismatches should fail");
    assert!(signature_mismatch
        .message
        .contains("method `map` in impl of `Mapper` does not match the trait signature"));

    for source in [
        "trait Show:\n    def show(self, text: str) -> int32\n\nclass Box:\n    value: int32\n\nimpl Show for Box:\n    def show(self, text: own str) -> int32:\n        return self.value\n",
    ] {
        let error = crate::check_source(source)
            .expect_err("a trait impl passing mismatch should fail");
        assert!(
            error
                .message
                .contains("does not match the trait signature"),
            "unexpected diagnostic: {}",
            error.message
        );
    }

    crate::check_source(
        "trait Identity:\n    def identity(self, value: own str) -> str\n\nclass Picker:\n    value: int32\n\nimpl Identity for Picker:\n    def identity(self, renamed: own str) -> str:\n        return renamed\n",
    )
    .expect("a trait impl may rename its parameters");

    let missing_method = check(
            crate::parser::parse(
                "trait Pairing:\n    def left(self) -> int32\n    def right(self) -> int32\n\nclass Box:\n    value: int32\n\nimpl Pairing for Box:\n    def left(self) -> int32:\n        return 1\n",
            )
            .expect("missing-method snippet should parse"),
        )
        .expect_err("missing trait methods should fail");
    assert!(missing_method
        .message
        .contains("impl of `Pairing` for `Box` is missing method `right`"));
}

#[test]
fn check_reports_duplicate_recursive_and_copy_class_errors() {
    for (source, expected) in [
            (
                "def dup() -> int32:\n    return 1\n\ndef dup() -> int32:\n    return 2\n",
                "duplicate item `dup`",
            ),
            (
                "class Box:\n    value: int32\n\nclass Box:\n    other: int32\n",
                "duplicate item `Box`",
            ),
            (
                "enum Status:\n    Ready\n\nenum Status:\n    Waiting\n",
                "duplicate item `Status`",
            ),
            (
                "trait Show:\n    def show() -> int32\n\ntrait Show:\n    def other() -> int32\n",
                "duplicate item `Show`",
            ),
            (
                "trait Show:\n    def show() -> int32\n    def show() -> int32\n",
                "duplicate method `show` in trait `Show`",
            ),
            (
                "enum Status:\n    Ready\n    Ready\n",
                "duplicate variant `Ready` in enum `Status`",
            ),
            (
                "class Counter:\n    value: int32\n    value: int32\n",
                "duplicate field `value` in class `Counter`",
            ),
            (
                "class Counter:\n    def value() -> int32:\n        return 1\n    def value() -> int32:\n        return 2\n",
                "duplicate method `value` in class `Counter`",
            ),
            (
                "class Node:\n    next: Node\n",
                "recursive field `next` on class `Node` requires `indirect`",
            ),
            (
                "class Node:\n    next: (Node, int32)\n",
                "tuple types cannot be `indirect`",
            ),
            (
                "copy class Holder:\n    name: str\n",
                "field `name` on `copy class Holder` must be a copy type",
            ),
        ] {
            let error = check(crate::parser::parse(source).expect("fixture should parse"))
                .expect_err("invalid program should fail checking");
            assert!(
                error.message.contains(expected),
                "expected `{expected}` in `{}`",
                error.message
            );
        }
}

#[test]
fn check_reports_top_level_lowering_errors_from_source() {
    for (source, expected) in [
        (
            "trait Child: Missing:\n    def label() -> str\n\ndef main():\n    pass\n",
            "unknown trait `Missing`",
        ),
        (
            "trait Bad:\n    def value() -> Missing\n\ndef main():\n    pass\n",
            "unknown type `Missing`",
        ),
        (
            "trait Bad:\n    def value[T: Missing](value: T) -> T\n\ndef main():\n    pass\n",
            "unknown trait `Missing`",
        ),
        (
            "trait Bad:\n    def value() -> int32:\n        pass\n\ndef main():\n    pass\n",
            "method `value` is missing a return",
        ),
        (
            "enum Bad[T: Missing]:\n    Value(T)\n\ndef main():\n    pass\n",
            "unknown trait `Missing`",
        ),
        (
            "enum Bad:\n    Value(Missing)\n\ndef main():\n    pass\n",
            "unknown type `Missing`",
        ),
        (
            "class Bad[T: Missing]:\n    value: T\n\ndef main():\n    pass\n",
            "unknown trait `Missing`",
        ),
        (
            "class Bad:\n    value: Missing\n\ndef main():\n    pass\n",
            "unknown type `Missing`",
        ),
        (
            "class Bad:\n    def value[T: Missing](self, value: T) -> T:\n        return value\n\ndef main():\n    pass\n",
            "unknown trait `Missing`",
        ),
        (
            "class Bad:\n    def value(self) -> Missing:\n        pass\n\ndef main():\n    pass\n",
            "unknown type `Missing`",
        ),
        (
            "def value[T: Missing](value: T) -> T:\n    return value\n\ndef main():\n    pass\n",
            "unknown trait `Missing`",
        ),
        (
            "def value() -> Missing:\n    pass\n\ndef main():\n    pass\n",
            "unknown type `Missing`",
        ),
        (
            "trait Show:\n    def render() -> str\n\nclass Box:\n    value: int32\n\nimpl[T: Missing] Show for Box:\n    def render() -> str:\n        return \"x\"\n\ndef main():\n    pass\n",
            "unknown trait `Missing`",
        ),
        (
            "trait Pair[A, B]:\n    def render() -> str\n\nclass Box:\n    value: int32\n\nimpl Pair for Box:\n    def render() -> str:\n        return \"x\"\n\ndef main():\n    pass\n",
            "expects exactly 2 type arguments",
        ),
        (
            "trait Transform:\n    def map[T](value: T) -> T\n\nclass Box:\n    value: int32\n\nimpl Transform for Box:\n    def map[T: Missing](value: T) -> T:\n        return value\n\ndef main():\n    pass\n",
            "unknown trait `Missing`",
        ),
        (
            "trait Show:\n    def render() -> str\n\nclass Box:\n    value: int32\n\nimpl Show for Box:\n    def render() -> Missing:\n        pass\n\ndef main():\n    pass\n",
            "unknown type `Missing`",
        ),
    ] {
        let error = crate::check_source(source).expect_err("invalid program should fail checking");
        assert!(
            error.message.contains(expected),
            "expected `{expected}` in `{}`",
            error.message
        );
    }
}

#[test]
fn check_lowers_generic_top_level_items_and_impls() {
    let program = check(
            crate::parser::parse(
                "trait Mapper[T]:\n    def map(self, value: own T) -> T\n\nenum Maybe[T]:\n    Some(T)\n    None\n\nclass Box[T]:\n    value: T\n    def take(self, value: own T) -> T:\n        return value\n\ndef wrap[T](value: own T, maybe: Maybe[T]) -> T:\n    return value\n\nimpl[T] Mapper[T] for Box[T]:\n    def map(self, value: own T) -> T:\n        return value\n",
            )
            .expect("generic lowering snippet should parse"),
        )
        .expect("generic lowering snippet should type check");

    let mapper = program.traits.get("Mapper").expect("trait should exist");
    assert_eq!(mapper.decl.type_params, vec!["T".to_string()]);
    assert_eq!(
        mapper
            .methods
            .get("map")
            .expect("trait method should exist")
            .signature
            .params,
        vec![Type::TypeParam("T".to_string())]
    );

    let maybe = program.enums.get("Maybe").expect("enum should exist");
    assert_eq!(maybe.decl.type_params, vec!["T".to_string()]);
    let some_payloads = &maybe
        .variants
        .get("Some")
        .expect("Some should exist")
        .payloads;
    assert_eq!(some_payloads.len(), 1);
    assert_eq!(some_payloads[0].name, None);
    assert_eq!(some_payloads[0].ty, Type::TypeParam("T".to_string()));
    let none_payloads = &maybe
        .variants
        .get("None")
        .expect("None should exist")
        .payloads;
    assert!(none_payloads.is_empty());

    let class = program.classes.get("Box").expect("class should exist");
    assert_eq!(class.decl.type_params, vec!["T".to_string()]);
    assert_eq!(
        class.fields.get("value").expect("field should exist").ty,
        Type::TypeParam("T".to_string())
    );
    assert_eq!(
        class
            .methods
            .get("take")
            .expect("method should exist")
            .signature
            .return_type,
        Type::TypeParam("T".to_string())
    );

    let function = program
        .functions
        .get("wrap")
        .expect("function should exist");
    assert_eq!(function.decl.type_params, vec!["T".to_string()]);
    assert_eq!(
        function.signature.params,
        vec![
            Type::TypeParam("T".to_string()),
            Type::Named("Maybe".to_string(), vec![Type::TypeParam("T".to_string())]),
        ]
    );
    assert_eq!(
        function.signature.return_type,
        Type::TypeParam("T".to_string())
    );

    let impl_info = program
        .trait_impls
        .iter()
        .find(|info| info.trait_name == "Mapper")
        .expect("trait impl should exist");
    assert_eq!(impl_info.trait_args, vec![Type::TypeParam("T".to_string())]);
    assert_eq!(
        impl_info.for_type,
        Type::Named("Box".to_string(), vec![Type::TypeParam("T".to_string())])
    );
    assert_eq!(
        impl_info
            .methods
            .get("map")
            .expect("impl method should exist")
            .signature
            .return_type,
        Type::TypeParam("T".to_string())
    );

    let supertrait_program = check(
        crate::parser::parse(include_str!("../../../examples/traits/supertraits.au"))
            .expect("supertraits example should parse"),
    )
    .expect("supertraits example should type check");
    let labelled = supertrait_program
        .traits
        .get("Labelled")
        .expect("Labelled trait should lower");
    assert_eq!(labelled.supertraits.len(), 1);
    assert_eq!(labelled.supertraits[0].trait_name, "Named");
    assert!(labelled.methods.contains_key("label"));

    let bounded_program = check(
        crate::parser::parse(include_str!("../../../examples/generics/bounded_types.au"))
            .expect("bounded generics example should parse"),
    )
    .expect("bounded generics example should type check");
    let wrapper = bounded_program
        .classes
        .get("Wrapper")
        .expect("Wrapper class should lower");
    assert_eq!(wrapper.type_param_bounds["T"][0].trait_name, "Named");
    let maybe_named = bounded_program
        .enums
        .get("MaybeNamed")
        .expect("MaybeNamed enum should lower");
    assert_eq!(maybe_named.type_param_bounds["T"][0].trait_name, "Named");

    let default_method_program = crate::check_source(
        "\
trait DefaultMapper[T]:
    def identity(self, value: own T) -> T:
        return value

class Box:
    value: int32

impl DefaultMapper[int32] for Box:
    pass

def main():
    pass
",
    )
    .expect("impls should inherit default trait methods with substituted signatures");
    let default_impl = default_method_program
        .trait_impls
        .iter()
        .find(|info| info.trait_name == "DefaultMapper")
        .expect("DefaultMapper impl should exist");
    let identity = default_impl
        .methods
        .get("identity")
        .expect("default method should be inherited by the impl");
    assert_eq!(identity.signature.params, vec![Type::named("int32")]);
    assert_eq!(identity.signature.param_passings, vec![ReceiverKind::Value]);
    assert_eq!(identity.signature.return_type, Type::named("int32"));

    let default_associated_program = crate::check_source(
        "\
trait Factory:
    def answer() -> int32:
        return 42

def main():
    pass
",
    )
    .expect("default associated trait methods should be checked in trait scope");
    let factory = default_associated_program
        .traits
        .get("Factory")
        .expect("Factory trait should exist");
    let answer = factory
        .methods
        .get("answer")
        .expect("default associated method should exist");
    assert!(answer.signature.params.is_empty());
    assert_eq!(answer.signature.return_type, Type::named("int32"));
}

#[test]
fn lower_type_and_imported_context_helpers_cover_builtin_and_context_paths() {
    let mut type_names = BTreeMap::from([
        ("Pair".to_string(), Span::new(1, 1)),
        ("pkg.tools.Widget".to_string(), Span::new(1, 1)),
    ]);
    let mut type_arities = BTreeMap::from([
        ("Pair".to_string(), 2usize),
        ("pkg.tools.Widget".to_string(), 0usize),
    ]);
    let type_params = BTreeMap::from([("T".to_string(), ())]);

    assert_eq!(
        lower_type(
            &type_ref("str"),
            &type_names,
            &type_arities,
            empty_canonical_type_names(),
            &type_params,
        )
        .expect("str should canonicalize"),
        Type::named("str")
    );
    assert_eq!(
        lower_type(
            &TypeRef::named("pkg.tools.Widget", Vec::new(), false, Span::new(1, 1),),
            &type_names,
            &type_arities,
            empty_canonical_type_names(),
            &BTreeMap::new(),
        )
        .expect("qualified imported type should lower"),
        Type::named("pkg.tools.Widget")
    );

    for (invalid_type, expected) in [
        (
            TypeRef::named("T", vec![type_ref("int32")], false, Span::new(2, 1)),
            "type parameter `T` does not take type arguments",
        ),
        (
            nested_type_ref("None", vec![type_ref("int32")]),
            "`None` does not take generic arguments",
        ),
        (
            nested_type_ref("Option", Vec::new()),
            "`Option` expects exactly one type argument",
        ),
        (
            nested_type_ref("Result", vec![type_ref("int32")]),
            "`Result` expects exactly two type arguments",
        ),
        (
            nested_type_ref("Queue", Vec::new()),
            "`Queue` expects exactly one type argument",
        ),
        (
            nested_type_ref("dict", vec![type_ref("str")]),
            "`dict` expects exactly two type arguments",
        ),
        (
            nested_type_ref("MapEntry", vec![type_ref("str")]),
            "unknown type `MapEntry`",
        ),
        (
            nested_type_ref("TaskGroup", vec![type_ref("int32")]),
            "`TaskGroup` does not take type arguments",
        ),
        (
            nested_type_ref("int32", vec![type_ref("str")]),
            "`int32` does not take type arguments",
        ),
        (
            nested_type_ref("Pair", vec![type_ref("int32")]),
            "`Pair` expects exactly 2 type arguments, found 1",
        ),
        (type_ref("Missing"), "unknown type `Missing`"),
    ] {
        let error = lower_type(
            &invalid_type,
            &type_names,
            &type_arities,
            empty_canonical_type_names(),
            &type_params,
        )
        .expect_err("invalid type should fail lowering");
        assert!(
            error.message.contains(expected),
            "expected `{expected}` in `{}`",
            error.message
        );
    }

    let reserved = reject_reserved_type_name("Task", Span::new(3, 1))
        .expect_err("built-in type names are reserved");
    assert!(reserved
        .message
        .contains("`Task` is a reserved built-in type name"));

    let mut child = namespace("pkg.tools.child");
    child
        .classes
        .insert("Inner".to_string(), class_info("Inner", true, Vec::new()));
    let mut imported = namespace("pkg.helpers");
    imported
        .traits
        .insert("Named".to_string(), trait_info("Named", Vec::new()));
    let mut registry_root = namespace("pkg.tools");
    registry_root.classes.insert(
        "Widget".to_string(),
        class_info("Widget", false, Vec::new()),
    );
    registry_root
        .enums
        .insert("Status".to_string(), enum_info("Status", None));
    registry_root
        .traits
        .insert("Show".to_string(), trait_info("Show", Vec::new()));
    registry_root
        .modules
        .insert("child".to_string(), child.clone());
    registry_root
        .imported_modules
        .insert("helpers".to_string(), imported.clone());
    register_module_namespace_types(&registry_root, &mut type_names, &mut type_arities);
    assert!(type_names.contains_key("pkg.tools.Widget"));
    assert!(type_names.contains_key("pkg.tools.Status"));
    assert!(type_names.contains_key("pkg.tools.Show"));
    assert!(type_names.contains_key("pkg.tools.child.Inner"));
    assert!(type_names.contains_key("pkg.helpers.Named"));

    let mut remote_function = FunctionInfo {
        module_name: "pkg.tools".to_string(),
        decl: function_decl("remote_fn"),
        signature: function_signature(Vec::new(), Type::named("int32")),
        type_param_bounds: BTreeMap::new(),
    };
    remote_function.decl.return_type = type_ref("int32");
    remote_function.decl.body = vec![Stmt::Return(crate::ast::ReturnStmt {
        value: Some(expr(ExprKind::Int(7))),
        view: None,
        span: Span::new(1, 1),
    })];
    let mut remote_class = class_info("Widget", false, Vec::new());
    remote_class.module_name = "pkg.tools".to_string();
    let mut remote_enum = enum_info("Status", None);
    remote_enum.module_name = "pkg.tools".to_string();
    let mut remote_trait = trait_info("Show", Vec::new());
    remote_trait.module_name = "pkg.tools".to_string();

    let program = check_with_context(
        crate::parser::parse("def main() -> int32:\n    return remote_fn()\n")
            .expect("main snippet should parse"),
        ModuleContext {
            module_name: "main".to_string(),
            imported_bindings: BTreeMap::from([
                (
                    "remote_fn".to_string(),
                    ImportedBinding::Function(remote_function),
                ),
                ("Widget".to_string(), ImportedBinding::Class(remote_class)),
                ("Status".to_string(), ImportedBinding::Enum(remote_enum)),
                ("Show".to_string(), ImportedBinding::Trait(remote_trait)),
                (
                    "tools".to_string(),
                    ImportedBinding::Module(registry_root.clone()),
                ),
            ]),
            module_registry: BTreeMap::from([("pkg.tools".to_string(), registry_root)]),
            is_entry_module: true,
        },
    )
    .expect("imported binding kinds should seed program context");
    assert!(program.functions.contains_key("remote_fn"));
    assert!(program.classes.contains_key("Widget"));
    assert!(program.enums.contains_key("Status"));
    assert!(program.traits.contains_key("Show"));
    assert!(program.imported_modules.contains_key("tools"));
}

#[test]
fn type_copy_and_display_helpers_cover_builtin_module_and_generic_paths() {
    let program = crate::check_source(
        "\
copy class Count:
    value: int32

class HeapBox[T]:
    value: T

enum CopyState:
    Ready
    Count(int32)

enum HeapState:
    Text(str)

def main():
    pass
",
    )
    .expect("program should type-check");

    assert!(type_is_copy_in_context(
        &Type::Unit,
        &program.classes,
        &program.enums,
    ));
    assert!(!type_is_copy_in_context(
        &Type::Module("pkg.tools".to_string()),
        &program.classes,
        &program.enums,
    ));
    assert!(!type_is_copy_in_context(
        &Type::TypeParam("T".to_string()),
        &program.classes,
        &program.enums,
    ));
    assert!(type_is_copy_in_context(
        &Type::named("int32"),
        &program.classes,
        &program.enums,
    ));
    assert!(type_is_copy_in_context(
        &Type::Named("Option".to_string(), vec![Type::named("int32")]),
        &program.classes,
        &program.enums,
    ));
    assert!(!type_is_copy_in_context(
        &Type::Named("Option".to_string(), vec![Type::named("str")]),
        &program.classes,
        &program.enums,
    ));
    assert!(type_is_copy_in_context(
        &Type::Named(
            "Result".to_string(),
            vec![Type::named("int32"), Type::named("bool")],
        ),
        &program.classes,
        &program.enums,
    ));
    assert!(type_is_copy_in_context(
        &Type::Named("SendError".to_string(), vec![Type::named("int32")]),
        &program.classes,
        &program.enums,
    ));
    assert!(type_is_copy_in_context(
        &Type::Named("Count".to_string(), Vec::new()),
        &program.classes,
        &program.enums,
    ));
    assert!(!type_is_copy_in_context(
        &Type::Named("HeapBox".to_string(), vec![Type::named("str")]),
        &program.classes,
        &program.enums,
    ));
    assert!(!type_is_copy_in_context(
        &Type::Named("HeapBox".to_string(), vec![Type::named("int32")]),
        &program.classes,
        &program.enums,
    ));
    assert!(type_is_copy_in_context(
        &Type::Named("CopyState".to_string(), Vec::new()),
        &program.classes,
        &program.enums,
    ));
    assert!(!type_is_copy_in_context(
        &Type::Named("HeapState".to_string(), Vec::new()),
        &program.classes,
        &program.enums,
    ));
    assert!(!type_is_copy_in_context(
        &Type::Named("Missing".to_string(), Vec::new()),
        &program.classes,
        &program.enums,
    ));

    assert_eq!(Type::Unit.to_string(), "None");
    assert_eq!(
        Type::Module("pkg.tools".to_string()).to_string(),
        "module pkg.tools"
    );
    assert_eq!(Type::TypeParam("T".to_string()).to_string(), "T");
    assert_eq!(Type::named("str").to_string(), "str");
    assert_eq!(
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        )
        .to_string(),
        "dict[str, int32]"
    );
}

#[test]
fn sema_type_helper_suite_covers_default_args_patterns_and_classifiers() {
    let param_names = vec!["value".to_string(), "fallback".to_string()];
    let referenced = default_argument_references_param(
        &expr(ExprKind::Call {
            callee: Box::new(expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Group(Box::new(expr(ExprKind::Name(
                    "value".to_string(),
                )))))),
                field: "trim".to_string(),
            })),
            args: vec![
                arg(expr(ExprKind::String("ignored".to_string()))),
                arg(expr(ExprKind::Map(vec![MapEntryExpr {
                    key: expr(ExprKind::String("k".to_string())),
                    value: expr(ExprKind::FString(vec![
                        FormatPart::Literal("".to_string()),
                        FormatPart::Expr(expr(ExprKind::Name("fallback".to_string()))),
                    ])),
                }]))),
            ],
        }),
        &param_names,
    );
    assert_eq!(referenced, Some("value".to_string()));
    assert_eq!(
        default_argument_references_param(
            &expr(ExprKind::Binary {
                left: Box::new(expr(ExprKind::Int(1))),
                op: BinaryOp::Add,
                right: Box::new(expr(ExprKind::Int(2))),
            }),
            &param_names,
        ),
        None
    );

    let merged = merge_trait_bounds(
        &BTreeMap::from([(
            "T".to_string(),
            vec![TraitBound {
                trait_name: "Show".to_string(),
                trait_args: Vec::new(),
            }],
        )]),
        &BTreeMap::from([
            (
                "T".to_string(),
                vec![TraitBound {
                    trait_name: "Cloneable".to_string(),
                    trait_args: Vec::new(),
                }],
            ),
            (
                "U".to_string(),
                vec![TraitBound {
                    trait_name: "Named".to_string(),
                    trait_args: Vec::new(),
                }],
            ),
        ]),
    );
    assert_eq!(merged.get("T").map(Vec::len), Some(2));
    assert_eq!(merged.get("U").map(Vec::len), Some(1));

    assert!(type_contains_named(
        &Type::Named(
            "Result".to_string(),
            vec![
                Type::Named("Option".to_string(), vec![Type::named("str")]),
                Type::named("int32"),
            ],
        ),
        "str",
    ));
    assert!(!type_contains_named(&Type::Unit, "str"));

    let class_program = crate::check_source(
        "\
class Target:
    value: int32

class Wrapper:
    target: Target

class IndirectWrapper:
    target: indirect Target?

def main():
    pass
",
    )
    .expect("class program should type-check");
    assert!(type_reaches_class_through_non_indirect_fields(
        &Type::named("Wrapper"),
        "Target",
        &class_program.classes,
        &mut BTreeSet::new(),
    ));
    assert!(!type_reaches_class_through_non_indirect_fields(
        &Type::named("IndirectWrapper"),
        "Target",
        &class_program.classes,
        &mut BTreeSet::new(),
    ));

    let substitutions = HashMap::from([
        ("T".to_string(), Type::named("str")),
        ("U".to_string(), Type::named("int32")),
    ]);
    assert_eq!(
        substitute_type(
            &Type::Named(
                "dict".to_string(),
                vec![
                    Type::TypeParam("T".to_string()),
                    Type::TypeParam("U".to_string())
                ],
            ),
            &substitutions,
        ),
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        )
    );
    assert_eq!(
        substitute_trait_bound(
            &TraitBound {
                trait_name: "Mapper".to_string(),
                trait_args: vec![Type::TypeParam("T".to_string())],
            },
            &substitutions,
        ),
        TraitBound {
            trait_name: "Mapper".to_string(),
            trait_args: vec![Type::named("str")],
        }
    );
    assert_eq!(
        substitute_trait_bounds(
            &BTreeMap::from([(
                "T".to_string(),
                vec![TraitBound {
                    trait_name: "Mapper".to_string(),
                    trait_args: vec![Type::TypeParam("U".to_string())],
                }],
            )]),
            &substitutions,
        )
        .get("T")
        .expect("bounds should be substituted")[0]
            .trait_args,
        vec![Type::named("int32")]
    );

    let mut collected = BTreeSet::new();
    collect_type_params_from_type(
        &Type::Named(
            "Result".to_string(),
            vec![
                Type::TypeParam("T".to_string()),
                Type::Named("Option".to_string(), vec![Type::TypeParam("U".to_string())]),
            ],
        ),
        &mut collected,
    );
    assert_eq!(
        collected,
        BTreeSet::from(["T".to_string(), "U".to_string()])
    );

    let type_params = BTreeSet::from(["T".to_string()]);
    let mut pattern_substitutions = HashMap::new();
    assert!(type_pattern_matches(
        &Type::Named("Box".to_string(), vec![Type::TypeParam("T".to_string())]),
        &Type::Named("Box".to_string(), vec![Type::named("str")]),
        &type_params,
        &mut pattern_substitutions,
    ));
    assert_eq!(pattern_substitutions.get("T"), Some(&Type::named("str")));
    assert!(!type_pattern_matches(
        &Type::Named("Box".to_string(), vec![Type::TypeParam("T".to_string())]),
        &Type::Named("list".to_string(), vec![Type::named("str")]),
        &type_params,
        &mut HashMap::new(),
    ));
    assert!(has_unresolved_type_params(&Type::TypeParam(
        "T".to_string()
    )));
    assert!(!has_unresolved_type_params(&Type::named("str")));
    assert_eq!(
        substitutions_from_decl_type_args(
            &["K".to_string(), "V".to_string()],
            &[Type::named("str"), Type::named("int32")],
        ),
        HashMap::from([
            ("K".to_string(), Type::named("str")),
            ("V".to_string(), Type::named("int32")),
        ])
    );

    let mut unified = HashMap::new();
    unify_type_pattern(
        &Type::Named("Box".to_string(), vec![Type::TypeParam("T".to_string())]),
        &Type::Named("Box".to_string(), vec![Type::named("int32")]),
        &mut unified,
    )
    .expect("type patterns should unify");
    assert_eq!(unified.get("T"), Some(&Type::named("int32")));
    let conflict = unify_type_pattern(
        &Type::TypeParam("T".to_string()),
        &Type::named("str"),
        &mut HashMap::from([("T".to_string(), Type::named("int32"))]),
    )
    .expect_err("conflicting substitutions should fail");
    assert!(conflict.message.contains("conflicting inferred types"));
    let mismatch = unify_type_pattern(
        &Type::Named("list".to_string(), vec![Type::named("int32")]),
        &Type::named("str"),
        &mut HashMap::new(),
    )
    .expect_err("named type mismatches should fail");
    assert!(mismatch
        .message
        .contains("expected `list[int32]`, found `str`"));

    assert!(is_builtin_type("list"));
    assert!(!is_builtin_type("Widget"));
    assert!(is_integer_type(&Type::named("int32")));
    assert!(is_float_type(&Type::named("float64")));
    assert!(is_string_type(&Type::named("str")));
    assert!(is_numeric_type(&Type::named("uint64")));
    assert!(!is_numeric_type(&Type::named("str")));
    assert!(integer_type_bounds(&Type::named("int8")).is_some());
    assert!(integer_type_bounds(&Type::named("float64")).is_none());

    let duplicate_enum = crate::check_source(
        "\
enum Status:
    Ready

enum Status:
    Done

def main():
    pass
",
    )
    .expect_err("duplicate enums should be rejected");
    assert!(duplicate_enum.message.contains("duplicate item `Status`"));

    let duplicate_function = crate::check_source(
        "\
def helper():
    pass

def helper():
    pass

def main():
    pass
",
    )
    .expect_err("duplicate functions should be rejected");
    assert!(duplicate_function
        .message
        .contains("duplicate item `helper`"));

    let duplicate_trait_method = crate::check_source(
        "\
trait Show:
    def render() -> str
    def render() -> str

def main():
    pass
",
    )
    .expect_err("duplicate trait methods should be rejected");
    assert!(duplicate_trait_method
        .message
        .contains("duplicate method `render` in trait `Show`"));

    let duplicate_variant = crate::check_source(
        "\
enum Status:
    Ready
    Ready

def main():
    pass
",
    )
    .expect_err("duplicate variants should be rejected");
    assert!(duplicate_variant
        .message
        .contains("duplicate variant `Ready` in enum `Status`"));

    let duplicate_field = crate::check_source(
        "\
class Box:
    value: int32
    value: int32

def main():
    pass
",
    )
    .expect_err("duplicate fields should be rejected");
    assert!(duplicate_field
        .message
        .contains("duplicate field `value` in class `Box`"));

    let duplicate_method = crate::check_source(
        "\
class Box:
    def render() -> str:
        return \"a\"

    def render() -> str:
        return \"b\"

def main():
    pass
",
    )
    .expect_err("duplicate methods should be rejected");
    assert!(duplicate_method
        .message
        .contains("duplicate method `render` in class `Box`"));

    let bad_field_default = crate::check_source(
        "\
class Counter:
    value: int32 = \"zero\"

def main():
    pass
",
    )
    .expect_err("mismatched field defaults should be rejected");
    assert!(bad_field_default
        .message
        .contains("default value for field `value` has type `str`, expected `int32`"));

    let mixed_top_level = crate::check_source(
        "\
print(1)

def main():
    pass
",
    )
    .expect_err("top-level statements and main should not mix");
    assert!(mixed_top_level.message.contains(
        "files cannot mix top-level statements, including declarations, with an explicit `main` function"
    ));

    let main_params = crate::check_source(
        "\
def main(value: int32):
    pass
",
    )
    .expect_err("main parameters should be rejected");
    assert!(main_params
        .message
        .contains("`main` must not take parameters in the bootstrap runtime"));

    let unknown_trait_impl = crate::check_source(
        "\
class Box:
    value: int32

impl Missing for Box:
    def render() -> str:
        return \"x\"

def main():
    pass
",
    )
    .expect_err("unknown impl traits should be rejected");
    assert!(unknown_trait_impl
        .message
        .contains("unknown trait `Missing`"));

    let trait_arity_mismatch = crate::check_source(
        "\
trait Mapper[T]:
    def map(value: T) -> T

class Box:
    value: int32

impl Mapper for Box:
    def map(value: int32) -> int32:
        return value

def main():
    pass
",
    )
    .expect_err("trait impl arity mismatches should be rejected");
    assert!(trait_arity_mismatch
        .message
        .contains("expects exactly 1 type argument"));

    let trait_impl_target_type_param = crate::check_source(
        "\
trait Show:
    def render() -> str

impl[T] Show for T:
    def render() -> str:
        return \"x\"

def main():
    pass
",
    )
    .expect_err("impl targets cannot be bare type params");
    assert!(trait_impl_target_type_param
        .message
        .contains("trait impl target must name a concrete or generic outer type"));

    let duplicate_trait_impl = crate::check_source(
        "\
trait Show:
    def render() -> str

class Box:
    value: int32

impl Show for Box:
    def render() -> str:
        return \"x\"

impl Show for Box:
    def render() -> str:
        return \"y\"

def main():
    pass
",
    )
    .expect_err("duplicate trait impls should be rejected");
    assert!(duplicate_trait_impl
        .message
        .contains("duplicate impl of trait `Show` for `Box`"));

    let trait_impl_unknown_method = crate::check_source(
        "\
trait Show:
    def render() -> str

class Box:
    value: int32

impl Show for Box:
    def missing() -> str:
        return \"x\"

def main():
    pass
",
    )
    .expect_err("unknown impl methods should be rejected");
    assert!(trait_impl_unknown_method
        .message
        .contains("method `missing` is not part of trait `Show`"));

    let trait_impl_receiver_mismatch = crate::check_source(
        "\
trait Show:
    def render() -> str

class Box:
    value: int32

impl Show for Box:
    def render(self) -> str:
        return \"x\"

def main():
    pass
",
    )
    .expect_err("receiver mismatches should be rejected");
    assert!(trait_impl_receiver_mismatch
        .message
        .contains("receiver does not match trait `Show`"));

    let trait_impl_signature_mismatch = crate::check_source(
        "\
trait Show:
    def render() -> str

class Box:
    value: int32

impl Show for Box:
    def render() -> int32:
        return 1

def main():
    pass
",
    )
    .expect_err("trait impl signatures should match");
    assert!(trait_impl_signature_mismatch
        .message
        .contains("does not match the trait signature"));

    let trait_impl_missing_method = crate::check_source(
        "\
trait Show:
    def render() -> str
    def label() -> str

class Box:
    value: int32

impl Show for Box:
    def render() -> str:
        return \"x\"

def main():
    pass
",
    )
    .expect_err("missing trait impl methods should be rejected");
    assert!(trait_impl_missing_method
        .message
        .contains("is missing method `label`"));
}

#[test]
fn moving_a_payload_out_of_a_bare_match_names_match_own() {
    // ADR-0022 Q2: bare `match` is shared, so extracting a payload from one is
    // rejected. The rejection must name the exact replacement rather than the
    // generic borrowed-move wording, because `match own` is the only fix.
    let rejected = crate::check_source(
        r#"
enum Packet:
    Text(str)

def unwrap(packet: own Packet) -> str:
    match packet:
        case Packet.Text(text):
            return text

def main():
    print(unwrap(Packet.Text("hi")))
"#,
    )
    .expect_err("a bare match cannot move its payload out");
    assert_eq!(rejected.code, "AU3002");
    assert_eq!(
        rejected.message,
        "cannot move `text` out of a shared match on `packet`"
    );
    assert_eq!(
        rejected.help,
        vec!["write `match own packet` to consume the scrutinee, or call `.clone()` to consume an independent copy"]
    );

    // The named replacement is what actually compiles and runs.
    let consumed = crate::run_source(
        r#"
enum Packet:
    Text(str)

def unwrap(packet: own Packet) -> str:
    match own packet:
        case Packet.Text(text):
            return text

def main():
    print(unwrap(Packet.Text("hi")))
"#,
    )
    .expect("`match own` should consume the scrutinee");
    assert_eq!(consumed.stdout, "hi\n");
}

#[test]
fn mutable_noncopy_sources_cannot_form_local_aliases() {
    let cases = [
        (
            "parameter/plain",
            r#"
class User:
    name: str

def rename(user: mut User):
    alias = user
    alias.name = "Grace"
"#,
            "user",
        ),
        (
            "parameter/mut",
            r#"
class User:
    name: str

def rename(user: mut User):
    mut alias = user
    alias.name = "Grace"
"#,
            "user",
        ),
        (
            "receiver/plain",
            r#"
class User:
    name: str

    def rename(mut self):
        alias = self
        alias.name = "Grace"
"#,
            "self",
        ),
        (
            "receiver/mut",
            r#"
class User:
    name: str

    def rename(mut self):
        mut alias = self
        alias.name = "Grace"
"#,
            "self",
        ),
        (
            "match/plain",
            r#"
class User:
    name: str

enum Slot:
    Filled(User)

def rename(slot: mut Slot):
    match mut slot:
        case Filled(user):
            alias = user
            alias.name = "Grace"
"#,
            "user",
        ),
        (
            "match/mut",
            r#"
class User:
    name: str

enum Slot:
    Filled(User)

def rename(slot: mut Slot):
    match mut slot:
        case Filled(user):
            mut alias = user
            alias.name = "Grace"
"#,
            "user",
        ),
        (
            "loop/plain",
            r#"
class User:
    name: str

def rename(users: mut list[User]):
    for user in mut users:
        alias = user
        alias.name = "Grace"
"#,
            "user",
        ),
        (
            "loop/mut",
            r#"
class User:
    name: str

def rename(users: mut list[User]):
    for user in mut users:
        mut alias = user
        alias.name = "Grace"
"#,
            "user",
        ),
        (
            "member projection",
            r#"
class Profile:
    name: str

class User:
    profile: Profile

def rename(user: mut User):
    alias = user.profile
    alias.name = "Grace"
"#,
            "user.profile",
        ),
    ];

    for (label, source, mutable_source) in cases {
        let rejected = crate::check_source(source).expect_err(
            "a non-copy value reached through mutable access cannot form a local alias",
        );
        assert_eq!(rejected.code, "AU3002", "{label}");
        assert_eq!(
            rejected.message,
            format!(
                "cannot create local alias `alias` from mutable access to `{mutable_source}`; local mutable aliases do not write through to their source"
            ),
            "{label}"
        );
        assert_eq!(
            rejected.help,
            vec![format!(
                "mutate `{mutable_source}` directly, or pass it to another `mut` parameter"
            )],
            "{label}"
        );
    }

    // Copy-typed assignments materialize independent snapshots, so they do not
    // form the write-through aliases rejected above.
    crate::check_source(
        r#"
def snapshot(counter: mut int32):
    mut copy = counter
    copy += 1
"#,
    )
    .expect("a copy-typed mutable source may still be copied into an independent local");
}

#[test]
fn shared_local_alias_rebinding_has_capability_aware_guidance() {
    let rejected = crate::check_source(
        r#"
class User:
    name: str

def replace(user: User):
    alias = user
    alias = User(name="Grace")
"#,
    )
    .expect_err("shared local aliases must remain non-assignable");

    assert_eq!(rejected.code, "AU3003");
    assert_eq!(
        rejected.message,
        "cannot rebind shared alias `alias`; shared aliases are non-assignable"
    );
    assert_eq!(
        rejected.help,
        vec![
            "use `alias` only for shared access; rebinding requires a separate `mut` value obtained through `own` input or a supported `.clone()`"
        ]
    );

    crate::check_source(
        r#"
class User:
    name: str

def replace(user: own User):
    mut replacement = user
    replacement = User(name="Grace")
"#,
    )
    .expect("an owned input can initialize a separate rebindable value");
    crate::check_source(
        r#"
def replace(user: str):
    mut replacement = user.clone()
    replacement = "Grace"
"#,
    )
    .expect("a supported clone can initialize a separate rebindable value");
}

#[test]
fn ownership_help_uses_current_capability_language() {
    let cases = [
        (
            r#"
def consume(value: own str):
    pass

def main():
    value = "hello"
    consume(value)
    print(value)
"#,
            "AU3001",
            "pass shared access when ownership is not needed, or call `.clone()` at the move site when an independent value is required",
        ),
        (
            r#"
import random

def consume(value: own random.Rng):
    pass

def main():
    mut value = random.Rng(seed=1)
    consume(value)
    print(value)
"#,
            "AU3001",
            "pass shared access when ownership is not needed, or transfer this non-cloneable value only once",
        ),
        (
            r#"
class Data:
    value: int32

def read_and_write(read: Data, write: mut Data):
    write.value += read.value

def main():
    mut data = Data(value=1)
    read_and_write(data, data)
"#,
            "AU3002",
            "pass non-overlapping places; shared accesses may overlap, but mutable access must remain exclusive",
        ),
        (
            r#"
class Pair:
    first: str
    second: str

def main():
    pair = Pair(first="a", second="b")
    first = pair.first
    print(pair.first)
"#,
            "AU3001",
            "use shared access to the field when ownership is not needed, or call `.clone()` before moving it when an independent value is required",
        ),
    ];

    for (source, code, expected_help) in cases {
        let rejected = crate::check_source(source).expect_err("ownership misuse must be rejected");
        assert_eq!(rejected.code, code, "{source}");
        assert_eq!(rejected.help, vec![expected_help], "{source}");
        assert!(
            !rejected.help.join(" ").contains("borrow"),
            "help must use current capability language: {source}"
        );
    }
}

#[test]
fn moved_collection_diagnostic_offers_the_canonical_copy_member() {
    for (collection_type, literal) in [
        ("list[int64]", "[1]"),
        ("dict[str, int64]", "{\"one\": 1}"),
        ("set[int64]", "{1}"),
    ] {
        let source = format!(
            r#"
def consume(values: own {collection_type}):
    pass

def main():
    values = {literal}
    consume(values)
    print(values)
"#
        );
        let rejected = crate::check_source(&source)
            .expect_err("using a transferred collection must be rejected");

        assert_eq!(rejected.code, "AU3001", "{source}");
        assert_eq!(
            rejected.help,
            vec![
                "pass shared access when ownership is not needed, or call `.copy()` at the move site when an independent value is required"
            ],
            "{source}"
        );
        assert_eq!(rejected.edits.len(), 1, "{source}");
        assert_eq!(rejected.edits[0].replacement, ".copy()", "{source}");
    }
}

#[test]
fn shared_match_aliases_keep_source_provenance_until_last_use() {
    for (label, alias_expr, alias_use) in [
        ("direct", "user", "alias.profile.name"),
        ("member", "user.profile", "alias.name"),
    ] {
        let source = format!(
            r#"
class Profile:
    name: str

class User:
    profile: Profile

enum Slot:
    Filled(User)
    Empty

def main():
    mut slot = Slot.Filled(User(profile=Profile(name="Ada")))
    match slot:
        case Filled(user):
            alias = {alias_expr}
            slot = Slot.Empty
            print({alias_use})
        case Empty:
            pass
"#
        );
        let rejected = crate::check_source(&source)
            .expect_err("a shared match alias cannot outlive mutation of its source");
        assert_eq!(rejected.code, "AU3002", "{label}");
        assert_eq!(
            rejected.message,
            "cannot use shared match binding `alias` after changing match scrutinee `slot`",
            "{label}"
        );
        assert_eq!(
            rejected.help,
            vec!["finish using `alias` before changing `slot`; use `match mut slot` to update its payload or `match own slot` to consume it"],
            "{label}"
        );
    }

    let direct_payload = crate::check_source(
        r#"
class User:
    name: str

enum Slot:
    Filled(User)
    Empty

def main():
    mut slot = Slot.Filled(User(name="Ada"))
    match slot:
        case Filled(user):
            slot = Slot.Empty
            print(user.name)
        case Empty:
            pass
"#,
    )
    .expect_err("the original shared payload must retain its match provenance");
    assert_eq!(direct_payload.code, "AU3002");
    assert_eq!(
        direct_payload.message,
        "cannot use shared match binding `user` after changing match scrutinee `slot`"
    );

    let branch_change = crate::check_source(
        r#"
class User:
    name: str

enum Slot:
    Filled(User)
    Empty

def main():
    mut slot = Slot.Filled(User(name="Ada"))
    match slot:
        case Filled(user):
            alias = user
            if true:
                slot = Slot.Empty
            print(alias.name)
        case Empty:
            pass
"#,
    )
    .expect_err("a possible source change in control flow must conservatively stale the alias");
    assert_eq!(branch_change.code, "AU3002");
    assert_eq!(
        branch_change.message,
        "cannot use shared match binding `alias` after changing match scrutinee `slot`"
    );

    let after_last_use = crate::run_source(
        r#"
class User:
    name: str

enum Slot:
    Filled(User)
    Empty

def main():
    mut slot = Slot.Filled(User(name="Ada"))
    match slot:
        case Filled(user):
            alias = user
            print(alias.name)
            slot = Slot.Empty
            print("updated")
        case Empty:
            pass
"#,
    )
    .expect("source mutation after an alias's last use should remain valid");
    assert_eq!(after_last_use.stdout, "Ada\nupdated\n");

    let returned_scrutinee = crate::check_source(
        r#"
class User:
    name: str

enum Slot:
    Filled(User)
    Empty

def shared(slot: Slot) -> view Slot from slot:
    return view slot

def main():
    mut slot = Slot.Filled(User(name="Ada"))
    match shared(slot):
        case Filled(user):
            alias = user
            slot = Slot.Empty
            print(alias.name)
        case Empty:
            pass
"#,
    )
    .expect_err("a returned shared match scrutinee must retain alias provenance");
    assert_eq!(returned_scrutinee.code, "AU3002", "{returned_scrutinee:?}");
    assert_eq!(
        returned_scrutinee.message,
        "cannot use pattern binding `alias` after reassigning match scrutinee `slot`"
    );

    crate::check_source(
        r#"
class User:
    name: str

enum Slot:
    Filled(User)
    Empty

def shared(slot: Slot) -> view Slot from slot:
    return view slot

def main():
    mut slot = Slot.Filled(User(name="Ada"))
    match shared(slot):
        case Filled(user):
            alias = user
            print(alias.name)
            slot = Slot.Empty
        case Empty:
            pass
"#,
    )
    .expect("a non-Copy returned match loan may end after its final alias use");
}

#[test]
fn bare_copy_matches_retain_logical_shared_access_through_the_arm() {
    let cases = [
        (
            "statement root mutation",
            r#"
def bump(value: mut int32):
    value += 1

def main():
    mut value: int32 = 1
    match value:
        case 1:
            bump(value)
        case _:
            pass
"#,
            "cannot mutate `value` while `value` remains shared by a bare match",
        ),
        (
            "statement member mutation",
            r#"
class Counter:
    value: int32

def bump(value: mut int32):
    value += 1

def main():
    mut counter = Counter(value=1)
    match counter.value:
        case 1:
            bump(counter.value)
        case _:
            pass
"#,
            "cannot mutate `counter.value` while `counter.value` remains shared by a bare match",
        ),
        (
            "expression mutation",
            r#"
def bump(value: mut int32) -> int32:
    value += 1
    return value

def main():
    mut value: int32 = 1
    result = match value:
        case 1: bump(value)
        case _: value
    print(result)
"#,
            "cannot mutate `value` while `value` remains shared by a bare match",
        ),
        (
            "owned copy access",
            r#"
def take(value: own int32):
    print(value)

def main():
    mut value: int32 = 1
    match value:
        case 1:
            take(value)
        case _:
            pass
"#,
            "cannot consume `value` while `value` remains shared by a bare match",
        ),
        (
            "owned copy member access",
            r#"
class Counter:
    value: int32

def take(value: own int32):
    print(value)

def main():
    mut counter = Counter(value=1)
    match counter.value:
        case 1:
            take(counter.value)
        case _:
            pass
"#,
            "cannot consume `counter.value` while `counter.value` remains shared by a bare match",
        ),
        (
            "nested owned copy match",
            r#"
def main():
    mut value: int32 = 1
    match value:
        case 1:
            match own value:
                case 1:
                    pass
                case _:
                    pass
        case _:
            pass
"#,
            "cannot consume `value` while `value` remains shared by a bare match",
        ),
        (
            "owned copy receiver",
            r#"
copy class Counter:
    value: int32

    def take(own self):
        print(self.value)

def main():
    mut counter = Counter(value=1)
    match counter.value:
        case 1:
            counter.take()
        case _:
            pass
"#,
            "cannot consume `counter` while `counter.value` remains shared by a bare match",
        ),
    ];

    for (label, source, expected) in cases {
        let rejected = crate::check_source(source)
            .expect_err("a bare copy match keeps a logical shared access through its arm");
        assert_eq!(rejected.code, "AU3002", "{label}");
        assert_eq!(rejected.message, expected, "{label}");
    }

    crate::check_source(
        r#"
def main():
    mut value: int32 = 1
    match value:
        case 1:
            snapshot = value
            print(snapshot)
        case _:
            pass
"#,
    )
    .expect("ordinary copy snapshots inside a bare match are not ownership transfers");
}

#[test]
fn builtin_bare_copy_arguments_retain_shared_access_through_later_arguments() {
    let cases = [
        (
            "range start",
            r#"
def bump(value: mut int32) -> int32:
    value += 1
    return value

def main():
    mut value: int32 = 1
    selected = range(value, bump(value))
"#,
        ),
        (
            "min left",
            r#"
def bump(value: mut int32) -> int32:
    value += 1
    return value

def main():
    mut value: int32 = 1
    selected = min(value, bump(value))
"#,
        ),
        (
            "max left",
            r#"
def bump(value: mut int32) -> int32:
    value += 1
    return value

def main():
    mut value: int32 = 1
    selected = max(value, bump(value))
"#,
        ),
        (
            "random lower bound",
            r#"
import random

def bump(value: mut int64) -> int64:
    value += 1
    return value

def sample(rng: mut random.Rng, value: mut int64):
    selected = rng.next_int(value, bump(value))
"#,
        ),
        (
            "list.set index",
            r#"
def bump(value: mut int32) -> int32:
    value += 1
    return value

def update(values: mut list[int32], index: mut int32):
    values.set(index, bump(index))
"#,
        ),
        (
            "list.insert index",
            r#"
def bump(value: mut int32) -> int32:
    value += 1
    return value

def update(values: mut list[int32], index: mut int32):
    values.insert(index, bump(index))
"#,
        ),
        (
            "list.swap first",
            r#"
def bump(value: mut int32) -> int32:
    value += 1
    return value

def update(values: mut list[int32], index: mut int32):
    values.swap(index, bump(index))
"#,
        ),
        (
            "HTTP text status",
            r#"
import net

def text_after_bump(status: mut int32) -> str:
    status += 1
    return "done"

def respond(exchange: net.HttpExchange, status: mut int32):
    exchange.respond_text(status, text_after_bump(status), {})
"#,
        ),
        (
            "HTTP bytes status",
            r#"
import net

def bytes_after_bump(status: mut int32) -> list[uint8]:
    status += 1
    return [1 as uint8]

def respond(exchange: net.HttpExchange, status: mut int32):
    exchange.respond_bytes(status, bytes_after_bump(status), {})
"#,
        ),
        (
            "TCP read_bytes maximum",
            r#"
import net

def timeout_after_bump(count: mut int32) -> Duration:
    count += 1
    return Duration.ms(1)

def read(stream: net.TcpStream, count: mut int32):
    stream.read_bytes(count, timeout_after_bump(count))
"#,
        ),
        (
            "TCP read_exact count",
            r#"
import net

def timeout_after_bump(count: mut int32) -> Duration:
    count += 1
    return Duration.ms(1)

def read(stream: net.TcpStream, count: mut int32):
    stream.read_exact(count, timeout_after_bump(count))
"#,
        ),
        (
            "Unix read_exact count",
            r#"
import net

def timeout_after_bump(count: mut int32) -> Duration:
    count += 1
    return Duration.ms(1)

def read(stream: net.UnixStream, count: mut int32):
    stream.read_exact(count, timeout_after_bump(count))
"#,
        ),
        (
            "TLS read_exact count",
            r#"
import net

def timeout_after_bump(count: mut int32) -> Duration:
    count += 1
    return Duration.ms(1)

def read(stream: net.TlsStream, count: mut int32):
    stream.read_exact(count, timeout_after_bump(count))
"#,
        ),
        (
            "UDP recv maximum",
            r#"
import net

def timeout_after_bump(count: mut int32) -> Duration:
    count += 1
    return Duration.ms(1)

def read(socket: net.UdpSocket, count: mut int32):
    socket.recv(count, timeout_after_bump(count))
"#,
        ),
        (
            "UDP recv_from maximum",
            r#"
import net

def timeout_after_bump(count: mut int32) -> Duration:
    count += 1
    return Duration.ms(1)

def read(socket: net.UdpSocket, count: mut int32):
    socket.recv_from(count, timeout_after_bump(count))
"#,
        ),
        (
            "process pipe read_bytes maximum",
            r#"
import process

def timeout_after_bump(count: mut int32) -> Duration:
    count += 1
    return Duration.ms(1)

def read(pipe: process.Pipe, count: mut int32):
    pipe.read_bytes(count, timeout_after_bump(count))
"#,
        ),
    ];

    for (label, source) in cases {
        let rejected = crate::check_source(source)
            .expect_err("a bare copy builtin argument must retain shared access");
        assert_eq!(rejected.code, "AU3002", "{label}");
        assert!(
            rejected.message.contains("remains shared-borrowed"),
            "{label}: {}",
            rejected.message
        );
    }

    crate::check_source(
        r#"
def bump(value: mut int32) -> int32:
    value += 1
    return value

def update(values: mut list[int32], index: mut int32):
    values.set(value=bump(index), index=index)
"#,
    )
    .expect("a mutation completed before the later shared argument does not overlap it");
}
#[test]
fn task_group_stack_override_checks_type_literal_bounds_and_target_arguments() {
    let valid = crate::check_source(
        r#"
def worker(left: int32, right: int32) -> int32:
    return left + right

def main() -> int32:
    with TaskGroup() as group:
        task = group.start_with_stack(262144, worker, right=2, left=1)
        group.start_soon_with_stack(67108864, worker, left=1, right=2)
        return task.result_or(-1)
"#,
    );
    valid.expect("inclusive stack bounds and forwarded named target arguments should type-check");

    let returned_view = crate::check_source(
        r#"
def borrowed(value: mut int64) -> view mut int64 from value:
    return view mut value

def worker(value: int64) -> int64:
    return value

def main():
    mut value = 1
    with TaskGroup() as group:
        task = group.start_with_stack(262144, worker, value=borrowed(value))
        print(task.result_or(-1))
"#,
    )
    .expect_err("a named forwarded returned view cannot cross a task boundary");
    assert_eq!(returned_view.code, "AU3008", "{returned_view:?}");
    assert_eq!(
        returned_view.message,
        "view argument 1 cannot cross a task boundary"
    );

    for (literal, expected) in [
        (
            "262143",
            "task stack size must be between 262144 and 67108864 bytes, found 262143",
        ),
        (
            "67108865",
            "task stack size must be between 262144 and 67108864 bytes, found 67108865",
        ),
        (
            "-1",
            "task stack size must be between 262144 and 67108864 bytes, found -1",
        ),
    ] {
        let source = format!(
            "def worker() -> int32:\n    return 1\n\ndef main() -> int32:\n    with TaskGroup() as group:\n        group.start_with_stack({literal}, worker)\n    return 0\n"
        );
        let error = crate::check_source(&source)
            .expect_err("literal stack sizes outside the supported range must be rejected");
        assert_eq!(error.code, "AU2002");
        assert_eq!(error.message, expected);
    }

    for method in ["start_with_stack", "start_soon_with_stack"] {
        let source = format!(
            "def worker() -> int32:\n    return 1\n\ndef main() -> int32:\n    with TaskGroup() as group:\n        group.{method}(\"large\", worker)\n    return 0\n"
        );
        let wrong_type = crate::check_source(&source).expect_err("stack size must be int64");
        assert_eq!(wrong_type.code, "AU2002", "{method}");
        assert!(
            wrong_type.message.contains("expects `int64`, found `str`"),
            "unexpected diagnostic for {method}: {wrong_type:?}"
        );
    }
}

#[test]
fn task_boundaries_accept_structurally_transferable_values_and_results() {
    crate::check_source(
        r#"
class Message:
    label: str
    samples: list[int64]

enum Delivery:
    Ready(Message)
    Routed(queue: Queue[str], task: Task[str])

def echo(payload: own Delivery, metadata: own dict[str, set[int32]]) -> Delivery:
    print(metadata)
    return payload

def relay(task: own Task[str], queue: Queue[str]) -> (Task[str], Queue[str]):
    return (task, queue)

def launch(task: own Task[str], queue: Queue[str]):
    with group = TaskGroup():
        metadata = dict[str, set[int32]]()
        payload = Delivery.Ready(Message(label="ready", samples=[1, 2]))
        group.start(echo, payload, metadata)
        group.start(relay, task, queue)
"#,
    )
    .expect(
        "copy data, str, structural containers, classes, enums, tuples, and handles are Transfer",
    );
}

#[test]
fn task_boundaries_derive_transfer_for_both_select_outcome_payload_categories() {
    crate::check_source(
        r#"
def consume(outcome: own SelectOutcome[str, int32]):
    print(outcome)

def launch(outcome: own SelectOutcome[str, int32]):
    with group = TaskGroup():
        group.start(consume, outcome)
"#,
    )
    .expect("SelectOutcome is Transfer when both payload categories are Transfer");

    for (type_args, category) in [
        (
            "random.Rng, int32",
            "queue payload of `SelectOutcome[random.Rng, int32]`",
        ),
        (
            "str, random.Rng",
            "task payload of `SelectOutcome[str, random.Rng]`",
        ),
    ] {
        let source = format!(
            r#"
import random

def consume(outcome: own SelectOutcome[{type_args}]):
    print(outcome)

def launch(outcome: own SelectOutcome[{type_args}]):
    with group = TaskGroup():
        group.start(consume, outcome)
"#
        );
        let rejected = crate::check_source(&source)
            .expect_err("a non-Transfer SelectOutcome payload must not cross a task boundary");
        assert_eq!(rejected.code, "AU3008", "{rejected:?}");
        assert!(
            rejected
                .message
                .contains("task argument `outcome` cannot cross a task boundary"),
            "{rejected:?}"
        );
        assert!(
            rejected.message.contains(&format!(
                "{category} -> `random.Rng` is a stateful generator and is not Transfer"
            )),
            "{rejected:?}"
        );
    }
}

#[test]
fn range_is_transfer_without_becoming_copy() {
    crate::check_source(
        r#"
def echo(values: own Range, output: Queue[Range]) -> Range:
    output.put(values)
    return range(4, 7)

def keep_handle(output: Queue[Range]) -> Queue[Range]:
    return output

def launch():
    output = Queue[Range]()
    with group = TaskGroup():
        group.start(echo, range(0, 3), output)
        group.start(keep_handle, output)
"#,
    )
    .expect("Range data crosses task and Queue boundaries while Queue handles remain Transfer");

    assert!(
        !Type::named("Range").is_copy(),
        "Phase 5.6 must preserve Range's existing non-Copy ownership semantics"
    );
}

#[test]
fn task_boundary_diagnostics_explain_the_exact_nested_non_transfer_reason() {
    let nested_argument = crate::check_source(
        r#"
import random

class GeneratorBox:
    generator: random.Rng

class Job:
    boxes: list[GeneratorBox]

def use_job(job: own Job):
    pass

def launch(job: own Job):
    with group = TaskGroup():
        group.start_soon(use_job, job)
"#,
    )
    .expect_err("a nested random generator must not cross a task boundary");
    assert_eq!(nested_argument.code, "AU3008");
    assert!(
        nested_argument
            .message
            .contains("task argument `job` cannot cross a task boundary"),
        "{nested_argument:?}"
    );
    assert!(
        nested_argument.message.contains(
            "field `boxes` of `Job` -> element of `list[GeneratorBox]` -> field `generator` of `GeneratorBox` -> `random.Rng` is a stateful generator and is not Transfer"
        ),
        "{nested_argument:?}"
    );

    let nested_result = crate::check_source(
        r#"
import fs

enum WorkerResult:
    Opened(fs.File)
    Skipped

def worker() -> WorkerResult:
    return WorkerResult.Skipped

def launch():
    with group = TaskGroup():
        group.start_soon(worker)
"#,
    )
    .expect_err("a host resource nested in a task result must be rejected");
    assert_eq!(nested_result.code, "AU3008");
    assert!(
        nested_result
            .message
            .contains("task result `WorkerResult` cannot cross a task boundary"),
        "{nested_result:?}"
    );
    assert!(
        nested_result.message.contains(
            "variant `Opened` of `WorkerResult` -> payload 1 -> `fs.File` is a host resource and is not Transfer"
        ),
        "{nested_result:?}"
    );
}

#[test]
fn task_transfer_checks_use_the_concrete_generic_specialization() {
    crate::check_source(
        r#"
def echo[T](value: own T) -> T:
    return value

def launch():
    with group = TaskGroup():
        group.start(echo, "transfer")
"#,
    )
    .expect("a generic task target specialized with str should be Transfer");

    let rejected = crate::check_source(
        r#"
import random

def echo[T](value: own T) -> T:
    return value

def launch():
    with group = TaskGroup():
        group.start(echo, random.Rng(seed=7))
"#,
    )
    .expect_err("the concrete random.Rng specialization is not Transfer");
    assert_eq!(rejected.code, "AU3008");
    assert!(
        rejected
            .message
            .contains("task argument `value` cannot cross a task boundary"),
        "{rejected:?}"
    );
    assert!(
        rejected
            .message
            .contains("`random.Rng` is a stateful generator and is not Transfer"),
        "{rejected:?}"
    );
}

#[test]
fn task_transfer_is_compiler_derived_and_not_a_user_trait_escape_hatch() {
    let rejected = crate::check_source(
        r#"
import random

class Wrapper:
    generator: random.Rng

trait Transfer:
    pass

impl Transfer for Wrapper:
    pass

def worker(value: own Wrapper):
    pass

def launch(value: own Wrapper):
    with group = TaskGroup():
        group.start_soon(worker, value)
"#,
    )
    .expect_err("a same-named user trait must not affect structural Transfer");
    assert_eq!(rejected.code, "AU3008");
    assert!(rejected.message.contains("field `generator` of `Wrapper`"));
}

#[test]
fn transfer_uses_stored_components_not_phantom_type_arguments() {
    crate::check_source(
        r#"
import random

class Phantom[T]:
    value: int32

def worker(value: own Phantom[random.Rng]) -> int32:
    return value.value

def launch(value: own Phantom[random.Rng]):
    with group = TaskGroup():
        group.start(worker, value)
"#,
    )
    .expect("an unused generic argument is not a stored Transfer component");
}

#[test]
fn transfer_coinduction_checks_changing_recursive_specializations() {
    crate::check_source(
        r#"
class Growing[T]:
    value: T
    next: indirect Growing[list[T]]

def worker(value: own Growing[int32]):
    pass

def launch(value: own Growing[int32]):
    with group = TaskGroup():
        group.start_soon(worker, value)
"#,
    )
    .expect("a recursive specialization whose stored components stay Transfer should terminate");

    let rejected = crate::check_source(
        r#"
import random

class Node[T]:
    value: T
    next: indirect Node[random.Rng]

def worker(value: own Node[int32]):
    pass

def launch(value: own Node[int32]):
    with group = TaskGroup():
        group.start_soon(worker, value)
"#,
    )
    .expect_err("a changing recursive specialization must inspect its substituted stored field");
    assert_eq!(rejected.code, "AU3008");
    assert!(
        rejected
            .message
            .contains("field `next` of `Node` -> field `value` of `Node` -> `random.Rng`"),
        "{rejected:?}"
    );
}

#[test]
fn task_result_observation_rights_follow_repeatability() {
    crate::check_source(
        r#"
def number() -> int32:
    return 7

def launch():
    with group = TaskGroup():
        task = group.start(number)
        print(task.result())
        print(task.result_or(0))
"#,
    )
    .expect("copy task results remain repeatably observable");

    let consumed = crate::check_source(
        r#"
def text() -> str:
    return "ready"

def launch():
    with group = TaskGroup():
        task = group.start(text)
        print(task.result_or("missing"))
        print(task.result_or("missing"))
"#,
    )
    .expect_err("a non-repeatable task result has one observation right");
    assert_eq!(consumed.code, "AU3001");
    assert!(consumed.message.contains("use of moved value `task`"));

    let shared = crate::check_source(
        r#"
def observe(task: Task[str]):
    print(task.result_or("missing"))
"#,
    )
    .expect_err("shared access cannot consume a single task-result observation right");
    assert_eq!(shared.code, "AU3002");
    assert!(shared.message.contains("parameter `task` is borrowed"));

    assert!(Type::Named("Task".to_string(), vec![Type::named("int32")]).is_copy());
    assert!(!Type::Named("Task".to_string(), vec![Type::named("str")]).is_copy());
    assert!(Type::Named(
        "Task".to_string(),
        vec![Type::Named(
            "Queue".to_string(),
            vec![Type::named("random.Rng")]
        )]
    )
    .is_copy());
    assert!(!Type::Named(
        "Task".to_string(),
        vec![Type::Named("Task".to_string(), vec![Type::named("str")])]
    )
    .is_copy());
}

#[test]
fn clone_producing_operations_cannot_duplicate_task_observation_rights() {
    let rejected = crate::check_source(
        r#"
def duplicate(tasks: list[Task[str]]) -> list[Task[str]]:
    return tasks.copy()
"#,
    )
    .expect_err("cloning a container must not duplicate single-consumer Task handles");
    assert_eq!(rejected.code, "AU3009");
    assert!(
        rejected
            .message
            .contains("second observation right for non-repeatable task result `str`"),
        "{rejected:?}"
    );
}

#[test]
fn queue_transport_requires_transfer_payloads_but_handle_only_methods_do_not() {
    let constructed = crate::check_source(
        r#"
import random

def launch():
    queue = Queue[random.Rng]()
"#,
    )
    .expect_err("constructing a Queue transport for random.Rng must fail");
    assert_eq!(constructed.code, "AU3008");
    assert!(constructed.message.contains("Queue payload `random.Rng`"));

    let sent = crate::check_source(
        r#"
import random

def send(queue: Queue[random.Rng], value: own random.Rng):
    queue.put(value)
"#,
    )
    .expect_err("put must reject a non-Transfer payload even on an external handle");
    assert_eq!(sent.code, "AU3008");
    assert!(sent.message.contains("Queue payload `random.Rng`"));

    crate::check_source(
        r#"
import random

def close_only(queue: Queue[random.Rng]):
    queue.close()
"#,
    )
    .expect("handle-only Queue operations do not transport their payload");
}

#[test]
fn owned_builtin_snapshots_are_transfer_but_live_authority_is_not() {
    crate::check_source(
        r#"
import net
import process

def completed(value: own process.Completed) -> process.Completed:
    return value

def response(value: own net.HttpResponse) -> net.HttpResponse:
    return value

def datagram(value: own net.UdpDatagram) -> net.UdpDatagram:
    return value

def launch(
    completed_value: own process.Completed,
    response_value: own net.HttpResponse,
    datagram_value: own net.UdpDatagram
):
    with group = TaskGroup():
        group.start(completed, completed_value)
        group.start(response, response_value)
        group.start(datagram, datagram_value)
"#,
    )
    .expect("completed process, HTTP, and UDP snapshots contain only owned Transfer data");

    let live = crate::check_source(
        r#"
import net

def worker(stream: own net.TcpStream):
    pass

def launch(stream: own net.TcpStream):
    with group = TaskGroup():
        group.start_soon(worker, stream)
"#,
    )
    .expect_err("a live stream is host authority and is not Transfer");
    assert_eq!(live.code, "AU3008");
    assert!(live.message.contains("`net.TcpStream` is a host resource"));
}

#[test]
fn dict_items_clone_observers_preserve_single_task_result_rights() {
    let rejected = crate::check_source(
        "def duplicate(values: dict[str, Task[str]]):\n    print(values.items())\n",
    )
    .expect_err("dict item cloning must not duplicate a Task[str] right");
    assert_eq!(rejected.code, "AU3009", "{rejected:?}");
    assert!(
        rejected
            .message
            .contains("non-repeatable task result `str`"),
        "{rejected:?}"
    );
}

#[test]
fn empty_set_constructor_requires_an_explicit_element_type() {
    let rejected = crate::check_source("def main():\n    values = set()\n")
        .expect_err("an empty set constructor cannot infer its element type");

    assert_eq!(rejected.code, "AU2005");
    assert_eq!(
        rejected.message,
        "empty set construction requires an explicit element type"
    );
    assert_eq!(
        rejected.help,
        ["write `set[T]()` with the intended element type"]
    );
    assert!(rejected.edits.is_empty());
}

#[test]
fn task_target_explicit_specialization_and_contextual_defaults_are_concrete() {
    crate::check_source(
        r#"
def empty[T]() -> Option[T]:
    return Option.None

class Factory:
    def empty[T]() -> Option[T]:
        return Option.None

def pair[A, B]() -> (Option[A], Option[B]):
    return (Option.None, Option.None)

def fallback(value: own Option[str] = Option.None) -> Option[str]:
    return value

def launch():
    with group = TaskGroup():
        first = group.start(empty[str])
        second = group.start(Factory.empty[str])
        third = group.start(fallback)
        group.start(pair[str, int32])
        print(first.result_or(Option.None))
        print(second.result_or(Option.None))
        print(third.result_or(Option.None))
"#,
    )
    .expect("explicit callable type arguments and contextual defaults must classify concretely");

    let arity = crate::check_source(
        r#"
def pair[A, B]() -> (Option[A], Option[B]):
    return (Option.None, Option.None)

def launch():
    with group = TaskGroup():
        group.start(pair[str])
"#,
    )
    .expect_err("task callable specialization must enforce all explicit type arguments");
    assert!(arity
        .message
        .contains("function `pair` expects 2 type arguments, found 1"));
}

#[test]
fn task_capture_materializes_copy_snapshots_but_not_noncopy_shared_views() {
    crate::check_source(
        r#"
def worker(value: int32) -> int32:
    return value

def launch(value: int32):
    with group = TaskGroup():
        group.start(worker, value)
"#,
    )
    .expect("a borrowed Copy parameter can materialize an owned snapshot for the child");

    let rejected = crate::check_source(
        r#"
def worker(value: own str):
    pass

def launch(value: str):
    with group = TaskGroup():
        group.start_soon(worker, value)
"#,
    )
    .expect_err("a noncopy shared parameter cannot be moved into a child");
    assert_eq!(rejected.code, "AU3002");
    assert!(rejected.message.contains("parameter `value` is borrowed"));
}

#[test]
fn transfer_recursion_does_not_skip_a_third_changing_specialization() {
    let rejected = crate::check_source(
        r#"
import random

class Stair[A, B]:
    value: A
    next: indirect Stair[B, random.Rng]

def worker(value: own Stair[int32, str]):
    pass

def launch(value: own Stair[int32, str]):
    with group = TaskGroup():
        group.start_soon(worker, value)
"#,
    )
    .expect_err("the third recursive specialization stores random.Rng");
    assert_eq!(rejected.code, "AU3008");
    assert!(
        rejected
            .message
            .contains("field `next` of `Stair` -> field `next` of `Stair` -> field `value` of `Stair` -> `random.Rng`"),
        "{rejected:?}"
    );
}

#[test]
fn task_observation_duplication_ignores_phantom_type_arguments() {
    crate::check_source(
        r#"
class Phantom[T]:
    marker: int32

def duplicate(values: list[Phantom[Task[str]]]) -> list[Phantom[Task[str]]]:
    return values.copy()
"#,
    )
    .expect("a Task in an unused type argument does not create a stored observation right");

    crate::check_source(
        r#"
class Growing[T]:
    value: T
    next: indirect Growing[list[T]]

def duplicate(values: list[Growing[Task[int32]]]) -> list[Growing[Task[int32]]]:
    return values.copy()
"#,
    )
    .expect("changing recursive specializations must not invent an observation right");
}

#[test]
fn conditional_task_observation_consumption_participates_in_argument_overlap() {
    let task_member = crate::check_source(
        r#"
class Holder:
    task: Task[str]

def derive(holder: Holder) -> str:
    return "fallback"

def observe(holder: own Holder):
    print(holder.task.result_or(derive(holder)))
"#,
    )
    .expect_err("the consuming Task receiver precedes and conflicts with the fallback read");
    assert_eq!(task_member.code, "AU3002");
    assert!(
        task_member.message.contains("overlap")
            || task_member.message.contains("partially moved")
            || task_member.message.contains("moved field")
            || task_member.message.contains("reserved for consumption"),
        "{task_member:?}"
    );

    let wait = crate::check_source(
        r#"
def derive_timeout(tasks: list[Task[str]]) -> Duration:
    return 1ms

def observe(tasks: own list[Task[str]]):
    print(wait_any(tasks, timeout=derive_timeout(tasks)))
"#,
    )
    .expect_err("the consuming wait collection precedes and conflicts with the timeout read");
    assert_eq!(wait.code, "AU3002");
    assert!(
        wait.message.contains("overlap")
            || wait.message.contains("moved value")
            || wait.message.contains("borrow"),
        "{wait:?}"
    );

    crate::check_source(
        r#"
def derive_timeout(tasks: list[Task[int32]]) -> Duration:
    return 1ms

def observe(tasks: list[Task[int32]]):
    print(wait_any(tasks, timeout=derive_timeout(tasks)))
    print(wait_all(tasks, timeout=derive_timeout(tasks)))
"#,
    )
    .expect("repeatable Task[int32] observation does not consume the tasks vector");
}

#[test]
fn transfer_diagnostics_preserve_dict_result_and_generic_witnesses() {
    let map_key = crate::check_source(
        r#"
import random

def consume(values: own dict[random.Rng, str]):
    pass

def launch(values: own dict[random.Rng, str]):
    with group = TaskGroup():
        group.start(consume, values)
"#,
    )
    .expect_err("a non-Transfer map key must be diagnosed at task admission");
    assert_eq!(map_key.code, "AU3008");
    assert_eq!(map_key.span, Some(Span::new(9, 30)));
    assert!(
        map_key
            .message
            .contains("key of `dict[random.Rng, str]` -> `random.Rng`"),
        "{map_key:?}"
    );
    assert!(map_key
        .help
        .iter()
        .any(|help| help.contains("keep capabilities and host resources on their owning worker")));

    let result_error = crate::check_source(
        r#"
import random

def produce() -> Result[str, random.Rng]:
    return Result.Ok("ready")

def launch():
    with group = TaskGroup():
        group.start(produce)
"#,
    )
    .expect_err("a non-Transfer Result error must be checked as part of the task result");
    assert_eq!(result_error.code, "AU3008");
    assert!(
        result_error
            .message
            .contains("error payload of `Result[str, random.Rng]` -> `random.Rng`"),
        "{result_error:?}"
    );

    let unresolved = crate::check_source(
        r#"
def consume[T](value: own T):
    pass

def launch[T](value: own T):
    with group = TaskGroup():
        group.start(consume, value)
"#,
    )
    .expect_err("an unspecialized generic task payload has no proven Transfer shape");
    assert_eq!(unresolved.code, "AU3008");
    assert!(
        unresolved
            .message
            .contains("type parameter `T` has no compiler-proven Transfer specialization"),
        "{unresolved:?}"
    );

    let module = crate::check_source(
        r#"
import fs

def consume[T](value: own T):
    pass

def launch():
    with group = TaskGroup():
        group.start(consume, fs)
"#,
    )
    .expect_err("an imported module capability must not cross a task boundary");
    assert_eq!(module.code, "AU3008");
    assert!(
        module
            .message
            .contains("`module fs` is a module capability and is not Transfer"),
        "{module:?}"
    );
}

#[test]
fn queue_try_put_enforces_structural_transfer_at_the_payload_span() {
    let rejected = crate::check_source(
        r#"
import random

class Envelope:
    value: random.Rng

def send(queue: Queue[Envelope], value: own Envelope):
    queue.try_put(value)
"#,
    )
    .expect_err("try_put transports its payload and must enforce structural Transfer");
    assert_eq!(rejected.code, "AU3008");
    assert_eq!(rejected.span, Some(Span::new(8, 11)));
    assert!(rejected.message.contains("Queue payload `Envelope`"));
    assert!(
        rejected
            .message
            .contains("field `value` of `Envelope` -> `random.Rng`"),
        "{rejected:?}"
    );
    assert!(rejected
        .help
        .iter()
        .any(|help| help.contains("use a Queue payload made only from Transfer components")));
}

#[test]
fn clone_rejection_follows_stored_task_rights_through_classes_enums_and_generics() {
    let class = crate::check_source(
        r#"
class Holder:
    label: str
    task: Task[str]

def duplicate(values: list[Holder]) -> list[Holder]:
    return values.copy()
"#,
    )
    .expect_err("a stored Task[str] right makes its enclosing class non-duplicable");
    assert_eq!(class.code, "AU3009");
    assert_eq!(class.span, Some(Span::new(7, 19)));
    assert!(class.message.contains("non-repeatable task result `str`"));

    let enumeration = crate::check_source(
        r#"
enum Work:
    Empty
    Pending(Task[str])

def duplicate(values: list[Work]) -> list[Work]:
    return values.copy()
"#,
    )
    .expect_err("a task right in a later enum variant must still prevent duplication");
    assert_eq!(enumeration.code, "AU3009");
    assert!(enumeration
        .message
        .contains("non-repeatable task result `str`"));

    crate::check_source(
        r#"
class Holder[T]:
    value: T

class Observer[T]:
    task: Task[T]

def copy_containment(values: list[Holder[Task[int32]]]) -> list[Holder[Task[int32]]]:
    return values.copy()

def copy_conditional(values: list[Observer[int32]]) -> list[Observer[int32]]:
    return values.copy()
"#,
    )
    .expect("copy-result tasks remain repeatably observable through generic stored fields");

    for source in [
        r#"
class Holder[T]:
    value: T

def duplicate(values: list[Holder[Task[str]]]) -> list[Holder[Task[str]]]:
    return values.copy()
"#,
        r#"
class Observer[T]:
    task: Task[T]

def duplicate(values: list[Observer[str]]) -> list[Observer[str]]:
    return values.copy()
"#,
    ] {
        let rejected = crate::check_source(source)
            .expect_err("a concrete str specialization has one stored task-result right");
        assert_eq!(rejected.code, "AU3009", "{rejected:?}");
        assert!(
            rejected
                .message
                .contains("non-repeatable task result `str`"),
            "{rejected:?}"
        );
    }
}

#[test]
fn task_repeatability_is_derived_through_every_symbolic_copy_wrapper() {
    crate::check_source(
        r#"
copy class CopyPoint:
    x: int32

enum Maybe[T]:
    None
    Some(T)

class TupleObserver[T]:
    task: Task[(T, int32)]

class OptionObserver[T]:
    task: Task[Option[T]]

class ResultObserver[T]:
    task: Task[Result[int32, T]]

class SendObserver[T]:
    task: Task[SendError[T]]

class ReceiveObserver[T]:
    task: Task[QueueReceive[T]]

class NestedTaskObserver[T]:
    task: Task[Task[T]]

class EnumObserver[T]:
    task: Task[Maybe[T]]

class CopyClassObserver:
    task: Task[CopyPoint]

def copy_tuple(values: list[TupleObserver[int32]]) -> list[TupleObserver[int32]]:
    return values.copy()

def copy_option(values: list[OptionObserver[int32]]) -> list[OptionObserver[int32]]:
    return values.copy()

def copy_result(values: list[ResultObserver[int32]]) -> list[ResultObserver[int32]]:
    return values.copy()

def copy_send(values: list[SendObserver[int32]]) -> list[SendObserver[int32]]:
    return values.copy()

def copy_receive(values: list[ReceiveObserver[int32]]) -> list[ReceiveObserver[int32]]:
    return values.copy()

def copy_nested_task(values: list[NestedTaskObserver[int32]]) -> list[NestedTaskObserver[int32]]:
    return values.copy()

def copy_enum(values: list[EnumObserver[int32]]) -> list[EnumObserver[int32]]:
    return values.copy()

def copy_class(values: list[CopyClassObserver]) -> list[CopyClassObserver]:
    return values.copy()
"#,
    )
    .expect("every all-Copy specialization retains repeatable task observation");

    for (shape, source, result) in [
        (
            "tuple",
            r#"
class Observer[T]:
    task: Task[(T, int32)]

def duplicate(values: list[Observer[str]]) -> list[Observer[str]]:
    return values.copy()
"#,
            "(str, int32)",
        ),
        (
            "option",
            r#"
class Observer[T]:
    task: Task[Option[T]]

def duplicate(values: list[Observer[str]]) -> list[Observer[str]]:
    return values.copy()
"#,
            "Option[str]",
        ),
        (
            "result",
            r#"
class Observer[T]:
    task: Task[Result[int32, T]]

def duplicate(values: list[Observer[str]]) -> list[Observer[str]]:
    return values.copy()
"#,
            "Result[int32, str]",
        ),
        (
            "send error",
            r#"
class Observer[T]:
    task: Task[SendError[T]]

def duplicate(values: list[Observer[str]]) -> list[Observer[str]]:
    return values.copy()
"#,
            "SendError[str]",
        ),
        (
            "queue receive",
            r#"
class Observer[T]:
    task: Task[QueueReceive[T]]

def duplicate(values: list[Observer[str]]) -> list[Observer[str]]:
    return values.copy()
"#,
            "QueueReceive[str]",
        ),
        (
            "nested task",
            r#"
class Observer[T]:
    task: Task[Task[T]]

def duplicate(values: list[Observer[str]]) -> list[Observer[str]]:
    return values.copy()
"#,
            "Task[str]",
        ),
        (
            "generic enum",
            r#"
enum Maybe[T]:
    None
    Some(T)

class Observer[T]:
    task: Task[Maybe[T]]

def duplicate(values: list[Observer[str]]) -> list[Observer[str]]:
    return values.copy()
"#,
            "Maybe[str]",
        ),
        (
            "non-copy class",
            r#"
class HeapValue:
    text: str

class Observer:
    task: Task[HeapValue]

def duplicate(values: list[Observer]) -> list[Observer]:
    return values.copy()
"#,
            "HeapValue",
        ),
    ] {
        let rejected = crate::check_source(source)
            .expect_err("the concrete non-Copy task result must remain single-consumer");
        assert_eq!(rejected.code, "AU3009", "{shape}: {rejected:?}");
        assert!(
            rejected
                .message
                .contains(&format!("non-repeatable task result `{result}`")),
            "{shape}: {rejected:?}"
        );
    }
}

#[test]
fn recursive_result_shapes_remain_single_consumer_without_nontermination() {
    let rejected = crate::check_source(
        r#"
enum Chain[T]:
    End
    Link(indirect Chain[T])

class Observer:
    task: Task[Chain[int32]]

def duplicate(values: list[Observer]) -> list[Observer]:
    return values.copy()
"#,
    )
    .expect_err("recursive heap-backed result shapes are not implicitly repeatable");
    assert_eq!(rejected.code, "AU3009");
    assert!(rejected
        .message
        .contains("non-repeatable task result `Chain[int32]`"));
}

#[test]
fn task_target_specialization_accepts_nested_and_grouped_type_arguments() {
    crate::check_source(
        r#"
def empty[T]() -> Option[T]:
    return Option.None

def identity[T](value: own T) -> T:
    return value

class Factory:
    def empty[T]() -> Option[T]:
        return Option.None

def launch():
    with group = TaskGroup():
        group.start(empty[Option[str]])
        group.start(Factory.empty[Result[str, int32]])
        group.start(identity[((str, int32))], ("ready", 7))
"#,
    )
    .expect("task targets accept nested named types and one grouped tuple type argument");

    let invalid = crate::check_source(
        r#"
def empty[T]() -> Option[T]:
    return Option.None

def make_type() -> str:
    return "str"

def launch():
    with group = TaskGroup():
        group.start(empty[make_type()])
"#,
    )
    .expect_err("a runtime call expression is not a task-target type argument");
    assert_eq!(invalid.code, "AU2002");
    assert_eq!(invalid.span, Some(Span::new(10, 27)));
    assert!(
        invalid
            .message
            .contains("function specialization expects type arguments"),
        "{}",
        invalid.message
    );
}

#[test]
fn capture_free_function_types_are_copy_values_with_declaration_spelling() {
    let function = Type::Function {
        params: vec![
            FunctionParamContract {
                name: "left".to_string(),
                ty: Type::named("int32"),
                passing: ReceiverKind::Borrow,
                has_default: false,
                default_erased: false,
            },
            FunctionParamContract {
                name: "right".to_string(),
                ty: Type::named("str"),
                passing: ReceiverKind::Value,
                has_default: true,
                default_erased: false,
            },
            FunctionParamContract {
                name: "counter".to_string(),
                ty: Type::named("int64"),
                passing: ReceiverKind::BorrowMut,
                has_default: false,
                default_erased: false,
            },
        ],
        return_type: Box::new(Type::named("bool")),
    };
    assert!(function.is_copy(), "capture-free code pointers are Copy");
    assert_eq!(
        function.to_string(),
        "def(int32, own str, mut int64) -> bool",
        "written function types preserve parameter capability contracts"
    );
}

#[test]
fn named_function_values_cover_variables_parameters_fields_and_collections() {
    crate::check_source(
        r#"
def increment(value: int32) -> int32:
    return value + 1

def double(value: int32) -> int32:
    return value * 2

def apply(f: def(int32) -> int32, value: int32) -> int32:
    return f(value)

class Pipeline:
    transform: def(int32) -> int32

def main():
    inferred = increment
    copied = inferred
    annotated: def(int32) -> int32 = increment
    pipeline = Pipeline(transform=annotated)
    transforms: list[def(int32) -> int32] = [increment, double]
    first: int32 = apply(copied, 1)
    second: int32 = pipeline.transform(first)
    third: int32 = transforms[1](second)
    fourth: int32 = inferred(third)
"#,
    )
    .expect("named function values should survive every required storage surface");
}

#[test]
fn inferred_function_values_preserve_own_and_mut_capabilities() {
    crate::check_source(
        r#"
def consume(value: own str) -> int32:
    return 1

def bump(value: mut int32):
    value += 1

def main():
    consuming = consume
    mutating = bump
    text = "owned"
    mut count: int32 = 0
    result: int32 = consuming(text)
    mutating(count)
"#,
    )
    .expect("inferred function types retain the declared own and mut capabilities");

    let immutable = crate::check_source(
        r#"
def bump(value: mut int32):
    value += 1

def main():
    callback = bump
    count: int32 = 0
    callback(count)
"#,
    )
    .expect_err("indirect mutable calls still require a mutable place");
    assert!(immutable.message.contains("must be a mutable place"));
}

#[test]
fn written_function_types_preserve_nonshared_capabilities_and_reject_mismatches() {
    for source in [
        "def transform(value: own str) -> str:\n    return value\n\ndef main():\n    callback: def(own str) -> str = transform\n    text = \"value\"\n    result: str = callback(text)\n",
        "def transform(value: mut str):\n    value += \"!\"\n\ndef main():\n    callback: def(mut str) -> None = transform\n    mut text = \"value\"\n    callback(text)\n",
    ] {
        crate::check_source(source)
            .expect("written function types should preserve explicit nonshared capabilities");
    }

    let mismatch = crate::check_source(
        "def transform(value: own str) -> str:\n    return value\n\ndef main():\n    callback: def(mut str) -> str = transform\n",
    )
    .expect_err("different written capabilities remain incompatible");
    assert_eq!(mismatch.code, "AU2002");
    assert!(mismatch
        .message
        .contains("function parameter 1 has `own` capability"));
    assert!(mismatch
        .message
        .contains("matching bare, `mut`, or `own` prefix"));
}

#[test]
fn generic_function_values_support_explicit_and_contextual_specialization() {
    crate::check_source(
        r#"
def identity[T](value: own T) -> T:
    return value

def empty[T]() -> Option[T]:
    return Option.None

def main():
    identity_string = identity[str]
    copied = identity_string
    value: str = copied("ready")
    empty_string: def() -> Option[str] = empty
    missing: Option[str] = empty_string()
"#,
    )
    .expect("generic function values specialize explicitly or from an expected function type");

    let ambiguous = crate::check_source(
        r#"
def empty[T]() -> Option[T]:
    return Option.None

def main():
    callback = empty
"#,
    )
    .expect_err("an unconstrained generic function value is not concrete");
    assert!(ambiguous
        .message
        .contains("requires explicit type arguments or an expected function type"));
}

#[test]
fn concrete_function_values_keep_named_and_default_call_rules() {
    crate::check_source(
        r#"
def offset(value: int32 = 3, scale: int32 = 2) -> int32:
    return value + scale

def main():
    callback = offset
    first: int32 = callback()
    second: int32 = callback(scale=4, value=10)
    third: int32 = callback(7, scale=5)
"#,
    )
    .expect("a concrete function value retains parameter names and default availability");
}

#[test]
fn dynamic_function_contracts_retain_only_names_and_defaults_that_agree() {
    crate::check_source(
        r#"
def plus_one(value: int32 = 1) -> int32:
    return value + 1

def plus_ten(value: int32 = 10) -> int32:
    return value + 10

def main():
    selected = plus_one if true else plus_ten
    defaulted: int32 = selected()
    named: int32 = selected(value=4)
"#,
    )
    .expect("different default values are supplied by the runtime-selected target");

    let names = crate::check_source(
        r#"
def first(value: int32 = 1) -> int32:
    return value

def second(number: int32 = 2) -> int32:
    return number

def main():
    selected = first if true else second
    defaulted: int32 = selected()
    selected(value=3)
"#,
    )
    .expect_err("different names erase named access without erasing shared defaults");
    assert_eq!(names.code, "AU2003");
    assert!(names.message.contains("contract was erased"));

    let defaults = crate::check_source(
        r#"
def required(value: int32) -> int32:
    return value

def defaulted(value: int32 = 2) -> int32:
    return value

def main():
    selected = required if true else defaulted
    selected()
"#,
    )
    .expect_err("a join with different default masks requires all positional arguments");
    assert_eq!(defaults.code, "AU2003");
    assert!(defaults.message.contains("complete positional list"));
}

#[test]
fn function_reassignment_intersects_named_and_default_contracts() {
    let names = crate::check_source(
        r#"
def first(value: int32 = 1) -> int32:
    return value

def second(number: int32 = 2) -> int32:
    return number

def main():
    mut selected = first
    selected = second
    selected(value=3)
"#,
    )
    .expect_err("reassignment to a differently named target erases named arguments");
    assert_eq!(names.code, "AU2003");
    assert!(names.message.contains("contract was erased"));

    let defaults = crate::check_source(
        r#"
def first(value: int32 = 1) -> int32:
    return value

def second(value: int32) -> int32:
    return value

def main():
    mut selected = first
    selected = second
    selected()
"#,
    )
    .expect_err("reassignment to a required target erases default availability");
    assert_eq!(defaults.code, "AU2003");
    assert!(defaults.message.contains("complete positional list"));
}

#[test]
fn control_flow_reassignment_intersects_function_contracts() {
    let error = crate::check_source(
        r#"
def first(value: int32 = 1) -> int32:
    return value

def second(number: int32 = 2) -> int32:
    return number

def choose(use_second: bool) -> int32:
    mut selected = first
    if use_second:
        selected = second
    return selected(value=3)
"#,
    )
    .expect_err("post-branch function contracts include every reachable assignment");
    assert_eq!(error.code, "AU2003");
    assert!(error.message.contains("contract was erased"));
}

#[test]
fn repeated_generic_evidence_intersects_callable_contract_metadata() {
    let callback = |name: &str, has_default: bool| Type::Function {
        params: vec![FunctionParamContract {
            name: name.to_string(),
            ty: Type::named("int32"),
            passing: ReceiverKind::Borrow,
            has_default,
            default_erased: false,
        }],
        return_type: Box::new(Type::named("int32")),
    };

    let mut names = HashMap::new();
    unify_type_pattern(
        &Type::TypeParam("T".to_string()),
        &callback("value", true),
        &mut names,
    )
    .expect("first generic observation binds the function contract");
    unify_type_pattern(
        &Type::TypeParam("T".to_string()),
        &callback("number", true),
        &mut names,
    )
    .expect("ABI-equal function evidence remains compatible");
    let Type::Function { params, .. } = names.get("T").expect("T is inferred") else {
        panic!("T should remain a function type");
    };
    assert_eq!(params[0].name, "");
    assert!(params[0].has_default);
    assert!(!params[0].default_erased);

    let nested = |contract| {
        Type::Tuple(vec![Type::Named(
            "list".to_string(),
            vec![Type::Named("Holder".to_string(), vec![contract])],
        )])
    };
    let mut nested_defaults = HashMap::new();
    unify_type_pattern(
        &Type::TypeParam("T".to_string()),
        &nested(callback("value", true)),
        &mut nested_defaults,
    )
    .expect("first nested generic observation binds T");
    unify_type_pattern(
        &Type::TypeParam("T".to_string()),
        &nested(callback("value", false)),
        &mut nested_defaults,
    )
    .expect("nested ABI-equal evidence remains compatible");
    let nested_result = nested_defaults.get("T").expect("nested T is inferred");
    let Type::Tuple(tuple) = nested_result else {
        panic!("nested result should retain its tuple wrapper");
    };
    let Type::Named(_, vec_args) = &tuple[0] else {
        panic!("nested result should retain its Vec wrapper");
    };
    let Type::Named(_, holder_args) = &vec_args[0] else {
        panic!("nested result should retain its Holder wrapper");
    };
    let Type::Function { params, .. } = &holder_args[0] else {
        panic!("nested result should retain its callback");
    };
    assert_eq!(params[0].name, "value");
    assert!(!params[0].has_default);
    assert!(params[0].default_erased);

    let type_params = BTreeSet::from(["T".to_string()]);
    let mut matcher_substitutions = HashMap::new();
    assert!(type_pattern_matches(
        &Type::TypeParam("T".to_string()),
        &nested(callback("value", true)),
        &type_params,
        &mut matcher_substitutions,
    ));
    assert!(type_pattern_matches(
        &Type::TypeParam("T".to_string()),
        &nested(callback("number", true)),
        &type_params,
        &mut matcher_substitutions,
    ));
    let merged = matcher_substitutions
        .get("T")
        .expect("trait-style matching also retains the safe intersection");
    let Type::Tuple(tuple) = merged else {
        panic!("matcher substitution should retain its tuple wrapper");
    };
    let Type::Named(_, vec_args) = &tuple[0] else {
        panic!("matcher substitution should retain its Vec wrapper");
    };
    let Type::Named(_, holder_args) = &vec_args[0] else {
        panic!("matcher substitution should retain its Holder wrapper");
    };
    let Type::Function { params, .. } = &holder_args[0] else {
        panic!("matcher substitution should retain its callback");
    };
    assert_eq!(params[0].name, "");
    assert!(params[0].has_default);
    assert!(!params[0].default_erased);
}

#[test]
fn generic_choose_returns_only_the_callable_contract_common_to_all_evidence() {
    let defaults = crate::check_source(
        r#"
def choose[T](first: own T, second: own T, use_second: bool) -> T:
    return second if use_second else first

def defaulted(value: int32 = 1) -> int32:
    return value

def required(value: int32) -> int32:
    return value

def main():
    selected = choose(defaulted, required, true)
    selected()
"#,
    )
    .expect_err("generic inference must not retain a default absent from later evidence");
    assert_eq!(defaults.code, "AU2003");
    assert!(defaults.message.contains("complete positional list"));

    let names = crate::check_source(
        r#"
def choose[T](first: own T, second: own T, use_second: bool) -> T:
    return second if use_second else first

def first(value: int32 = 1) -> int32:
    return value

def second(number: int32 = 2) -> int32:
    return number

def main():
    selected = choose(first, second, true)
    selected(value=3)
"#,
    )
    .expect_err("generic inference must not retain a name absent from later evidence");
    assert_eq!(names.code, "AU2003");
    assert!(names.message.contains("contract was erased"));

    for (surface, main_body) in [
        (
            "tuple",
            "    selected = choose((first, first), (second, second), true)[0]\n    selected(value=3)\n",
        ),
        (
            "list",
            "    selected = choose([first], [second], true)[0]\n    selected(value=3)\n",
        ),
        (
            "generic Holder field",
            "    selected = choose(\n        Holder(callback=first),\n        Holder(callback=second),\n        true\n    ).callback\n    selected(value=3)\n",
        ),
    ] {
        let source = format!(
            r#"
class Holder[T]:
    callback: T

def choose[T](first: own T, second: own T, use_second: bool) -> T:
    return second if use_second else first

def first(value: int32 = 1) -> int32:
    return value

def second(number: int32 = 2) -> int32:
    return number

def main():
{main_body}"#,
        );
        let nested = crate::check_source(&source)
            .expect_err("nested generic results must recursively intersect callable contracts");
        assert_eq!(
            nested.code, "AU2003",
            "{surface} generic evidence should erase incompatible callable names: {}",
            nested.message,
        );
        assert!(nested.message.contains("contract was erased"));
    }
}

#[test]
fn inferred_own_and_mut_function_contracts_survive_generic_and_storage_paths() {
    crate::check_source(
        r#"
def identity[T](value: own T) -> T:
    return value

def consume(value: own str) -> int32:
    return 1

def bump(value: mut int32):
    value += 1

class Holder[T]:
    callback: T

def main():
    consuming = identity(consume)
    mutating = identity(bump)
    consuming_values = [consume, consuming]
    mutating_values = [bump, mutating]
    consuming_holder = Holder(callback=consuming)
    mutating_holder = Holder(callback=mutating)
    text = "owned"
    mut count: int32 = 0
    first: int32 = consuming_values[0](text)
    mutating_values[1](count)
    other = "second"
    second: int32 = consuming_holder.callback(other)
    mutating_holder.callback(count)
"#,
    )
    .expect("inferred generic, vector, and generic-field storage preserves own/mut capabilities");
}

#[test]
fn mutable_function_storage_erases_names_and_defaults_but_keeps_exact_calls() {
    crate::check_source(
        r#"
class Holder[T]:
    callback: T

def first(value: int32 = 1) -> int32:
    return value

def second(number: int32 = 2) -> int32:
    return number

def main():
    mut callbacks = [first]
    callbacks.append(second)
    callbacks.set(0, second)
    from_vec: int32 = callbacks[0](3)

    mut callbacks_by_name = {"first": first}
    callbacks_by_name["second"] = second
    from_map: int32 = callbacks_by_name["second"](4)

    mut holder = Holder(callback=first)
    holder.callback = second
    from_field: int32 = holder.callback(5)
"#,
    )
    .expect("mutable storage preserves structural signatures for exact positional calls");

    let vec_names = crate::check_source(
        r#"
def first(value: int32 = 1) -> int32:
    return value

def second(number: int32 = 2) -> int32:
    return number

def main():
    mut callbacks = [first]
    callbacks.append(second)
    callbacks.set(0, second)
    callbacks[0](value=3)
"#,
    )
    .expect_err("Vec mutation makes declaration names unavailable");
    assert_eq!(vec_names.code, "AU2003");
    assert!(vec_names.message.contains("contract was erased"));

    let vec_defaults = crate::check_source(
        r#"
def defaulted(value: int32 = 1) -> int32:
    return value

def required(value: int32) -> int32:
    return value

def main():
    mut callbacks = [defaulted]
    callbacks.append(required)
    callbacks.set(0, required)
    callbacks[0]()
"#,
    )
    .expect_err("Vec mutation makes omitted arguments unavailable");
    assert_eq!(vec_defaults.code, "AU2003");
    assert!(vec_defaults.message.contains("complete positional list"));

    let map_names = crate::check_source(
        r#"
def first(value: int32 = 1) -> int32:
    return value

def second(number: int32 = 2) -> int32:
    return number

def main():
    mut callbacks = {"first": first}
    callbacks["second"] = second
    callbacks["second"](number=3)
"#,
    )
    .expect_err("Map mutation makes declaration names unavailable");
    assert_eq!(map_names.code, "AU2003");
    assert!(map_names.message.contains("contract was erased"));

    let field_defaults = crate::check_source(
        r#"
class Holder[T]:
    callback: T

def defaulted(value: int32 = 1) -> int32:
    return value

def required(value: int32) -> int32:
    return value

def main():
    mut holder = Holder(callback=defaulted)
    holder.callback = required
    holder.callback()
"#,
    )
    .expect_err("generic field mutation makes omitted arguments unavailable");
    assert_eq!(field_defaults.code, "AU2003");
    assert!(field_defaults.message.contains("complete positional list"));
}

#[test]
fn function_values_are_transfer_task_targets() {
    crate::check_source(
        r#"
def worker(value: int32) -> int32:
    return value + 1

def notify(value: int32):
    print(value)

def main():
    task_target = worker
    soon_target = notify
    with group = TaskGroup():
        task: Task[int32] = group.start(task_target, 1)
        group.start_soon(soon_target, 2)
"#,
    )
    .expect("capture-free function values are Transfer and valid task targets");
}

#[test]
fn imported_module_functions_are_first_class_values() {
    let remote_function = FunctionInfo {
        module_name: "pkg.tools".to_string(),
        decl: unary_function_decl("remote_fn"),
        signature: function_signature(vec![Type::named("int32")], Type::named("int32")),
        type_param_bounds: BTreeMap::new(),
    };
    let namespace = ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "tools".to_string(),
        path: "pkg.tools".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: BTreeMap::from([("remote_fn".to_string(), remote_function.clone())]),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::from([("remote_fn".to_string(), remote_function)]),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    };
    let module = crate::parser::parse(
        "def main():\n    callback = tools.remote_fn\n    result: int32 = callback(1)\n",
    )
    .expect("module-function value fixture should parse");
    check_with_context(
        module,
        ModuleContext {
            module_name: "<main>".to_string(),
            imported_bindings: BTreeMap::from([(
                "tools".to_string(),
                ImportedBinding::Module(namespace.clone()),
            )]),
            module_registry: BTreeMap::from([("pkg.tools".to_string(), namespace)]),
            is_entry_module: true,
        },
    )
    .expect("module-qualified named functions should be usable as values");
}

#[test]
fn nested_imported_module_functions_are_first_class_values() {
    let triple = FunctionInfo {
        module_name: "function_value_imported_support.helpers".to_string(),
        decl: unary_function_decl("triple"),
        signature: function_signature(vec![Type::named("int32")], Type::named("int32")),
        type_param_bounds: BTreeMap::new(),
    };
    let helpers = ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "helpers".to_string(),
        path: "function_value_imported_support.helpers".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: BTreeMap::from([("triple".to_string(), triple.clone())]),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::from([("triple".to_string(), triple)]),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    };
    let support = ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "function_value_imported_support".to_string(),
        path: "function_value_imported_support".to_string(),
        source_path: None,
        modules: BTreeMap::from([("helpers".to_string(), helpers.clone())]),
        functions: BTreeMap::new(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::new(),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    };
    let module = crate::parser::parse(
        "def main():\n    callback = function_value_imported_support.helpers.triple\n    result: int32 = callback(4)\n",
    )
    .expect("nested module-function value fixture should parse");
    check_with_context(
        module,
        ModuleContext {
            module_name: "<main>".to_string(),
            imported_bindings: BTreeMap::from([(
                "function_value_imported_support".to_string(),
                ImportedBinding::Module(support.clone()),
            )]),
            // The real package loader registers the compiled leaf module;
            // the synthetic import-root namespace lives only in bindings.
            module_registry: BTreeMap::from([(
                "function_value_imported_support.helpers".to_string(),
                helpers,
            )]),
            is_entry_module: true,
        },
    )
    .expect("functions in nested imported namespaces should be usable as values");
}

#[test]
fn method_values_and_function_trait_dispatch_are_explicitly_out_of_scope() {
    let method = crate::check_source(
        r#"
class Counter:
    value: int32

    def read(self) -> int32:
        return self.value

def main():
    counter = Counter(value=1)
    callback = counter.read
"#,
    )
    .expect_err("instance method values remain out of scope");
    assert_eq!(method.code, "AU2005");
    assert!(method.message.contains("method values are not supported"));

    let trait_dispatch = crate::check_source(
        r#"
trait Marker:
    def mark(self) -> int32

def accept[T: Marker](value: T):
    pass

def function(value: int32) -> int32:
    return value

def main():
    accept(function)
"#,
    )
    .expect_err("function values do not enter trait dispatch");
    assert_eq!(trait_dispatch.code, "AU2005");
    assert!(trait_dispatch
        .message
        .contains("function values do not participate in trait or trait-object dispatch"));
}

#[test]
fn function_type_helpers_preserve_nested_generic_shape_and_capability_diagnostics() {
    let span = Span::new(1, 1);
    let function_ref = TypeRef::function_with_params(
        vec![crate::ast::FunctionTypeParam::new(
            ParamMode::BorrowMut,
            type_ref("T"),
            span,
        )],
        nested_type_ref("list", vec![type_ref("U")]),
        span,
    );
    let mut ref_params = BTreeSet::new();
    collect_type_ref_type_params(&function_ref, &BTreeMap::new(), &mut ref_params, false);
    assert_eq!(
        ref_params,
        BTreeSet::from(["T".to_string(), "U".to_string()]),
        "implicit generic discovery walks function parameters and returns",
    );

    let pattern = Type::Function {
        params: vec![FunctionParamContract {
            name: String::new(),
            ty: Type::TypeParam("T".to_string()),
            passing: ReceiverKind::BorrowMut,
            has_default: false,
            default_erased: true,
        }],
        return_type: Box::new(Type::Named(
            "list".to_string(),
            vec![Type::TypeParam("U".to_string())],
        )),
    };
    let mut collected = BTreeSet::new();
    collect_type_params_from_type(&pattern, &mut collected);
    assert_eq!(
        collected,
        BTreeSet::from(["T".to_string(), "U".to_string()])
    );
    assert!(has_unresolved_type_params(&pattern));
    assert!(
        has_unresolved_type_params(&Type::Function {
            params: vec![FunctionParamContract {
                name: String::new(),
                ty: Type::named("int32"),
                passing: ReceiverKind::Borrow,
                has_default: false,
                default_erased: true,
            }],
            return_type: Box::new(Type::TypeParam("U".to_string())),
        }),
        "an otherwise concrete function type remains unresolved through its return type",
    );
    assert_eq!(
        type_pattern_specificity(&pattern),
        2,
        "a function contributes one structural point plus its concrete Vec return",
    );

    let actual = Type::Function {
        params: vec![FunctionParamContract {
            name: "value".to_string(),
            ty: Type::named("int32"),
            passing: ReceiverKind::BorrowMut,
            has_default: true,
            default_erased: false,
        }],
        return_type: Box::new(Type::Named("list".to_string(), vec![Type::named("str")])),
    };
    let type_params = BTreeSet::from(["T".to_string(), "U".to_string()]);
    let mut substitutions = HashMap::new();
    assert!(type_pattern_matches(
        &pattern,
        &actual,
        &type_params,
        &mut substitutions,
    ));
    assert_eq!(substitutions.get("T"), Some(&Type::named("int32")));
    assert_eq!(substitutions.get("U"), Some(&Type::named("str")));
    assert!(
        !type_pattern_matches(
            &pattern,
            &Type::named("str"),
            &type_params,
            &mut HashMap::new(),
        ),
        "a function pattern does not match a non-callable value",
    );

    let non_callable = unify_type_pattern(&pattern, &Type::named("str"), &mut HashMap::new())
        .expect_err("function unification rejects a non-callable actual type");
    assert_eq!(
        non_callable.message,
        "expected `def(mut T) -> list[U]`, found `str`",
    );

    let wrong_capability = Type::Function {
        params: vec![FunctionParamContract {
            name: "value".to_string(),
            ty: Type::named("int32"),
            passing: ReceiverKind::Value,
            has_default: false,
            default_erased: false,
        }],
        return_type: Box::new(Type::Named("list".to_string(), vec![Type::named("str")])),
    };
    let mismatch = unify_type_pattern(&pattern, &wrong_capability, &mut HashMap::new())
        .expect_err("function unification preserves parameter capabilities");
    assert!(mismatch
        .message
        .contains("function parameter 1 has `own` capability"));
    assert!(mismatch.message.contains("requires `mut`"));

    let wrong_arity = Type::Function {
        params: Vec::new(),
        return_type: Box::new(Type::Named("list".to_string(), vec![Type::named("str")])),
    };
    let mismatch = unify_type_pattern(&pattern, &wrong_arity, &mut HashMap::new())
        .expect_err("function unification preserves callable arity");
    assert_eq!(
        mismatch.message,
        "expected `def(mut T) -> list[U]`, found `def() -> list[str]`",
    );

    assert!(
        rng_clone_obligation_params_in_context_with_modules(
            &pattern,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .is_empty(),
        "a capture-free code pointer does not clone values of its parameter or return types",
    );
}

#[test]
fn function_value_diagnostics_distinguish_lowering_capability_and_call_contract_failures() {
    for (surface, source) in [
        (
            "parameter",
            "def apply(callback: def(list[int32, str]) -> int32):\n    pass\n",
        ),
        (
            "return",
            "def apply(callback: def(int32) -> list[int32, str]):\n    pass\n",
        ),
    ] {
        let error =
            crate::check_source(source).expect_err("nested function types enforce type arity");
        assert_eq!(error.code, "AU2002", "{surface}: {error:?}");
        assert!(
            error
                .message
                .contains("`list` expects exactly one type argument"),
            "{surface}: {error:?}",
        );
    }

    let reassignment = crate::check_source(
        r#"
def consume(value: own str) -> str:
    return value

def inspect(value: str) -> str:
    return value.clone()

def main():
    mut callback = consume
    callback = inspect
"#,
    )
    .expect_err("reassignment cannot change a function parameter capability");
    assert_eq!(reassignment.code, "AU2002");
    assert!(reassignment
        .message
        .contains("function parameter 1 has `shared` capability"));
    assert!(reassignment.message.contains("requires `own`"));

    let member_specialization = crate::check_source(
        r#"
class Holder:
    callback: def(int32) -> int32

def identity(value: int32) -> int32:
    return value

def main():
    holder = Holder(callback=identity)
    result: int32 = holder.callback[int32](1)
"#,
    )
    .expect_err("stored function values already have a concrete signature");
    assert_eq!(member_specialization.code, "AU2005");
    assert!(member_specialization
        .message
        .contains("function values have a concrete signature"));

    let non_callable = crate::check_source("def main():\n    value = 1\n    value()\n")
        .expect_err("a local integer is not callable");
    assert_eq!(non_callable.code, "AU2999");
    assert_eq!(non_callable.message, "unsupported call target");

    let required = crate::check_source(
        r#"
def required(value: int32) -> int32:
    return value

def main():
    callback = required
    callback()
"#,
    )
    .expect_err("an originally required function-value parameter stays required");
    assert_eq!(required.code, "AU2004");
    assert!(required
        .message
        .contains("missing required argument `value`"));
}

#[test]
fn contextual_generic_function_values_report_each_specialization_failure_at_the_value() {
    let capability = crate::check_source(
        r#"
def identity[T](value: own T) -> T:
    return value

def main():
    callback: def(mut str) -> str = identity
"#,
    )
    .expect_err("contextual generic specialization preserves capabilities");
    assert_eq!(capability.code, "AU2002");
    assert!(capability
        .message
        .contains("function parameter 1 has `own` capability"));
    assert!(capability.message.contains("requires `mut`"));

    let parameter = crate::check_source(
        r#"
def first[T](values: own list[T]) -> list[T]:
    return values

def main():
    callback: def(own set[int32]) -> list[int32] = first
"#,
    )
    .expect_err("the expected parameter shape must specialize the generic declaration");
    assert_eq!(parameter.code, "AU2002");
    assert!(parameter
        .message
        .contains("cannot specialize function `first`"));
    assert!(parameter
        .message
        .contains("expected `list[T]`, found `set[int32]`"));

    let return_type = crate::check_source(
        r#"
def identity[T](value: own T) -> T:
    return value

def main():
    callback: def(own int32) -> str = identity
"#,
    )
    .expect_err("parameter and return evidence must agree on one specialization");
    assert_eq!(return_type.code, "AU2002");
    assert!(return_type
        .message
        .contains("cannot specialize function `identity`"));
    assert!(return_type
        .message
        .contains("conflicting inferred types for `T`: `int32` and `str`"));

    let unresolved = crate::check_source(
        r#"
def marker[T](value: int32) -> int32:
    return value

def main():
    callback: def(int32) -> int32 = marker
"#,
    )
    .expect_err("unused generic parameters cannot be invented from context");
    assert_eq!(unresolved.code, "AU2002");
    assert!(unresolved
        .message
        .contains("cannot infer type parameter `T` for function `marker`"));

    let arity = crate::check_source(
        r#"
def identity[T](value: own T) -> T:
    return value

def main():
    callback = identity[int32, str]
"#,
    )
    .expect_err("explicit specialization supplies every type argument exactly once");
    assert_eq!(arity.code, "AU2002");
    assert!(arity
        .message
        .contains("function `identity` expects 1 type argument, found 2"));

    crate::check_source(
        r#"
trait Marker:
    def marker(self) -> int32

class Tagged:
    value: int32

impl Marker for Tagged:
    def marker(self) -> int32:
        return self.value

def identity[T: Marker](value: own T) -> T:
    return value

def main():
    callback = identity[Tagged]
    tagged = callback(Tagged(value=7))
"#,
    )
    .expect("explicit function-value specialization enforces and accepts trait bounds");

    let bound = crate::check_source(
        r#"
trait Marker:
    def marker(self) -> int32

def identity[T: Marker](value: own T) -> T:
    return value

def main():
    callback = identity[int32]
"#,
    )
    .expect_err("explicit function-value specialization rejects an unsatisfied trait bound");
    assert_eq!(bound.code, "AU2002");
    assert!(bound
        .message
        .contains("type `int32` does not implement trait `Marker`"));
}

#[test]
fn imported_generic_function_values_specialize_as_values_and_task_targets() {
    let mut identity_decl = unary_function_decl("identity");
    identity_decl.type_params = vec!["T".to_string()];
    identity_decl.params[0].ty = type_ref("T");
    identity_decl.return_type = type_ref("T");
    let identity = FunctionInfo {
        module_name: "pkg.tools".to_string(),
        decl: identity_decl,
        signature: FunctionSignature {
            params: vec![Type::TypeParam("T".to_string())],
            param_passings: vec![ReceiverKind::Value],
            return_type: Type::TypeParam("T".to_string()),
            rng_clone_safe_type_params: BTreeSet::new(),
            array_equality_safe_type_params: BTreeSet::new(),
        },
        type_param_bounds: BTreeMap::new(),
    };
    let mut marker_decl = unary_function_decl("marker");
    marker_decl.type_params = vec!["Item".to_string(), "T".to_string()];
    marker_decl.params[0].ty = type_ref("T");
    marker_decl.return_type = type_ref("Item");
    let marker = FunctionInfo {
        module_name: "pkg.tools".to_string(),
        decl: marker_decl,
        signature: FunctionSignature {
            params: vec![Type::TypeParam("T".to_string())],
            param_passings: vec![ReceiverKind::Value],
            return_type: Type::TypeParam("Item".to_string()),
            rng_clone_safe_type_params: BTreeSet::new(),
            array_equality_safe_type_params: BTreeSet::new(),
        },
        type_param_bounds: BTreeMap::new(),
    };
    let namespace = ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "tools".to_string(),
        path: "pkg.tools".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: BTreeMap::from([
            ("identity".to_string(), identity.clone()),
            ("marker".to_string(), marker.clone()),
        ]),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::from([
            ("identity".to_string(), identity),
            ("marker".to_string(), marker),
        ]),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    };
    let module = crate::parser::parse(
        r#"
def main():
    callback = tools.identity[int32]
    value: int32 = callback(1)
    direct: int32 = tools.identity[int32](3)
    marker = tools.marker[int32, str]("ready")
    with group = TaskGroup():
        task: Task[int32] = group.start(tools.identity[int32], 2)
"#,
    )
    .expect("imported generic function-value source should parse");
    check_with_context(
        module,
        ModuleContext {
            module_name: "<main>".to_string(),
            imported_bindings: BTreeMap::from([(
                "tools".to_string(),
                ImportedBinding::Module(namespace.clone()),
            )]),
            module_registry: BTreeMap::from([("pkg.tools".to_string(), namespace)]),
            is_entry_module: true,
        },
    )
    .expect("qualified generic functions specialize both as values and task targets");
}

#[test]
fn function_types_are_transfer_and_do_not_create_task_observation_rights() {
    crate::check_source(
        r#"
class Holder:
    callback: def(int32) -> int32

def identity(value: int32) -> int32:
    return value

def apply(callback: own def(int32) -> int32, value: int32) -> int32:
    return callback(value)

def clone_holders(values: list[Holder]) -> list[Holder]:
    return values.copy()

def clone_tasks(
    tasks: list[Task[def(int32) -> int32]]
) -> list[Task[def(int32) -> int32]]:
    return tasks.copy()

def launch():
    with group = TaskGroup():
        task: Task[int32] = group.start(apply, identity, 1)
"#,
    )
    .expect(
        "capture-free callbacks cross task boundaries and stay repeatable inside holders and Task results",
    );
}

#[test]
fn function_value_task_targets_preserve_call_and_transfer_failure_diagnostics() {
    let call_mismatch = crate::check_source(
        r#"
def worker(value: int32) -> int32:
    return value

def launch():
    target = worker
    with group = TaskGroup():
        group.start(target, "wrong")
"#,
    )
    .expect_err("an indirect task target still checks its concrete argument types");
    assert_eq!(call_mismatch.code, "AU2002");
    assert!(call_mismatch
        .message
        .contains("argument type mismatch for function value: expected `int32`, found `str`"));

    let argument = crate::check_source(
        r#"
import random

def use_rng(value: own random.Rng) -> int32:
    return 1

def launch():
    target = use_rng
    with group = TaskGroup():
        group.start(target, random.Rng(seed=7))
"#,
    )
    .expect_err("a function-value task argument must be Transfer");
    assert_eq!(argument.code, "AU3008");
    assert!(argument
        .message
        .contains("task argument 1 for function value"));
    assert!(argument
        .message
        .contains("`random.Rng` is a stateful generator and is not Transfer"));

    let result = crate::check_source(
        r#"
import random

def make_rng() -> random.Rng:
    return random.Rng(seed=7)

def launch():
    target = make_rng
    with group = TaskGroup():
        group.start(target)
"#,
    )
    .expect_err("a function-value task result must be Transfer");
    assert_eq!(result.code, "AU3008");
    assert!(result
        .message
        .contains("task result `random.Rng` cannot cross a task boundary"));
}

#[test]
fn function_value_specialization_enforces_clone_safety_at_value_creation() {
    let rejected = crate::check_source(
        r#"
import random

def duplicate[T](values: list[T]) -> list[T]:
    return values.copy()

def main():
    callback = duplicate[random.Rng]
"#,
    )
    .expect_err("specializing a function value must enforce its inferred clone-safety contract");
    assert_eq!(rejected.code, "AU3007");
    assert!(rejected.message.contains("function `duplicate`"));
    assert!(rejected.message.contains("non-cloneable `random.Rng`"));
}

#[test]
fn vec_algorithm_methods_infer_callback_results_and_accept_existing_order_surface() {
    crate::check_source(
        r#"
trait Ord[Rhs]:
    def lt(self, rhs: Rhs) -> bool

class Score:
    value: int32

impl Ord[Score] for Score:
    def lt(self, rhs: Score) -> bool:
        return self.value < rhs.value

def label(value: int64) -> str:
    return value.to_string()

def positive(value: int64) -> bool:
    return value > 0

def score_key(value: Score) -> int32:
    return value.value

def ratio_key(value: int64) -> float64:
    return value.to_float()

def identity(value: int64) -> int64:
    return value

def map_values[T, U](values: list[T], f: def(T) -> U) -> list[U]:
    return values.map(f)

def sort_values[T: Ord[T]](values: mut list[T]):
    values.sort()

def main():
    mut numbers = [3, 1, 2]
    numbers.sort()
    numbers.sort(key=identity)
    numbers.sort(key=ratio_key)
    labels: list[str] = numbers.map(label)
    generic_labels: list[str] = map_values(numbers, label)
    kept: list[int64] = numbers.filter(positive)

    mut durations = [2ms, 1ms]
    durations.sort()

    mut ratios = [2.0, 1.0]
    ratios.sort()

    mut scores = [Score(value=2), Score(value=1)]
    scores.sort()
    scores.sort(key=score_key)
    sort_values(scores)
"#,
    )
    .expect("Vec algorithms should infer callback output and use the existing ordering surface");
}

#[test]
fn vec_algorithm_methods_enforce_arity_capability_returns_mutability_and_orderability() {
    for (source, expected) in [
        (
            "def main():\n    values = [2, 1]\n    values.sort()\n",
            "requires a mutable receiver",
        ),
        (
            "def own_key(value: own int64) -> int64:\n    return value\n\ndef main():\n    mut values = [2, 1]\n    values.sort(key=own_key)\n",
            "must take exactly one shared parameter",
        ),
        (
            "def mut_map(value: mut int64) -> int64:\n    return value\n\ndef main():\n    values = [1]\n    print(values.map(mut_map))\n",
            "must take exactly one shared parameter",
        ),
        (
            "def no_args() -> int64:\n    return 1\n\ndef main():\n    values = [1]\n    print(values.map(no_args))\n",
            "must take exactly one shared parameter",
        ),
        (
            "def number(value: int64) -> int64:\n    return value\n\ndef main():\n    values = [1]\n    print(values.filter(number))\n",
            "must return `bool`",
        ),
        (
            "def main():\n    mut values = [[1], [2]]\n    values.sort()\n",
            "cannot order list element type",
        ),
        (
            "def key(value: int64) -> list[int64]:\n    return [value]\n\ndef main():\n    mut values = [2, 1]\n    values.sort(key=key)\n",
            "cannot order key type",
        ),
    ] {
        let rejected =
            crate::check_source(source).expect_err("invalid Vec algorithm call should fail");
        assert!(
            rejected.message.contains(expected),
            "{source}: expected `{expected}`, got {rejected:?}"
        );
    }

    for source in [
        "def key(value: int64) -> int64:\n    return value\n\ndef main():\n    values = [1]\n    values.map()\n",
        "def key(value: int64) -> int64:\n    return value\n\ndef main():\n    values = [1]\n    values.filter(key, key)\n",
    ] {
        let rejected =
            crate::check_source(source).expect_err("Vec callbacks have exactly one argument");
        assert!(
            rejected.message.contains("argument"),
            "{source}: {rejected:?}"
        );
    }
}

#[test]
fn vec_filter_requires_clone_safe_retained_elements() {
    let rejected = crate::check_source(
        r#"
import random

def keep(value: random.Rng) -> bool:
    return true

def main():
    values = [random.Rng(seed=1)]
    print(values.filter(keep))
"#,
    )
    .expect_err("filter must clone retained elements rather than transfer them");
    assert_eq!(rejected.code, "AU3007");
    assert!(rejected.message.contains("list.filter"));
    assert!(rejected.message.contains("non-cloneable `random.Rng`"));

    let generic = crate::check_source(
        r#"
import random

def retain[T](values: list[T], predicate: def(T) -> bool) -> list[T]:
    return values.filter(predicate)

def keep(value: random.Rng) -> bool:
    return true

def main():
    values = [random.Rng(seed=1)]
    print(retain(values, keep))
"#,
    )
    .expect_err("filter clone safety should propagate through a generic helper");
    assert_eq!(generic.code, "AU3007");
    assert!(generic.message.contains("function `retain`"));
}

#[test]
fn owned_vec_and_string_slices_preserve_result_types_and_sources() {
    crate::check_source(
        r#"
def retain_slice[T](values: list[T], start: int32, end: int32) -> list[T]:
    return values[start:end]

def main():
    values: list[int32] = [1, 2, 3, 4]
    prefix: list[int32] = values[:2]
    suffix: list[int32] = values[1:]
    middle: list[int32] = values[1:3]
    copy: list[int32] = values[:]
    empty: list[int32] = [][:]
    names: list[str] = ["Ada", "Grace"]
    names_copy: list[str] = names[:]
    text: str = "Aé🙂Z"
    text_prefix: str = text[:2]
    text_suffix: str = text[-2:]
    text_middle: str = text[1:3]
    text_copy: str = text[:]
    generic: list[int32] = retain_slice(values, 0, 2)
    print(values.len())
    print(text.len())
    print(prefix.len() + suffix.len() + middle.len() + copy.len() + empty.len() + generic.len())
    print(names.len() + names_copy.len())
    print(text_prefix + text_suffix + text_middle + text_copy)
"#,
    )
    .expect("owned Vec and str slices should retain their source and preserve its type");
}

#[test]
fn owned_slice_endpoints_use_the_int64_index_domain() {
    for (source, found) in [
        (
            "def invalid(values: list[int32], endpoint: bool):\n    print(values[:endpoint])\n",
            "bool",
        ),
        (
            "def invalid(text: str, endpoint: str):\n    print(text[endpoint:])\n",
            "str",
        ),
        (
            "def invalid(values: list[int32], endpoint: uint64):\n    print(values[endpoint:])\n",
            "uint64",
        ),
    ] {
        let rejected =
            crate::check_source(source).expect_err("slice endpoints must enter the int64 domain");
        assert_eq!(rejected.code, "AU2002");
        assert!(
            rejected.message.contains(&format!(
                "slice endpoints must have type `int64` or a losslessly narrower integer type, found `{found}`"
            )),
            "{source}: {rejected:?}"
        );
    }

    crate::check_source(
        r#"
def main():
    values: list[int32] = [1, 2, 3]
    text = "Aé🙂Z"
    print(values[0:2])
    print(values[-2:-1])
    print(text[1:3])
"#,
    )
    .expect("integer literals in slice endpoints should receive int64 context");

    let overflow = crate::check_source(
        "def main():\n    values = [1]\n    print(values[9223372036854775808:])\n",
    )
    .expect_err("slice endpoint literals must fit int64");
    assert_eq!(overflow.code, "AU2999");
    assert!(
        overflow.message.contains("int64"),
        "overflowing endpoint should name the int64 boundary: {overflow:?}"
    );
}

#[test]
fn owned_slices_reject_unsupported_bases_and_string_scalar_indexing_remains_unavailable() {
    for source in [
        "def main():\n    value: int32 = 1\n    print(value[:])\n",
        "def main():\n    values = {1, 2}\n    print(values[:])\n",
        "def main():\n    values = {\"one\": 1}\n    print(values[:])\n",
        "def main():\n    values = (1, 2)\n    print(values[:])\n",
    ] {
        let rejected =
            crate::check_source(source).expect_err("only list and str support owned slicing");
        assert_eq!(rejected.code, "AU2003");
        assert!(
            rejected
                .message
                .contains("owned slicing is available only for `Array[T]`, `list[T]`, and `str`"),
            "{source}: {rejected:?}"
        );
    }

    let indexed = crate::check_source("def main():\n    print(\"text\"[0])\n")
        .expect_err("str scalar indexing remains unavailable");
    assert!(indexed.message.contains("cannot index"));
}

#[test]
fn vec_slices_require_structurally_clone_safe_elements() {
    let rng = crate::check_source(
        r#"
import random

def duplicate(values: list[random.Rng]) -> list[random.Rng]:
    return values[:]
"#,
    )
    .expect_err("a list slice must not duplicate random.Rng state");
    assert_eq!(rng.code, "AU3007");
    assert!(rng.message.contains("list slice"));
    assert!(rng.message.contains("non-cloneable `random.Rng`"));

    let transitive = crate::check_source(
        r#"
import random

class Holder:
    generator: random.Rng

def duplicate(values: list[Holder]) -> list[Holder]:
    return values[1:]
"#,
    )
    .expect_err("list slice clone safety must be structural");
    assert_eq!(transitive.code, "AU3007");
    assert!(transitive.message.contains("list slice"));

    let task = crate::check_source(
        r#"
def duplicate(values: list[Task[str]]) -> list[Task[str]]:
    return values[:]
"#,
    )
    .expect_err("a list slice must not duplicate a non-repeatable Task observation right");
    assert_eq!(task.code, "AU3009");
    assert!(task.message.contains("list slice"));
    assert!(
        task.message.contains("non-repeatable task result `str`"),
        "{task:?}"
    );

    let generic = crate::check_source(
        r#"
import random

def duplicate[T](values: list[T]) -> list[T]:
    return values[:]

def main():
    values = [random.Rng(seed=1)]
    print(duplicate(values))
"#,
    )
    .expect_err("generic Vec slicing must propagate its clone-safety obligation");
    assert_eq!(generic.code, "AU3007");
    assert!(generic.message.contains("function `duplicate`"));

    let consuming_closure = crate::check_source(
        r#"
def singleton[T](value: own T) -> list[T]:
    return [value]

def take(value: own str) -> str:
    return value

def main():
    token = "secret"
    callback = lambda: take(token)
    callbacks = singleton(callback)
    copies = callbacks[:]
    print(copies.len())
"#,
    )
    .expect_err("generic construction must not let a list slice duplicate a closure environment");
    assert_eq!(consuming_closure.code, "AU3007");
    assert!(consuming_closure.message.contains("list slice"));
    assert!(
        consuming_closure.message.contains("non-cloneable closure"),
        "{consuming_closure:?}"
    );
}

#[test]
fn slice_base_is_retained_through_both_endpoint_expressions() {
    for source in [
        r#"
def consume(values: own list[int32]) -> int32:
    return 0

def invalid(values: own list[int32]) -> list[int32]:
    return values[consume(values):]
"#,
        r#"
def consume(values: own list[int32]) -> int32:
    return 0

def invalid(values: own list[int32]) -> list[int32]:
    return values[:consume(values)]
"#,
        r#"
def mutate(values: mut list[int32]) -> int32:
    values.clear()
    return 0

def invalid(values: mut list[int32]) -> list[int32]:
    return values[mutate(values):]
"#,
        r#"
def mutate(values: mut list[int32]) -> int32:
    values.clear()
    return 0

def invalid(values: mut list[int32]) -> list[int32]:
    return values[:mutate(values)]
"#,
    ] {
        let rejected = crate::check_source(source)
            .expect_err("a slice base must remain shared through both endpoint expressions");
        assert_eq!(rejected.code, "AU3002");
        assert!(
            rejected.message.contains("slice base"),
            "{source}: {rejected:?}"
        );
    }
}

#[test]
fn slice_walkers_cover_defaults_lambdas_and_nested_expression_positions() {
    for (source, parameter) in [
        (
            r#"
def invalid(values: list[int32], copy: list[int32] = values[:]):
    pass
"#,
            "values",
        ),
        (
            r#"
def invalid(start: int32, copy: list[int32] = [][start:]):
    pass
"#,
            "start",
        ),
        (
            r#"
def invalid(end: int32, copy: list[int32] = [][:end]):
    pass
"#,
            "end",
        ),
    ] {
        let default = crate::check_source(source)
            .expect_err("a slice default must not hide a reference to another parameter");
        assert_eq!(default.code, "AU2004");
        assert!(
            default.message.contains(&format!(
                "default argument for parameter `copy` may not reference parameter `{parameter}`"
            )),
            "{default:?}"
        );
    }

    let program = crate::check_source(
        r#"
def main():
    values: list[int32] = [1, 2, 3]
    start: int32 = 0
    end: int32 = 2
    nested: list[list[int32]] = [values[:]]
    take: def() -> list[int32] = lambda: values[start:end]
    print(nested)
    print(take())
"#,
    )
    .expect("slice inputs should participate in lambda capture and nested-expression walks");
    let captures = program
        .closures
        .values()
        .flat_map(|closure| closure.captures.iter().map(|capture| capture.name.as_str()))
        .collect::<BTreeSet<_>>();
    assert_eq!(captures, BTreeSet::from(["end", "start", "values"]));
}

#[test]
fn class_field_default_calls_keep_the_callable_boundary_diagnostic() {
    let rejected = crate::check_source(
        r#"
class Settings:
    value: int32 = make_value()

def make_value() -> int32:
    return 7
"#,
    )
    .expect_err("class defaults cannot call a module function before callable registration");
    assert_eq!(rejected.code, "AU2999");
    assert_eq!(rejected.message, "unsupported call target");
}

#[test]
fn lambdas_infer_contextual_parameters_and_record_lexical_capture_metadata() {
    let program = crate::check_source(
        r#"
def inspect(prefix: str, value: int32) -> bool:
    return prefix == "kept" and value > 0

def main():
    prefix = "kept"
    offset: int32 = 2
    render: def(int32) -> bool = lambda value: inspect(prefix, value + offset)
    print(render(1))
    print(render(2))
"#,
    )
    .expect("a read-only captured lambda should retain its closure metadata and be reusable");

    let closure = program
        .closures
        .values()
        .find(|closure| closure.span.line == 8)
        .expect("checked lambda metadata");
    assert_eq!(
        closure
            .captures
            .iter()
            .map(|capture| capture.name.as_str())
            .collect::<Vec<_>>(),
        vec!["prefix", "offset"],
        "captures are ordered by lexical first use, not map order"
    );
    assert_eq!(closure.call_kind, ClosureCallKind::Repeatable);
    assert_eq!(closure.captures[0].mode, ClosureCaptureMode::Move);
    assert_eq!(closure.captures[1].mode, ClosureCaptureMode::Copy);
    assert!(matches!(closure.ty(), Type::Closure { .. }));
}

#[test]
fn closure_type_keeps_compact_runtime_layout_and_stable_serialization() {
    let closure = Type::Closure {
        params: Box::new(vec![FunctionParamContract {
            name: "value".to_string(),
            ty: Type::named("int32"),
            passing: ReceiverKind::Borrow,
            has_default: false,
            default_erased: false,
        }]),
        return_type: Box::new(Type::named("str")),
        captures: Box::new(vec![ClosureCapture {
            name: "prefix".to_string(),
            ty: Type::named("str"),
            mode: ClosureCaptureMode::Move,
            span: Span::new(4, 17),
        }]),
        call_kind: ClosureCallKind::Repeatable,
    };

    assert!(type_contains_named(&closure, "str"));
    assert!(type_reaches_class_through_non_indirect_fields(
        &closure,
        "str",
        &BTreeMap::new(),
        &mut BTreeSet::new(),
    ));
    assert!(type_pattern_specificity(&closure) >= 3);
    assert!(type_pattern_matches(
        &closure,
        &closure,
        &BTreeSet::new(),
        &mut HashMap::new(),
    ));
    assert!(!has_unresolved_type_params(&closure));
    assert!(matches!(
        erase_type_callable_contracts(&closure),
        Type::Closure { .. }
    ));

    assert!(
        std::mem::size_of::<Type>() <= 48,
        "closure metadata must stay indirectly stored so Type does not inflate every runtime collection; actual Type size is {} bytes",
        std::mem::size_of::<Type>()
    );
    assert!(
        std::mem::size_of::<crate::runtime_value::Value>() <= 128,
        "Type growth must not make runtime errors and collection values excessively large; actual Value size is {} bytes",
        std::mem::size_of::<crate::runtime_value::Value>()
    );

    let serialized = serde_json::to_value(&closure).expect("closure type should serialize");
    assert_eq!(
        serialized,
        serde_json::json!({
            "Closure": {
                "params": [{
                    "name": "value",
                    "ty": {"Named": ["int32", []]},
                    "passing": "Borrow",
                    "has_default": false,
                    "default_erased": false
                }],
                "return_type": {"Named": ["str", []]},
                "captures": [{
                    "name": "prefix",
                    "ty": {"Named": ["str", []]},
                    "mode": "Move",
                    "span": {"line": 4, "column": 17}
                }],
                "call_kind": "Repeatable"
            }
        }),
        "boxing closure metadata must remain transparent to the serialized type schema"
    );
    let round_trip: Type =
        serde_json::from_value(serialized).expect("closure type should deserialize");
    assert_eq!(round_trip, closure);
}

#[test]
fn noncopy_capture_moves_at_creation_and_reports_au3001_on_later_use() {
    let diagnostic = crate::check_source(
        r#"
def inspect(value: str) -> bool:
    return value == "captured"

def main():
    token = "captured"
    inspect_later: def() -> bool = lambda: inspect(token)
    print(token)
"#,
    )
    .expect_err("capturing a non-copy value moves it when the lambda is created");
    assert_eq!(diagnostic.code, "AU3001");
    assert_eq!(diagnostic.message, "use of moved value `token`");
    assert_eq!(diagnostic.secondary_spans.len(), 1);
    assert_eq!(
        diagnostic.secondary_spans[0].label,
        "value moved into closure here"
    );
}

#[test]
fn consuming_capture_makes_the_closure_single_use() {
    let diagnostic = crate::check_source(
        r#"
def take(value: own str) -> int32:
    return 1

def main():
    token = "once"
    take_later: def() -> int32 = lambda: take(token)
    first = take_later()
    second = take_later()
"#,
    )
    .expect_err("a closure that consumes a capture must move on its first call");
    assert_eq!(diagnostic.code, "AU3001");
    assert_eq!(diagnostic.message, "use of moved value `take_later`");
}

#[test]
fn shared_parameter_capture_is_rejected_with_clone_or_own_guidance() {
    let diagnostic = crate::check_source(
        r#"
def build(value: str):
    inspect: def() -> str = lambda: value
"#,
    )
    .expect_err("a bare parameter is shared capability and cannot be captured by value");
    assert_eq!(diagnostic.code, "AU3002");
    assert_eq!(
        diagnostic.message,
        "lambda cannot capture shared parameter `value` by value"
    );
    assert!(diagnostic.help.iter().any(|help| {
        help.contains("clone `value` into an owned local") && help.contains("`own str`")
    }));
}

#[test]
fn mutable_access_to_a_capture_is_rejected_until_fnmut_exists() {
    let diagnostic = crate::check_source(
        r#"
def main():
    mut values = ["kept"]
    update: def() -> None = lambda: values.append("new")
"#,
    )
    .expect_err("Phase 6.3 does not define mutable closures");
    assert_eq!(diagnostic.code, "AU3003");
    assert_eq!(
        diagnostic.message,
        "lambda capture `values` cannot be mutably accessed because mutable closures are not supported"
    );
}

#[test]
fn capturing_closure_uses_annotation_as_context_without_erasing_ownership() {
    let program = crate::check_source(
        r#"
def inspect(value: str) -> bool:
    return value == "kept"

def main():
    token = "kept"
    inspect_later: def() -> bool = lambda: inspect(token)
    print(inspect_later())
"#,
    )
    .expect("an immutable local annotation should contextualize but not erase a closure");

    let closure = program
        .closures
        .values()
        .next()
        .expect("closure metadata is retained");
    assert!(matches!(closure.ty(), Type::Closure { .. }));

    let mutable_storage = crate::check_source(
        r#"
def main():
    token = "kept"
    mut inspect_later: def() -> str = lambda: token
"#,
    )
    .expect_err("mutable def storage cannot erase capture ownership metadata");
    assert_eq!(mutable_storage.code, "AU2002");
    assert!(mutable_storage.message.contains("mutable"));
    assert!(mutable_storage.message.contains("capturing closure"));
}

#[test]
fn capture_free_lambda_coerces_to_an_ordinary_copy_function_value() {
    let program = crate::check_source(
        r#"
def main():
    identity: def(int32) -> int32 = lambda value: value
    copy = identity
    print(identity(1))
    print(copy(2))
"#,
    )
    .expect("capture-free lambdas use the existing Copy function-value type");
    let closure = program
        .closures
        .values()
        .next()
        .expect("capture-free lambda metadata is available to lowering");
    assert!(closure.captures.is_empty());
    assert!(matches!(closure.ty(), Type::Function { .. }));
}

#[test]
fn lambda_capture_discovery_respects_match_pattern_shadowing() {
    let program = crate::check_source(
        r#"
def main():
    outer = "outside"
    checks: def(Option[str]) -> bool = lambda choice: match choice:
        case Option.Some(outer): outer == "inside"
        case Option.None: false
    print(checks(Option.Some("inside")))
    print(outer)
"#,
    )
    .expect("match pattern bindings inside a lambda should shadow outer locals");

    let closure = program
        .closures
        .values()
        .next()
        .expect("lambda metadata is available");
    assert!(
        closure.captures.is_empty(),
        "the match-arm binding named `outer` must not capture the outer local"
    );
}

#[test]
fn inferred_collection_storage_does_not_hide_capturing_closure_metadata() {
    let diagnostic = crate::check_source(
        r#"
def main():
    offset: int64 = 2
    callbacks = [lambda: offset]
"#,
    )
    .expect_err("collection inference must not make capturing closures storable");
    assert_eq!(diagnostic.code, "AU2002");
    assert_eq!(
        diagnostic.message,
        "capturing closures cannot be stored in collection literals in this language version"
    );
}

#[test]
fn task_group_start_moves_a_transfer_closure_target_once() {
    let diagnostic = crate::check_source(
        r#"
def main():
    payload = "task"
    worker: def() -> str = lambda: f"{payload}"
    with TaskGroup() as group:
        task = group.start(worker)
        print(task.result_or("missing", timeout=1s))
    print(worker())
"#,
    )
    .expect_err("starting a task must transfer ownership of its closure environment");
    assert_eq!(diagnostic.code, "AU3001");
    assert_eq!(diagnostic.message, "use of moved value `worker`");
}

#[test]
fn branch_expressions_reject_capturing_closure_unions_with_teaching_diagnostics() {
    let conditional = crate::check_source(
        r#"
def main():
    left = "left"
    right = "right"
    selected: def() -> str = (lambda: left) if true else (lambda: right)
"#,
    )
    .expect_err("conditional branches cannot erase distinct closure environments");
    assert_eq!(conditional.code, "AU2002");
    assert_eq!(
        conditional.message,
        "conditional expressions cannot merge capturing closure values in this language version"
    );
    assert!(conditional.help.iter().any(|help| {
        help.contains("call the closure inside each branch")
            && help.contains("capture-free lambdas or named functions")
    }));

    let matched = crate::check_source(
        r#"
def main():
    left = "left"
    right = "right"
    selected = match true:
        case true: lambda: left
        case false: lambda: right
"#,
    )
    .expect_err("match arms cannot erase distinct closure environments");
    assert_eq!(matched.code, "AU2002");
    assert_eq!(
        matched.message,
        "match expressions cannot merge capturing closure values in this language version"
    );
    assert!(matched.help.iter().any(|help| {
        help.contains("call the closure inside each arm")
            && help.contains("capture-free lambdas or named functions")
    }));
}

#[test]
fn vec_callbacks_preserve_repeatable_closure_types_and_diagnose_invalid_values() {
    let program = crate::check_source(
        r#"
def main():
    offset: int64 = 1
    values = [1, 2]
    mapped: list[int64] = values.map(lambda value: value + offset)
    kept: list[int64] = values.filter(lambda value: value > offset)
"#,
    )
    .expect("Vec callbacks should contextualize repeatable capturing lambdas");
    let closures = program.closures.values().collect::<Vec<_>>();
    assert_eq!(closures.len(), 2);
    assert_eq!(closures[0].return_type, Type::named("int64"));
    assert_eq!(closures[1].return_type, Type::named("bool"));
    for closure in closures {
        assert_eq!(closure.call_kind, ClosureCallKind::Repeatable);
        assert_eq!(
            closure
                .captures
                .iter()
                .map(|capture| (capture.name.as_str(), capture.mode))
                .collect::<Vec<_>>(),
            vec![("offset", ClosureCaptureMode::Copy)]
        );
    }

    let consuming = crate::check_source(
        r#"
def take(value: own str) -> int64:
    return 1

def main():
    token = "once"
    values = [1]
    values.map(lambda value: value + take(token))
"#,
    )
    .expect_err("list.map may invoke its callback repeatedly");
    assert_eq!(consuming.code, "AU2002");
    assert_eq!(
        consuming.message,
        "`list.map` callback must be repeatable, found `consuming closure def(int64) -> int64`"
    );

    let non_callable = crate::check_source("def main():\n    values = [1]\n    values.map(1)\n")
        .expect_err("list.map requires a callable value");
    assert_eq!(non_callable.code, "AU2002");
    assert_eq!(
        non_callable.message,
        "`list.map` expects a function value, found `int64`"
    );

    let wrong_element = crate::check_source(
        r#"
def text_length(value: str) -> int64:
    return value.length()

def main():
    values = [1]
    values.map(text_length)
"#,
    )
    .expect_err("Vec callbacks receive the list element by shared capability");
    assert_eq!(wrong_element.code, "AU2002");
    assert_eq!(
        wrong_element.message,
        "`list.map` callback expects shared `int64`, found shared `str`"
    );
}

#[test]
fn lambda_context_diagnostics_pin_inference_arity_and_capability_boundaries() {
    for (source, expected) in [
        (
            "def main():\n    callback = lambda value: value\n",
            "lambda parameter types require an expected `def(...) -> ...` context",
        ),
        (
            "def main():\n    callback: int64 = lambda: 1\n",
            "lambda requires a callable context, found expected type `int64`",
        ),
        (
            "def main():\n    callback: def(int64) -> int64 = lambda: 1\n",
            "lambda expects 0 contextual parameters, but its function type provides 1",
        ),
        (
            "def main():\n    callback: def(own str) -> int64 = lambda value: 1\n",
            "lambda parameter `value` has `shared` capability, but the expected function type requires `own`",
        ),
    ] {
        let diagnostic =
            crate::check_source(source).expect_err("invalid lambda context should be rejected");
        assert_eq!(diagnostic.code, "AU2002", "{diagnostic:?}");
        assert_eq!(diagnostic.message, expected, "{diagnostic:?}");
    }
}

#[test]
fn lambda_capture_discovery_tracks_composite_expressions_in_lexical_order() {
    let program = crate::check_source(
        r#"
class Label:
    number: int64

def main():
    prefix = ">"
    values = [11]
    index: int32 = 0
    fallback: int64 = 0
    label = Label(number=99)
    render: def(bool) -> str = lambda use_value: f"{prefix}{values[index] if use_value else fallback}{label.number}"
"#,
    )
    .expect("capture discovery should walk formatting, conditionals, indexing, and fields");
    let closure = program
        .closures
        .values()
        .next()
        .expect("render lambda metadata");
    assert_eq!(
        closure
            .captures
            .iter()
            .map(|capture| capture.name.as_str())
            .collect::<Vec<_>>(),
        vec!["prefix", "values", "index", "fallback", "label"]
    );
    assert_eq!(
        closure
            .captures
            .iter()
            .map(|capture| capture.mode)
            .collect::<Vec<_>>(),
        vec![
            ClosureCaptureMode::Move,
            ClosureCaptureMode::Move,
            ClosureCaptureMode::Copy,
            ClosureCaptureMode::Copy,
            ClosureCaptureMode::Move,
        ]
    );
    assert_eq!(closure.return_type, Type::named("str"));
}

#[test]
fn lambda_capture_rejects_already_moved_partially_moved_and_shared_local_state() {
    let already_moved = crate::check_source(
        r#"
def main():
    token = "one owner"
    first: def() -> str = lambda: token
    second: def() -> str = lambda: token
"#,
    )
    .expect_err("a second closure cannot capture a value moved into the first");
    assert_eq!(already_moved.code, "AU3001");
    assert_eq!(already_moved.message, "use of moved value `token`");
    assert_eq!(
        already_moved.secondary_spans[0].label,
        "value moved into closure here"
    );

    let partially_moved = crate::check_source(
        r#"
class Pair:
    first: str
    second: str

def take(value: own str):
    pass

def main():
    pair = Pair(first="first", second="second")
    take(pair.first)
    later: def() -> str = lambda: pair.second
"#,
    )
    .expect_err("a closure cannot hide a prior partial move of its environment");
    assert_eq!(partially_moved.code, "AU3001");
    assert_eq!(
        partially_moved.message,
        "cannot capture partially moved value `pair`"
    );

    let shared_local = crate::check_source(
        r#"
def build(value: str):
    alias = value
    later: def() -> str = lambda: alias
"#,
    )
    .expect_err("a shared local alias cannot become an owned closure capture");
    assert_eq!(shared_local.code, "AU3002");
    assert_eq!(
        shared_local.message,
        "lambda cannot capture shared value `alias` by value"
    );
    assert_eq!(
        shared_local.secondary_spans[0].label,
        "shared value `alias` is declared here"
    );
}

#[test]
fn capturing_closure_storage_rejection_is_uniform_across_collection_and_field_surfaces() {
    for source in [
        r#"
def main():
    offset: int64 = 1
    callbacks = {lambda: offset}
"#,
        r#"
def main():
    offset: int64 = 1
    callbacks = {"offset": lambda: offset}
"#,
    ] {
        let diagnostic = crate::check_source(source)
            .expect_err("set and map literals cannot erase a closure environment");
        assert_eq!(diagnostic.code, "AU2002", "{diagnostic:?}");
        assert_eq!(
            diagnostic.message,
            "capturing closures cannot be stored in collection literals in this language version",
            "{diagnostic:?}"
        );
    }

    let field = crate::check_source(
        r#"
class Holder:
    callback: def() -> str

def main():
    token = "field"
    holder = Holder(callback=lambda: token)
"#,
    )
    .expect_err("written field types cannot erase a closure environment");
    assert_eq!(field.code, "AU2002");
    assert_eq!(
        field.message,
        "expected `def() -> str`, found `consuming closure def() -> str`"
    );
    assert!(field.help.iter().any(|help| {
        help.contains("capturing closures cannot be stored in fields")
            && help.contains("named function or a capture-free lambda")
    }));
}

#[test]
fn task_closure_targets_preserve_consuming_types_and_reject_nontransfer_captures() {
    let program = crate::check_source(
        r#"
def take(value: own str) -> str:
    return value

def main():
    token = "task"
    worker: def() -> str = lambda: take(token)
    with TaskGroup() as group:
        task: Task[str] = group.start(worker)
"#,
    )
    .expect("a consuming closure may transfer to one child task and run once");
    let closure = program
        .closures
        .values()
        .next()
        .expect("task closure metadata");
    assert_eq!(closure.call_kind, ClosureCallKind::Consuming);
    assert_eq!(closure.return_type, Type::named("str"));

    let rejected = crate::check_source(
        r#"
import random

def main():
    rng = random.Rng(seed=1)
    with TaskGroup() as group:
        group.start(lambda: rng)
"#,
    )
    .expect_err("an inline task closure cannot transfer an RNG capability");
    assert_eq!(rejected.code, "AU3008");
    assert_eq!(
        rejected.message,
        "task closure target cannot cross a task boundary because capture `rng` of `consuming closure def() -> random.Rng` -> `random.Rng` is a stateful generator and is not Transfer"
    );
    assert_eq!(
        rejected.secondary_spans[0].label,
        "capture `rng` is created here"
    );
}

#[test]
fn lambda_capture_discovery_covers_nested_and_composite_expression_surfaces() {
    let program = crate::check_source(
        r#"
def identity[T](value: own T) -> T:
    return value

def main():
    grouped_value: int64 = 1
    grouped: def() -> int64 = lambda: (grouped_value)

    negative_value: int64 = 2
    negative: def() -> int64 = lambda: -negative_value

    cast_value: int64 = 3
    casted: def() -> float64 = lambda: cast_value as float64

    specialized_value: int64 = 4
    specialized: def() -> int64 = lambda: identity[int64](specialized_value)

    needle: int64 = 5
    membership_values = [5, 6]
    contains: def() -> bool = lambda: needle in membership_values

    tuple_left: int64 = 7
    tuple_right: int64 = 8
    tupled: def() -> (int64, int64) = lambda: (tuple_left, tuple_right)

    list_left: int64 = 9
    list_right: int64 = 10
    listed: def() -> list[int64] = lambda: [list_left, list_right]

    set_left: int64 = 11
    set_right: int64 = 12
    setted: def() -> set[int64] = lambda: {set_left, set_right}

    map_key: int64 = 13
    map_value: int64 = 14
    mapped: def() -> dict[int64, int64] = lambda: {map_key: map_value}

    low: int64 = 15
    high: int64 = 20
    bounded: def(int64) -> bool = lambda candidate: low < candidate < high

    outer: int64 = 99
    tuple_shadow: def((int64, int64)) -> int64 = lambda pair: match pair:
        case (outer, other): outer + other

    nested_values = [1, 2]
    nested_offset: int64 = 3
    nested: def() -> list[int64] = lambda: nested_values.map(
        lambda value: value + nested_offset
    )
"#,
    )
    .expect("every lambda expression surface should retain source-derived capture metadata");

    assert_eq!(program.closures.len(), 13);
    let by_captures = program
        .closures
        .values()
        .map(|closure| {
            (
                closure
                    .captures
                    .iter()
                    .map(|capture| capture.name.clone())
                    .collect::<Vec<_>>(),
                closure,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (captures, return_type) in [
        (vec!["grouped_value"], Type::named("int64")),
        (vec!["negative_value"], Type::named("int64")),
        (vec!["cast_value"], Type::named("float64")),
        (vec!["specialized_value"], Type::named("int64")),
        (vec!["needle", "membership_values"], Type::named("bool")),
        (
            vec!["tuple_left", "tuple_right"],
            Type::Tuple(vec![Type::named("int64"), Type::named("int64")]),
        ),
        (
            vec!["list_left", "list_right"],
            Type::Named("list".to_string(), vec![Type::named("int64")]),
        ),
        (
            vec!["set_left", "set_right"],
            Type::Named("set".to_string(), vec![Type::named("int64")]),
        ),
        (
            vec!["map_key", "map_value"],
            Type::Named(
                "dict".to_string(),
                vec![Type::named("int64"), Type::named("int64")],
            ),
        ),
        (vec!["low", "high"], Type::named("bool")),
        (vec![], Type::named("int64")),
        (
            vec!["nested_values", "nested_offset"],
            Type::Named("list".to_string(), vec![Type::named("int64")]),
        ),
        (vec!["nested_offset"], Type::named("int64")),
    ] {
        let key = captures.into_iter().map(str::to_string).collect::<Vec<_>>();
        let closure = by_captures
            .get(&key)
            .unwrap_or_else(|| panic!("missing closure with captures {key:?}"));
        assert_eq!(closure.return_type, return_type, "{key:?}");
        assert_eq!(closure.call_kind, ClosureCallKind::Repeatable, "{key:?}");
        assert_eq!(program.closure_info(&closure.id), Some(*closure));
    }

    let mut missing_id = program
        .closures
        .keys()
        .next()
        .expect("at least one closure")
        .clone();
    missing_id.column += 10_000;
    assert_eq!(program.closure_info(&missing_id), None);

    let try_program = crate::check_source(
        r#"
def unwrap_result() -> Result[int64, str]:
    outcome: Result[int64, str] = Result.Ok(1)
    unwrap: def() -> int64 = lambda: try outcome
    return Result.Ok(unwrap())

def main():
    result = unwrap_result()
"#,
    )
    .expect("a consuming lambda may propagate a captured Result through `try`");
    let try_closure = try_program
        .closures
        .values()
        .next()
        .expect("try lambda metadata");
    assert_eq!(try_closure.return_type, Type::named("int64"));
    assert_eq!(try_closure.call_kind, ClosureCallKind::Consuming);
    assert_eq!(try_closure.captures[0].name, "outcome");
}

#[test]
fn source_derived_generic_closure_types_preserve_matching_and_clone_safety() {
    let generic_program = crate::check_source(
        r#"
def hold[T](value: own T):
    inspect: def() -> None = lambda: print(value)
    inspect()
"#,
    )
    .expect("a generic owned value can remain borrowed inside its closure environment");
    let generic_closure = generic_program
        .closures
        .values()
        .next()
        .expect("generic closure metadata");
    assert_eq!(generic_closure.call_kind, ClosureCallKind::Repeatable);
    assert_eq!(
        generic_closure.captures[0].ty,
        Type::TypeParam("T".to_string())
    );
    let generic_ty = generic_closure.ty();
    assert_eq!(type_pattern_specificity(&generic_ty), 2);
    assert!(has_unresolved_type_params(&generic_ty));

    let mut collected = BTreeSet::new();
    collect_type_params_from_type(&generic_ty, &mut collected);
    assert_eq!(collected, BTreeSet::from(["T".to_string()]));
    assert_eq!(
        rng_clone_obligation_params_in_context_with_modules(
            &generic_ty,
            &generic_program.classes,
            &generic_program.enums,
            &generic_program.imported_modules,
            &generic_program.module_registry,
        ),
        BTreeSet::from(["T".to_string()])
    );

    let concrete_ty = substitute_type(
        &generic_ty,
        &HashMap::from([("T".to_string(), Type::named("str"))]),
    );
    assert!(!has_unresolved_type_params(&concrete_ty));
    assert_eq!(
        concrete_ty,
        Type::Closure {
            params: Box::new(Vec::new()),
            return_type: Box::new(Type::Unit),
            captures: Box::new(vec![ClosureCapture {
                name: "value".to_string(),
                ty: Type::named("str"),
                mode: ClosureCaptureMode::Move,
                span: generic_closure.captures[0].span,
            }]),
            call_kind: ClosureCallKind::Repeatable,
        }
    );

    let mut matched = HashMap::new();
    assert!(type_pattern_matches(
        &generic_ty,
        &concrete_ty,
        &BTreeSet::from(["T".to_string()]),
        &mut matched,
    ));
    assert_eq!(matched.get("T"), Some(&Type::named("str")));

    let mut unified = HashMap::new();
    unify_type_pattern(&generic_ty, &concrete_ty, &mut unified)
        .expect("closure unification should infer through capture types");
    assert_eq!(unified.get("T"), Some(&Type::named("str")));

    let rng_program = crate::check_source(
        r#"
import random

def inspect(value: random.Rng) -> bool:
    return true

def main():
    rng = random.Rng(seed=1)
    inspect_later: def() -> bool = lambda: inspect(rng)
    print(inspect_later())
"#,
    )
    .expect("shared inspection keeps a captured RNG closure repeatable");
    let rng_ty = rng_program
        .closures
        .values()
        .next()
        .expect("RNG closure metadata")
        .ty();
    assert_eq!(
        rng_clone_safety_in_context_with_modules(
            &rng_ty,
            &rng_program.classes,
            &rng_program.enums,
            &rng_program.imported_modules,
            &rng_program.module_registry,
        ),
        RngCloneSafety::ContainsRng
    );
}

#[test]
fn lambda_defaults_respect_parameter_shadowing_and_reject_outer_parameter_capture() {
    let program = crate::check_source(
        r#"
def apply(
    value: int64,
    callback: def(int64) -> int64 = lambda value: value
) -> int64:
    return callback(value)

def main():
    print(apply(3))
"#,
    )
    .expect("a lambda parameter shadows an enclosing function parameter in a default");
    let closure = program
        .closures
        .values()
        .next()
        .expect("default lambda metadata");
    assert!(closure.captures.is_empty());
    assert_eq!(closure.ty().to_string(), "def(int64) -> int64");

    let diagnostic = crate::check_source(
        r#"
def invalid(
    value: int64,
    callback: def() -> int64 = lambda: value
):
    pass
"#,
    )
    .expect_err("a lambda default must not hide capture of another parameter");
    assert_eq!(
        diagnostic.message,
        "default argument for parameter `callback` may not reference parameter `value`"
    );
}

#[test]
fn callable_equality_is_rejected_uniformly_for_function_values_and_closures() {
    const MESSAGE: &str =
        "callable equality is not supported; compare results or use an explicit discriminant";
    const NOTE: &str =
        "Aura has no identity-equality fallback; equality-dependent operations require a defined value relation";
    let cases = [
        (
            "named function value ==",
            r#"
def identity(value: int64) -> int64:
    return value

def main():
    left = identity
    right = identity
    print(left == right)
"#,
        ),
        (
            "named function value !=",
            r#"
def identity(value: int64) -> int64:
    return value

def main():
    left = identity
    right = identity
    print(left != right)
"#,
        ),
        (
            "capture-free closure ==",
            r#"
def main():
    left: def(int64) -> int64 = lambda value: value
    right: def(int64) -> int64 = lambda value: value
    print(left == right)
"#,
        ),
        (
            "capture-free closure !=",
            r#"
def main():
    left: def(int64) -> int64 = lambda value: value
    right: def(int64) -> int64 = lambda value: value
    print(left != right)
"#,
        ),
        (
            "capturing closure ==",
            r#"
def main():
    offset: int64 = 1
    left: def(int64) -> int64 = lambda value: value + offset
    right: def(int64) -> int64 = lambda value: value + offset
    print(left == right)
"#,
        ),
        (
            "capturing closure !=",
            r#"
def main():
    offset: int64 = 1
    left: def(int64) -> int64 = lambda value: value + offset
    right: def(int64) -> int64 = lambda value: value + offset
    print(left != right)
"#,
        ),
    ];

    for (case, source) in cases {
        let diagnostic =
            crate::check_source(source).expect_err("callable equality must be rejected");
        assert_eq!(diagnostic.code, "AU2008", "{case}: {diagnostic:?}");
        assert_eq!(diagnostic.message, MESSAGE, "{case}: {diagnostic:?}");
        assert_eq!(diagnostic.notes, [NOTE], "{case}: {diagnostic:?}");
    }
}

#[test]
fn equality_dependent_collection_surfaces_reject_callables_and_rng_state() {
    let callable_cases = [
        (
            "list.remove",
            r#"
def reject(values: mut list[def(int64) -> int64], value: def(int64) -> int64):
    values.remove(value)
"#,
        ),
        (
            "list.index",
            r#"
def reject(values: list[def(int64) -> int64], value: def(int64) -> int64):
    print(values.index(value))
"#,
        ),
        (
            "list.count",
            r#"
def reject(values: list[def(int64) -> int64], value: def(int64) -> int64):
    print(values.count(value))
"#,
        ),
        (
            "membership",
            r#"
def reject(values: list[def(int64) -> int64], value: def(int64) -> int64):
    print(value in values)
"#,
        ),
        (
            "set.add",
            r#"
def reject(values: mut set[def(int64) -> int64], value: own def(int64) -> int64):
    values.add(value)
"#,
        ),
        (
            "dict key assignment",
            r#"
def reject(values: mut dict[def(int64) -> int64, int64], key: own def(int64) -> int64):
    values[key] = 1
"#,
        ),
        (
            "transitive class membership",
            r#"
class Handler:
    callback: def(int64) -> int64

def reject(values: list[Handler], value: Handler):
    print(value in values)
"#,
        ),
        (
            "recursive enum dictionary key",
            r#"
enum CallbackChain:
    Done
    Next(indirect CallbackChain)
    Work(def(int64) -> int64)

def reject(values: dict[CallbackChain, int64], key: CallbackChain):
    print(values[key])
"#,
        ),
    ];

    for (operation, source) in callable_cases {
        let diagnostic = crate::check_source(source)
            .expect_err("callable values must not reach identity equality in collections");
        assert_eq!(diagnostic.code, "AU2008", "{operation}: {diagnostic:?}");
        assert!(
            diagnostic.message.contains("does not define equality"),
            "{operation}: {diagnostic:?}"
        );
        assert!(
            diagnostic.message.contains("def(int64) -> int64"),
            "{operation}: {diagnostic:?}"
        );
        assert_eq!(
            diagnostic.notes,
            ["Aura has no identity-equality fallback; equality-dependent operations require a defined value relation"],
            "{operation}: {diagnostic:?}"
        );
    }

    let rng_cases = [
        (
            "direct equality",
            r#"
import random

def reject(left: random.Rng, right: random.Rng):
    print(left == right)
"#,
        ),
        (
            "list.remove",
            r#"
import random

def reject(values: mut list[random.Rng], value: random.Rng):
    values.remove(value)
"#,
        ),
        (
            "list.index",
            r#"
import random

def reject(values: list[random.Rng], value: random.Rng):
    print(values.index(value))
"#,
        ),
        (
            "list.count",
            r#"
import random

def reject(values: list[random.Rng], value: random.Rng):
    print(values.count(value))
"#,
        ),
        (
            "membership",
            r#"
import random

def reject(values: list[random.Rng], value: random.Rng):
    print(value in values)
"#,
        ),
        (
            "set.add",
            r#"
import random

def reject(values: mut set[random.Rng], value: own random.Rng):
    values.add(value)
"#,
        ),
        (
            "dict key assignment",
            r#"
import random

def reject(values: mut dict[random.Rng, int64], key: own random.Rng):
    values[key] = 1
"#,
        ),
        (
            "transitive wrapper membership",
            r#"
import random

class Holder:
    generator: random.Rng

def reject(values: list[Holder], value: Holder):
    print(value in values)
"#,
        ),
    ];

    for (operation, source) in rng_cases {
        let diagnostic = crate::check_source(source)
            .expect_err("random.Rng identity must not reach equality-dependent operations");
        assert_eq!(diagnostic.code, "AU2008", "{operation}: {diagnostic:?}");
        assert!(
            diagnostic.message.contains("does not define equality"),
            "{operation}: {diagnostic:?}"
        );
        assert!(
            diagnostic.message.contains("random.Rng"),
            "{operation}: {diagnostic:?}"
        );
        assert_eq!(
            diagnostic.notes,
            ["Aura has no identity-equality fallback; equality-dependent operations require a defined value relation"],
            "{operation}: {diagnostic:?}"
        );
    }
}

#[test]
fn task_closure_targets_forward_owned_arguments_and_reject_mut_writeback() {
    let program = crate::check_source(
        r#"
def main():
    offset: int64 = 2
    worker: def(int64) -> int64 = lambda value: value + offset
    with TaskGroup() as group:
        task: Task[int64] = group.start(worker, 3)
"#,
    )
    .expect("a repeatable closure may receive an owned child-task argument");
    let worker = program
        .closures
        .values()
        .next()
        .expect("task worker closure");
    assert_eq!(worker.params.len(), 1);
    assert_eq!(worker.params[0].ty, Type::named("int64"));
    assert_eq!(worker.params[0].passing, ReceiverKind::Borrow);
    assert_eq!(worker.return_type, Type::named("int64"));
    assert_eq!(worker.call_kind, ClosureCallKind::Repeatable);

    let mutating = crate::check_source(
        r#"
def main():
    offset: int64 = 2
    worker: def(mut int64) -> None = lambda mut value: print(value + offset)
    with TaskGroup() as group:
        group.start(worker, 3)
"#,
    )
    .expect_err("a child task cannot write through a closure parameter into its caller");
    assert_eq!(mutating.code, "AU3002");
    assert_eq!(
        mutating.message,
        "task starting does not support mutable parameter 1 on a function value; child tasks cannot write back through the starting call frame"
    );
}

#[test]
fn closure_diagnostics_reject_map_key_environment_erasure() {
    let diagnostic = crate::check_source(
        r#"
def main():
    offset: int64 = 7
    callbacks = {(lambda: offset): "captured"}
"#,
    )
    .expect_err("map keys cannot erase a capturing closure environment");
    assert_eq!(diagnostic.code, "AU2002");
    assert_eq!(
        diagnostic.message,
        "capturing closures cannot be stored in collection literals in this language version"
    );
    assert!(diagnostic.help.iter().any(|help| {
        help.contains("keep the closure in an immutable local")
            && help.contains("named function or capture-free lambda")
    }));
}

#[test]
fn closure_diagnostics_reject_explicit_specialization_of_concrete_signatures() {
    let diagnostic = crate::check_source(
        r#"
def main():
    offset: int64 = 7
    callback: def() -> int64 = lambda: offset
    print(callback[int64]())
"#,
    )
    .expect_err("a concrete closure value cannot accept generic type arguments");
    assert_eq!(diagnostic.code, "AU2005");
    assert_eq!(
        diagnostic.message,
        "closure values have a concrete signature and do not take explicit type arguments"
    );
}

#[test]
fn closure_diagnostics_reject_branch_environment_erasure_against_named_functions() {
    let conditional = crate::check_source(
        r#"
def constant() -> int64:
    return 1

def main():
    offset: int64 = 7
    selected = constant if true else (lambda: offset)
"#,
    )
    .expect_err("a conditional cannot erase one branch's closure environment");
    assert_eq!(conditional.code, "AU2002");
    assert_eq!(
        conditional.message,
        "conditional expressions cannot merge capturing closure values in this language version"
    );
    assert!(conditional.help.iter().any(|help| {
        help.contains("call the closure inside each branch")
            && help.contains("capture-free lambdas or named functions")
    }));

    let matched = crate::check_source(
        r#"
def constant() -> int64:
    return 1

def main():
    offset: int64 = 7
    choice: Option[bool] = Option.Some(true)
    selected = match choice:
        case Option.Some(value): constant
        case Option.None: lambda: offset
"#,
    )
    .expect_err("an enum match cannot erase one arm's closure environment");
    assert_eq!(matched.code, "AU2002");
    assert_eq!(
        matched.message,
        "match expressions cannot merge capturing closure values in this language version"
    );
    assert!(matched.help.iter().any(|help| {
        help.contains("call the closure inside each arm")
            && help.contains("capture-free lambdas or named functions")
    }));
}

#[test]
fn closure_diagnostics_reject_inferred_generic_field_environment_erasure() {
    let diagnostic = crate::check_source(
        r#"
class Holder[T]:
    value: T

def main():
    token = "field"
    holder = Holder(value=lambda: token)
"#,
    )
    .expect_err("generic inference cannot hide closure environment erasure in a field");
    assert_eq!(diagnostic.code, "AU2002");
    assert_eq!(
        diagnostic.message,
        "expected `consuming closure def() -> str`, found `consuming closure def() -> str`"
    );
    assert!(diagnostic.help.iter().any(|help| {
        help.contains("capturing closures cannot be stored in fields")
            && help.contains("named function or a capture-free lambda")
    }));
}

#[test]
fn comprehensions_share_bare_loop_types_and_export_progressive_metadata() {
    let program = crate::check_source(
        r#"
def main():
    numbers: list[int32] = [1, 2, 3]
    names = ["a", "b"]
    doubled: list[int32] = [value * 2 for value in numbers if value > 0]
    unique: set[int32] = {value for value in numbers}
    lookup: dict[int64, str] = {index: name.clone() for index, name in enumerate(names)}
    pairs: list[int32] = [left + right for left, right in zip(numbers, numbers)]
    nested: list[int64] = [outer * 10 + inner for outer in range(0, 2) for inner in range(0, outer)]
    jobs = Queue[str]()
    received: list[str] = [job for job in jobs]
"#,
    )
    .expect("every ordinary bare-loop source should work in a comprehension");

    assert_eq!(program.comprehensions.len(), 6);
    let result_types = program
        .comprehensions
        .values()
        .map(|info| info.result_type.clone())
        .collect::<Vec<_>>();
    assert!(result_types.contains(&Type::Named("list".to_string(), vec![Type::named("int32")])));
    assert!(result_types.contains(&Type::Named("set".to_string(), vec![Type::named("int32")])));
    assert!(result_types.contains(&Type::Named(
        "dict".to_string(),
        vec![Type::named("int64"), Type::named("str")]
    )));
    let nested = program
        .comprehensions
        .values()
        .find(|info| info.clauses.len() == 2)
        .expect("nested comprehension metadata");
    assert_eq!(
        nested
            .clauses
            .iter()
            .map(|clause| clause.binding_type.clone())
            .collect::<Vec<_>>(),
        vec![Type::named("int64"), Type::named("int64")]
    );
    let queue = program
        .comprehensions
        .values()
        .find(|info| info.clauses.iter().any(|clause| clause.receive_owned))
        .expect("Queue receive-owned metadata");
    assert_eq!(queue.clauses[0].binding_type, Type::named("str"));
}

#[test]
fn comprehension_filters_targets_and_contextual_results_are_checked_exactly() {
    let wrong_filter =
        crate::check_source("def main():\n    values = [value for value in range(0, 2) if 1]\n")
            .expect_err("comprehension filters require exact bool");
    assert_eq!(wrong_filter.code, "AU2002");
    assert_eq!(
        wrong_filter.message,
        "comprehension filter must have type `bool`, found `int64`"
    );

    let shadow = crate::check_source(
        "def main():\n    value = 1\n    values = [value for value in range(0, 2)]\n",
    )
    .expect_err("comprehension targets must not shadow visible names");
    assert_eq!(
        shadow.message,
        "comprehension binding `value` would shadow an existing name"
    );

    let leak = crate::check_source(
        "def main():\n    values = [value for value in range(0, 2)]\n    print(value)\n",
    )
    .expect_err("comprehension targets must not leak");
    assert_eq!(leak.message, "unknown name `value`");

    crate::check_source("def main():\n    values: list[int32] = [1 for value in range(0, 2)]\n")
        .expect("result expressions should receive their collection annotation as context");
}

#[test]
fn comprehension_ownership_freezes_sources_and_never_hides_clones_or_repeated_moves() {
    let borrowed_element = crate::check_source(
        r#"
def main():
    values = ["one"]
    copied = [value for value in values]
"#,
    )
    .expect_err("shared non-copy elements need an explicit clone");
    assert!(
        borrowed_element.message.contains("cannot move")
            && borrowed_element.message.contains("borrowed")
            && borrowed_element.message.contains("value"),
        "{borrowed_element:?}"
    );

    let shared_result = crate::check_source(
        r#"
def observe(values: list[str]):
    pass

def main():
    names = ["one"]
    observe([name for name in names])
"#,
    )
    .expect_err("a shared observer must not turn comprehension output into a hidden borrow");
    assert!(
        shared_result.message.contains("cannot move")
            && shared_result.message.contains("borrowed")
            && shared_result.message.contains("name"),
        "{shared_result:?}"
    );

    crate::check_source(
        r#"
def main():
    values = ["one"]
    copied: list[str] = [value.clone() for value in values]
"#,
    )
    .expect("an explicit clone should produce owned output");

    let frozen = crate::check_source(
        r#"
def main():
    mut values = [1]
    changed = [values.append(value) for value in values]
"#,
    )
    .expect_err("an active source stays frozen through output evaluation");
    assert!(
        frozen.message.contains("cannot mutate")
            && frozen.message.contains("borrowed for iteration"),
        "{frozen:?}"
    );

    let token_move = crate::check_source(
        r#"
def take(value: own str) -> int64:
    return 1

def main():
    token = "once"
    values = [take(token) for value in range(0, 2)]
"#,
    )
    .expect_err("an outer value cannot be moved by a repeated comprehension body");
    assert_eq!(
        token_move.message,
        "`comprehension` loop body moves `token` and may execute more than once"
    );

    let partial_move = crate::check_source(
        r#"
class Pair:
    left: str
    right: str

def take(value: own str) -> int64:
    return 1

def main():
    pair = Pair(left="left", right="right")
    values = [take(pair.left) for value in range(0, 2)]
"#,
    )
    .expect_err("a repeated comprehension body cannot partially move an outer value");
    assert_eq!(
        partial_move.message,
        "`comprehension` loop body partially moves `pair` and may execute more than once"
    );
}

#[test]
fn comprehension_owned_storage_diagnostics_distinguish_cloneable_and_noncloneable_values() {
    let cloneable = crate::check_source(
        r#"
def main():
    values = ["one"]
    copied = [value for value in values]
"#,
    )
    .expect_err("a comprehension cannot silently clone a shared str element");
    assert_eq!(cloneable.code, "AU3002");
    assert!(cloneable.message.contains("cannot move"), "{cloneable:?}");
    assert_eq!(
        cloneable.help,
        vec![
            "comprehensions store owned values; call `.clone()` on this shared value, or use an explicit consuming loop when the source should be transferred"
                .to_string()
        ]
    );
    assert!(
        !cloneable.edits.is_empty(),
        "clone-safe shared output should retain its machine-applicable clone edit"
    );

    let noncloneable = crate::check_source(
        r#"
import random

def main():
    generators = [random.Rng(seed=1)]
    copied = [generator for generator in generators]
"#,
    )
    .expect_err("a comprehension cannot transfer a shared move-only element");
    assert_eq!(noncloneable.code, "AU3002");
    assert!(
        noncloneable.message.contains("cannot move"),
        "{noncloneable:?}"
    );
    assert_eq!(
        noncloneable.help,
        vec![
            "comprehensions store owned values and cannot transfer this shared non-cloneable value; receive an owned value from a `Queue`, or use an explicit consuming loop"
                .to_string()
        ]
    );
    assert!(
        noncloneable.edits.is_empty(),
        "a non-cloneable output must not offer an unavailable `.clone()` edit"
    );
}

#[test]
fn comprehension_lambda_capture_scopes_follow_adr_0037() {
    let program = crate::check_source(
        r#"
def main():
    values = [1, 2]
    build: def() -> list[int64] = lambda: [value + 1 for value in values]
    immediate: list[int64] = [(lambda: value)() for value in range(0, 2)]
"#,
    )
    .expect("targets stay local while surrounding lambda sources are captured");
    let build = program
        .closures
        .values()
        .find(|closure| {
            closure
                .captures
                .iter()
                .any(|capture| capture.name == "values")
        })
        .expect("enclosing lambda captures the iterable");
    assert_eq!(
        build
            .captures
            .iter()
            .map(|capture| capture.name.as_str())
            .collect::<Vec<_>>(),
        vec!["values"]
    );

    let stored = crate::check_source(
        "def main():\n    callbacks = [lambda: value for value in range(0, 2)]\n",
    )
    .expect_err("capturing closures cannot be stored as comprehension results");
    assert_eq!(stored.code, "AU2002");
    assert_eq!(
        stored.message,
        "capturing closures cannot be stored in collection literals in this language version"
    );

    for source in [
        "def main():\n    callbacks = {lambda: value for value in range(0, 2)}\n",
        "def main():\n    callbacks = {(lambda: value): value for value in range(0, 2)}\n",
        "def main():\n    callbacks = {value: lambda: value for value in range(0, 2)}\n",
    ] {
        let diagnostic = crate::check_source(source)
            .expect_err("set elements and both map positions share closure-storage constraints");
        assert_eq!(diagnostic.code, "AU2002");
        assert_eq!(
            diagnostic.message,
            "capturing closures cannot be stored in collection literals in this language version"
        );
    }

    let shared_target = crate::check_source(
        r#"
def main():
    names = ["one"]
    callbacks = [(lambda: name)() for name in names]
"#,
    )
    .expect_err("ADR-0037 rejects capture of a shared non-Copy comprehension target");
    assert_eq!(shared_target.code, "AU3002");
    assert_eq!(
        shared_target.message,
        "lambda cannot capture shared value `name` by value"
    );
    assert!(shared_target.help.iter().any(|help| {
        help.contains("clone `name` into an owned local")
            && help.contains("declare the enclosing parameter as `own str`")
    }));
}

#[test]
fn comprehension_walkers_cover_default_parameter_references_and_bad_iterables() {
    let default = crate::check_source(
        r#"
def collect(values: list[int64], copied: list[int64] = [value for value in values]):
    pass

def main():
    pass
"#,
    )
    .expect_err("a comprehension default must not hide a parameter dependency");
    assert!(
        default.message.contains("default")
            && default.message.contains("values")
            && default.message.contains("parameter"),
        "{default:?}"
    );

    let unsupported = crate::check_source("def main():\n    values = [value for value in true]\n")
        .expect_err("non-iterable comprehension sources must be rejected");
    assert_eq!(unsupported.code, "AU2002");
    assert_eq!(
        unsupported.message,
        "comprehension iteration requires a `Range`, `Queue[T]`, `list[T]`, or `set[T]` iterable, found `bool`"
    );
}

#[test]
fn comprehensions_in_function_and_field_defaults_retain_lowering_metadata() {
    let program = crate::check_source(
        r#"
class Bucket:
    values: list[int64] = [value * 2 for value in [2, 4]]

def selected(values: own list[int64] = [value * 2 for value in [1, 2, 3]]) -> list[int64]:
    return values

def main():
    print(selected())
    print(Bucket().values)
"#,
    )
    .expect("comprehensions are valid in accepted default-expression positions");

    assert!(
        program.comprehensions.values().any(|info| {
            matches!(
                &info.id.owner,
                ClosureOwner::Function(name) if name == "selected"
            )
        }),
        "the function default needs owner-qualified metadata"
    );
    assert!(
        program
            .comprehensions
            .values()
            .any(|info| info.id.owner == ClosureOwner::TopLevel),
        "the early field-default checker needs to export its metadata"
    );
}

#[test]
fn comprehension_lockstep_sources_reject_non_collection_inputs_with_teaching_diagnostics() {
    for (source, expected_message) in [
        (
            "def main():\n    values = [index for index, value in enumerate(range(0, 2))]\n",
            "`enumerate` requires a `list[T]` or `set[T]` iterable, found `Range`",
        ),
        (
            "def main():\n    values = [left for left, right in zip([1, 2], range(0, 2))]\n",
            "`zip` requires a `list[T]` or `set[T]` iterable, found `Range`",
        ),
    ] {
        let diagnostic = crate::check_source(source)
            .expect_err("lockstep comprehensions must reject non-positioned iterable forms");
        assert_eq!(diagnostic.code, "AU2002");
        assert_eq!(diagnostic.message, expected_message);
        assert_eq!(
            diagnostic.help,
            vec![
                "these comprehension forms read collections by position; use a plain clause for a `Range` or `Queue[T]`"
                    .to_string()
            ]
        );
    }
}

#[test]
fn comprehension_contextual_output_mismatches_name_each_collection_position() {
    for (source, expected_message) in [
        (
            r#"
def main():
    values: list[int32] = ["value" for value in range(0, 2)]
"#,
            "list comprehension result has type `str`, expected `int32`",
        ),
        (
            r#"
def main():
    values: set[int32] = {"value" for value in range(0, 2)}
"#,
            "set comprehension result has type `str`, expected `int32`",
        ),
        (
            r#"
def main():
    values: dict[int32, str] = {"key": "value" for value in range(0, 2)}
"#,
            "map comprehension key has type `str`, expected `int32`",
        ),
        (
            r#"
def main():
    values: dict[int64, int32] = {value: "value" for value in range(0, 2)}
"#,
            "map comprehension value has type `str`, expected `int32`",
        ),
    ] {
        let diagnostic = crate::check_source(source)
            .expect_err("contextual comprehension output types must match their collection slot");
        assert_eq!(diagnostic.code, "AU2002");
        assert_eq!(diagnostic.message, expected_message);
    }
}

#[test]
fn comprehension_default_parameter_dependencies_cover_filters_nested_sources_and_map_outputs() {
    for (source, expected_parameter) in [
        (
            r#"
def collect(limit: int64, copied: list[int64] = [value for value in [1, 2] if value < limit]):
    pass

def main():
    pass
"#,
            "limit",
        ),
        (
            r#"
def collect(limit: int64, copied: list[int64] = [inner for outer in [1, 2] for inner in range(0, limit)]):
    pass

def main():
    pass
"#,
            "limit",
        ),
        (
            r#"
def collect(offset: int64, copied: set[int64] = {value + offset for value in [1, 2]}):
    pass

def main():
    pass
"#,
            "offset",
        ),
        (
            r#"
def collect(offset: int64, copied: dict[int64, int64] = {value + offset: value for value in [1, 2]}):
    pass

def main():
    pass
"#,
            "offset",
        ),
        (
            r#"
def collect(offset: int64, copied: dict[int64, int64] = {value: value + offset for value in [1, 2]}):
    pass

def main():
    pass
"#,
            "offset",
        ),
    ] {
        let diagnostic = crate::check_source(source)
            .expect_err("a comprehension must not hide a default's parameter dependency");
        assert!(
            diagnostic.message.contains("default")
                && diagnostic.message.contains(expected_parameter)
                && diagnostic.message.contains("parameter"),
            "{diagnostic:?}"
        );
    }
}

#[test]
fn comprehension_lambda_capture_collection_covers_filters_sets_and_map_positions() {
    let program = crate::check_source(
        r#"
def main():
    set_values = [1, 2]
    map_values = [1, 2]
    threshold: int64 = 0
    set_offset: int64 = 10
    key_offset: int64 = 20
    value_offset: int64 = 30
    build_set: def() -> set[int64] = lambda: {value + set_offset for value in set_values if value > threshold}
    build_map: def() -> dict[int64, int64] = lambda: {value + key_offset: value + value_offset for value in map_values if value > threshold}
"#,
    )
    .expect("comprehension targets stay local while every surrounding use is captured");

    let capture_sets = program
        .closures
        .values()
        .map(|closure| {
            closure
                .captures
                .iter()
                .map(|capture| capture.name.as_str())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert!(
        capture_sets
            .iter()
            .any(|captures| captures == &vec!["set_values", "threshold", "set_offset"]),
        "{capture_sets:?}"
    );
    assert!(
        capture_sets.iter().any(|captures| {
            captures == &vec!["map_values", "threshold", "key_offset", "value_offset"]
        }),
        "{capture_sets:?}"
    );
    assert!(
        capture_sets
            .iter()
            .all(|captures| !captures.contains(&"value")),
        "the comprehension target must not escape into the lambda capture set: {capture_sets:?}"
    );
}

#[test]
fn comprehension_arguments_preserve_owned_results_and_allow_source_reuse() {
    crate::check_source(
        r#"
def observe(values: list[int64], unique: set[int64], lookup: dict[int64, int64]):
    pass

def main():
    source = [1, 2, 3]
    observe(
        [value for value in source if value > 0],
        {value for value in source if value > 0},
        {value: value + 1 for value in source if value > 0}
    )
    print(source[0])
"#,
    )
    .expect(
        "collection arguments own their results, evaluate every comprehension position, and leave shared sources usable",
    );
}

#[test]
fn comprehension_defaults_find_parameter_dependencies_inside_nested_expression_containers() {
    for (source, parameter) in [
        (
            r#"
def collect(offset: int64, copied: list[int64] = [value for value in ([offset] if true else [1])]):
    pass

def main():
    pass
"#,
            "offset",
        ),
        (
            r#"
def collect(offset: int64, copied: list[int64] = [value for value in ([1] if true else [offset])]):
    pass

def main():
    pass
"#,
            "offset",
        ),
        (
            r#"
def collect(limit: int64, copied: list[int64] = [value for value in [1, 2] if 0 in [limit]]):
    pass

def main():
    pass
"#,
            "limit",
        ),
        (
            r#"
def collect(limit: int64, copied: list[int64] = [value for value in [1, 2] if 0 < limit < 10]):
    pass

def main():
    pass
"#,
            "limit",
        ),
        (
            r#"
def collect(offset: int64, copied: list[dict[str, int64]] = [{"fixed": offset} for value in [1, 2]]):
    pass

def main():
    pass
"#,
            "offset",
        ),
        (
            r#"
def collect(label: str, copied: set[str] = {f"value={label}" for value in [1, 2]}):
    pass

def main():
    pass
"#,
            "label",
        ),
        (
            r#"
def collect(offset: int64, copied: dict[int64, int64] = {int64(offset): value for value in [1, 2]}):
    pass

def main():
    pass
"#,
            "offset",
        ),
        (
            r#"
def collect(offset: int64, copied: dict[int64, int64] = {value: [10, 20][offset] for value in [1, 2]}):
    pass

def main():
    pass
"#,
            "offset",
        ),
        (
            r#"
def collect(offset: int64, copied: list[int64] = [(match true:
    case true: offset
    case false: 0
) for value in [1, 2]]):
    pass

def main():
    pass
"#,
            "offset",
        ),
        (
            r#"
def collect(offset: int64, copied: list[int64] = [(lambda: offset)() for value in [1, 2]]):
    pass

def main():
    pass
"#,
            "offset",
        ),
    ] {
        let diagnostic = crate::check_source(source)
            .expect_err("a nested comprehension expression must not hide a parameter dependency");
        assert_eq!(
            diagnostic.message,
            format!(
                "default argument for parameter `copied` may not reference parameter `{parameter}`"
            ),
            "{source}"
        );
    }
}

#[test]
fn comprehension_default_tuple_targets_shadow_same_named_parameters() {
    crate::check_source(
        r#"
def collect(left: int64, right: int64, copied: list[(int64, int64)] = [(left, right) for left, right in zip([1, 2], [3, 4])]):
    pass

def main():
    collect(10, 20)
"#,
    )
    .expect(
        "tuple comprehension targets are local bindings, so they shadow same-named parameters in the output",
    );
}

#[test]
fn adr0038_views_enforce_overlap_and_shorten_at_last_use() {
    crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def main():
    mut pair = Pair(left=1, right=2)
    view left = pair.left
    print(left)
    pair.left = 3
    view mut right = pair.right
    right = right + 4
    print(pair.left)
    print(pair.right)
"#,
    )
    .expect(
        "a last-used shared view must release before later mutation and sibling loans are disjoint",
    );

    let error = crate::check_source(
        r#"
class Box:
    value: int64

def main():
    mut box = Box(value=1)
    view value = box.value
    box.value = 2
    print(value)
"#,
    )
    .expect_err("a later view use must keep the overlapping source shared-loaned");
    assert_eq!(error.code, "AU3002");
    assert!(error.message.contains("shared view `value`"));

    crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def main():
    mut pair = Pair(left=1, right=2)
    view mut right = pair.right
    right = right + 1
    print(pair.left)
    print(right)
"#,
    )
    .expect("a mutable view permits access through itself and to a proven-disjoint sibling field");

    let blocked_read = crate::check_source(
        r#"
class Box:
    value: int64

def main():
    mut box = Box(value=1)
    view mut value = box.value
    print(box.value)
    value = 2
"#,
    )
    .expect_err("a live mutable view must block overlapping source reads");
    assert_eq!(blocked_read.code, "AU3002");
    assert!(blocked_read.message.contains("cannot read"));
}

#[test]
fn adr0038_returned_views_never_become_owned_values() {
    for source in [
        r#"
def values_view(values: list[int64]) -> view list[int64] from values:
    return view values

def main():
    values = [1]
    copied = values_view(values)
    print(copied)
"#,
        r#"
def values_view(values: list[int64]) -> view list[int64] from values:
    return view values

def sink(value: own list[int64]):
    print(value)

def main():
    values = [1]
    sink(values_view(values))
"#,
        r#"
def values_view(values: list[int64]) -> view list[int64] from values:
    return view values

def main():
    borrower = values_view
    values = [1]
    print(borrower(values))
"#,
    ] {
        let error = crate::check_source(source)
            .expect_err("a returned view must remain a view at every caller boundary");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_reborrow_suspension_and_nested_last_use_are_enforced() {
    crate::check_source(
        r#"
def main():
    mut value = 1
    view mut parent = value
    view mut child = parent
    child = 2
    print(parent)
"#,
    )
    .expect("a mutable parent view is suspended while its child reborrow is live");

    let error = crate::check_source(
        r#"
def main():
    mut values = [1]
    mut update: def(int64) -> None = lambda [mut values] item: values.append(item)
    if true:
        values.append(9)
        update(2)
    print(values)
"#,
    )
    .expect_err("an enclosing statement header must not expire a loan used in its body");
    assert_eq!(error.code, "AU3002");
}

#[test]
fn adr0038_returned_view_capability_trait_dispatch_and_footprints_are_checked() {
    let escalation = crate::check_source(
        r#"
def invalid(value: mut int64) -> view mut int64 from value:
    view shared = value
    return view mut shared
"#,
    )
    .expect_err("returning a shared child as mutable must not escalate capability");
    assert_eq!(escalation.code, "AU3010");

    crate::check_source(
        r#"
trait Project:
    def get(self) -> view int64 from self

class Box:
    value: int64

impl Project for Box:
    def get(self) -> view int64 from self:
        return view self.value

def read[T: Project](item: T) -> view int64 from item:
    view alias = item.get()
    return view alias

def main():
    item = Box(value=7)
    view alias = item.get()
    view forwarded = read(item)
    print(alias)
    print(forwarded)
"#,
    )
    .expect("returned-view contracts must survive concrete and bounded trait dispatch");

    crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def left_view(pair: Pair) -> view int64 from pair:
    return view pair.left

def main():
    mut pair = Pair(left=1, right=2)
    view left = left_view(pair)
    pair.right = 3
    print(left)
"#,
    )
    .expect("one exact returned projection must not lock a disjoint sibling");
}

#[test]
fn adr0038_views_of_copy_values_are_neither_owned_captures_nor_transferable() {
    let captured = crate::check_source(
        r#"
def main():
    mut value = 1
    view mut alias = value
    callback: def() -> int64 = lambda [own alias]: alias
    print(callback())
"#,
    )
    .expect_err("own capture must not snapshot a view of a Copy value");
    assert_eq!(captured.code, "AU3004");

    let transferred = crate::check_source(
        r#"
def main():
    jobs = Queue[int64]()
    value = 1
    view alias = value
    jobs.put(alias)
"#,
    )
    .expect_err("a view of a Copy value is still not Transfer");
    assert_eq!(transferred.code, "AU3008");
}

#[test]
fn adr0038_returned_views_cannot_hide_in_groups_returns_or_aggregates() {
    let cases = [
        r#"
def borrow(values: list[int64]) -> view list[int64] from values:
    return view values

def copied(values: list[int64]) -> list[int64]:
    return borrow(values)
"#,
        r#"
def borrow(values: list[int64]) -> view list[int64] from values:
    return view values

def main():
    values = [1]
    copied = (borrow(values))
"#,
        r#"
def borrow(values: list[int64]) -> view list[int64] from values:
    return view values

def main():
    values = [1]
    stored = (borrow(values),)
"#,
        r#"
class Holder:
    values: list[int64]

def borrow(values: list[int64]) -> view list[int64] from values:
    return view values

def main():
    values = [1]
    holder = Holder(values=borrow(values))
"#,
    ];
    for source in cases {
        let error = crate::check_source(source)
            .expect_err("returned views must not become owned values through syntax wrappers");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_forwarded_footprints_are_complete_and_exact() {
    let incomplete = crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def left(pair: Pair) -> view int64 from pair:
    return view pair.left

def choose(pair: Pair, direct: bool) -> view int64 from pair:
    if direct:
        return view pair.right
    return view left(pair)

def main():
    mut pair = Pair(left=1, right=2)
    view selected = choose(pair, false)
    pair.left = 9
    print(selected)
"#,
    )
    .expect_err("an unresolved forwarded return must not narrow a mixed footprint");
    assert_eq!(incomplete.code, "AU3002");

    crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def inner(pair: Pair) -> view int64 from pair:
    return view pair.left

def outer(pair: Pair) -> view int64 from pair:
    return view inner(pair)

def main():
    mut pair = Pair(left=1, right=2)
    view selected = outer(pair)
    pair.right = 3
    print(selected)
"#,
    )
    .expect("one transitively fixed returned projection must leave its sibling unlocked");
}

#[test]
fn adr0038_nested_dynamic_returned_views_keep_a_conservative_footprint() {
    let error = crate::check_source(
        r#"
class Cell:
    value: int64

class Pair:
    left: Cell
    right: Cell

def choose(pair: mut Pair, left: bool) -> view mut Cell from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def cell_value(cell: mut Cell) -> view mut int64 from cell:
    return view mut cell.value

def main():
    mut pair = Pair(left=Cell(value=1), right=Cell(value=2))
    view mut selected = cell_value(choose(pair, false))
    pair.right.value = 5
    selected = 9
"#,
    )
    .expect_err("a projection after a dynamic returned view must retain the conservative root");
    assert_eq!(error.code, "AU3002");
}

#[test]
fn adr0038_transitive_method_forwarding_keeps_exact_footprints() {
    crate::check_source(
        r#"
trait LeftProject:
    def left_view(self) -> view int64 from self

class Pair:
    left: int64
    right: int64

    def left_view(self) -> view int64 from self:
        return view self.left

    def static_left(value: Pair) -> view int64 from value:
        return view value.left

impl LeftProject for Pair:
    def left_view(self) -> view int64 from self:
        return view self.left

def through_class(pair: Pair) -> view int64 from pair:
    return view pair.left_view()

def through_static(pair: Pair) -> view int64 from pair:
    return view Pair.static_left(pair)

def through_trait[T: LeftProject](value: T) -> view int64 from value:
    return view value.left_view()

def main():
    mut pair = Pair(left=1, right=2)
    view class_value = through_class(pair)
    pair.right = 3
    print(class_value)
    view static_value = through_static(pair)
    pair.right = 4
    print(static_value)
    view trait_value = through_trait(pair)
    pair.right = 5
    print(trait_value)
"#,
    )
    .expect("concrete, static, and trait method forwarding should preserve one exact projection");
}

#[test]
fn adr0038_loop_headers_keep_loans_live_and_if_conditions_end_on_their_edge() {
    for source in [
        r#"
def main():
    mut value = 2
    view alias = value
    while alias > 0:
        value = 0
"#,
        r#"
def main():
    mut values = [1, 2]
    view alias = values
    for value in alias:
        values.append(value)
"#,
    ] {
        let error = crate::check_source(source)
            .expect_err("a loop header is reevaluated, so its source remains loaned in the body");
        assert_eq!(error.code, "AU3002", "{source}: {error:?}");
    }

    crate::check_source(
        r#"
def main():
    mut value = 1
    view alias = value
    if alias == 1:
        value = 2
    print(value)
"#,
    )
    .expect("a non-loop condition's final use ends on the selected control-flow edge");
}

#[test]
fn adr0038_grouped_closure_moves_and_view_task_arguments_retain_authority() {
    let moved = crate::check_source(
        r#"
def main():
    mut values = [1]
    mut first: def(int64) -> None = lambda [mut values] item: values.append(item)
    mut second = (first)
    values.append(9)
    second(2)
"#,
    )
    .expect_err("grouping a closure move must not discard its loan region");
    assert_eq!(moved.code, "AU3002");

    let transferred = crate::check_source(
        r#"
def echo(value: int64) -> int64:
    return value

def main():
    value = 7
    view alias = value
    with TaskGroup() as group:
        task = group.start(echo, alias)
        print(task.result())
"#,
    )
    .expect_err("a direct view cannot cross a task boundary even when its pointee is Copy");
    assert_eq!(transferred.code, "AU3008");
}

#[test]
fn adr0038_shared_children_suspend_parents_and_projected_reborrows_resume_them() {
    let suspended = crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def main():
    mut pair = Pair(left=1, right=2)
    view mut parent = pair
    view child = parent.left
    print(parent.right)
    print(child)
"#,
    )
    .expect_err("any live child reborrow suspends direct access through its mutable parent");
    assert_eq!(suspended.code, "AU3002");

    crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def main():
    mut pair = Pair(left=1, right=2)
    view mut parent = pair
    view mut child = parent.left
    child = 7
    print(parent.left)
"#,
    )
    .expect("a fixed projection from an existing view is a contained reborrow");
}

#[test]
fn adr0038_loan_closure_regions_follow_moves_and_reject_aggregate_escape() {
    let moved = crate::check_source(
        r#"
def main():
    mut values = [1]
    mut first: def(int64) -> None = lambda [mut values] item: values.append(item)
    mut second = first
    values.append(9)
    second(2)
"#,
    )
    .expect_err("moving a loan closure must move its still-live loan region");
    assert_eq!(moved.code, "AU3002");

    let aggregate = crate::check_source(
        r#"
def main():
    mut value = 1
    callback: def() -> int64 = lambda [value]: value
    stored = (callback,)
    value = 2
    print(stored[0]())
"#,
    )
    .expect_err("loan closures cannot escape into tuples");
    assert_eq!(aggregate.code, "AU3010");
}

#[test]
fn adr0038_returned_views_require_declared_origin_provenance() {
    crate::check_source(
        r#"
class User:
    name: str

def name(user: User) -> view str from user:
    return view user.name

def main():
    user = User(name="Ada")
    view name = name(user)
    print(name)
"#,
    )
    .expect("a returned shared view may derive from its declared addressable origin");

    crate::check_source(
        r#"
class Account:
    name: str

    def name_view(self) -> view str from self:
        return view self.name

    def name_view_mut(mut self) -> view mut str from self:
        return view mut self.name

def main():
    mut account = Account(name="Ada")
    view name = account.name_view()
    print(name)
    view mut editable = account.name_view_mut()
    editable = "Grace"
    print(editable)
"#,
    )
    .expect("receiver methods may return shared and mutable views tied to self");

    crate::check_source(
        r#"
class Counter:
    value: int64

class Sink:
    def bump(self, value: mut int64):
        value += 1

def value_mut(counter: mut Counter) -> view mut int64 from counter:
    return view mut counter.value

def bump(value: mut int64):
    value += 1

def main():
    mut counter = Counter(value=1)
    sink = Sink()
    callback: def(mut int64) -> None = bump
    bump(value_mut(counter))
    sink.bump(value_mut(counter))
    callback(value_mut(counter))
    print(counter.value)
"#,
    )
    .expect("a mutable returned view may be immediately reborrowed into a mutable call");

    let error = crate::check_source(
        r#"
class User:
    name: str

def invalid(user: User) -> view str from user:
    local = User(name="local")
    return view local.name
"#,
    )
    .expect_err("a returned view must not escape callee-local storage");
    assert_eq!(error.code, "AU3010");

    crate::check_source(
        r#"
class User:
    name: str

class Picker:
    marker: int64

trait Select:
    def select(self, left: User, right: User) -> view str from left

impl Select for Picker:
    def select(self, first: User, second: User) -> view str from first:
        return view first.name
"#,
    )
    .expect("trait implementations may rename a returned-view origin while preserving its parameter slot");

    let origin_mismatch = crate::check_source(
        r#"
class User:
    name: str

class Picker:
    marker: int64

trait Select:
    def select(self, left: User, right: User) -> view str from left

impl Select for Picker:
    def select(self, first: User, second: User) -> view str from second:
        return view second.name
"#,
    )
    .expect_err("trait implementations must preserve the returned-view origin slot");
    assert!(origin_mismatch
        .message
        .contains("does not match the trait signature"));

    crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def choose_field(pair: Pair, choose_left: bool) -> view int64 from pair:
    if choose_left:
        return view pair.left
    return view pair.right
"#,
    )
    .expect("control flow may select different projections of one declared returned-view origin");
}

#[test]
fn adr0038_explicit_capture_lists_are_exhaustive_and_capability_checked() {
    crate::check_source(
        r#"
def main():
    value = 3
    callback: def(int64) -> int64 = lambda [value] item: value + item
    print(callback(4))
"#,
    )
    .expect("an exhaustive shared-loan capture list must type-check");

    let error = crate::check_source(
        r#"
def main():
    left = 1
    right = 2
    callback: def(int64) -> int64 = lambda [left] item: left + right + item
    print(callback(3))
"#,
    )
    .expect_err("every used outer local must appear in an explicit capture list");
    assert_eq!(error.code, "AU3004");

    let overlap = crate::check_source(
        r#"
def main():
    mut value = 1
    callback: def() -> int64 = lambda [value]: value
    value = 2
    print(callback())
"#,
    )
    .expect_err("a live shared closure capture must keep its source loaned until final use");
    assert_eq!(overlap.code, "AU3002");

    let immutable_call = crate::check_source(
        r#"
def main():
    mut values = [1]
    update: def(int64) -> None = lambda [mut values] item: values.append(item)
    update(2)
"#,
    )
    .expect_err("mutable-repeatable closures require a mutable closure place");
    assert_eq!(immutable_call.code, "AU3003");

    let task_escape = crate::check_source(
        r#"
def main():
    value = 1
    worker: def() -> int64 = lambda [value]: value
    with TaskGroup() as group:
        task = group.start(worker)
        print(task.result_or(-1, timeout=1s))
"#,
    )
    .expect_err("loan closures must not cross a task boundary");
    assert_eq!(task_escape.code, "AU3008");
}

#[test]
fn adr0038_view_return_contract_validation_covers_every_invalid_origin() {
    let cases = [
        (
            "def invalid() -> view int64 from self:\n    return view 1\n",
            "`from self` is valid only on a method with a receiver",
        ),
        (
            "class Box:\n    value: int64\n\n    def invalid(own self) -> view int64 from self:\n        return view self.value\n",
            "an owned `self` receiver cannot be the origin of a returned view",
        ),
        (
            "class Box:\n    value: int64\n\n    def invalid(self) -> view mut int64 from self:\n        return view mut self.value\n",
            "a mutable returned view requires a `mut self` origin",
        ),
        (
            "def invalid(value: int64) -> view int64 from missing:\n    return view value\n",
            "returned-view origin `missing` is not a receiver or parameter",
        ),
        (
            "def invalid(value: own str) -> view str from value:\n    return view value\n",
            "owned parameter `value` cannot be the origin of a returned view",
        ),
        (
            "def invalid(value: int64 = 1) -> view int64 from value:\n    return view value\n",
            "defaulted parameter `value` cannot be the origin of a returned view",
        ),
        (
            "def invalid(value: int64) -> view mut int64 from value:\n    return view mut value\n",
            "a mutable returned view requires mutable origin parameter `value`",
        ),
    ];
    for (source, expected) in cases {
        let error = crate::check_source(source).expect_err(expected);
        assert_eq!(
            error.code, "AU3010",
            "expected `{expected}`, found `{}`",
            error.message
        );
        assert_eq!(error.message, expected);
    }
}

#[test]
fn adr0038_view_binding_diagnostics_cover_kind_place_and_shadowing_errors() {
    let cases = [
        (
            "def main():\n    value = 1\n    view value = value\n",
            "view binding `value` would shadow an existing name",
        ),
        (
            "def main():\n    view value = 1\n",
            "a view source must be an addressable local, parameter, receiver, fixed field, tuple position, or existing view",
        ),
        (
            "def main():\n    value = 1\n    view mut editable = value\n",
            "mutable view source `value` is not mutable",
        ),
        (
            "def main():\n    mut value = 1\n    view shared = value\n    view mut editable = shared\n",
            "a shared view cannot be escalated to a mutable view",
        ),
        (
            "def borrowed(value: int64) -> view int64 from value:\n    return view value\n\ndef main():\n    value = 1\n    view mut mismatch = borrowed(value)\n",
            "a returned view must initialize a view binding with the same shared or mutable kind",
        ),
        (
            "def borrowed(value: mut int64) -> view mut int64 from value:\n    return view mut value\n\ndef main():\n    mut value = 1\n    view mismatch = borrowed(value)\n",
            "a returned view must initialize a view binding with the same shared or mutable kind",
        ),
    ];
    for (source, expected) in cases {
        let error = crate::check_source(source).expect_err(expected);
        assert_eq!(error.message, expected);
    }
}

#[test]
fn adr0038_return_statement_diagnostics_cover_contract_and_place_mismatches() {
    let cases = [
        (
            "def invalid(value: int64) -> view int64 from value:\n    return value\n",
            "this function must return a shared view from `value`",
        ),
        (
            "def invalid(value: mut int64) -> view mut int64 from value:\n    return view value\n",
            "this function must return a mutable view from `value`",
        ),
    ];
    for (source, expected) in cases {
        let error = crate::check_source(source).expect_err(expected);
        assert_eq!(
            error.code, "AU3010",
            "expected `{expected}`, found `{}`",
            error.message
        );
        assert_eq!(error.message, expected);
    }
}

#[test]
fn adr0038_tuple_view_places_reject_dynamic_negative_and_out_of_range_positions() {
    let cases = [
        (
            "def main():\n    pair = (1, 2)\n    index = 0\n    view value = pair[index]\n",
            "a tuple view requires a fixed integer position",
        ),
        (
            "def main():\n    pair = (1, 2)\n    view value = pair[-1]\n",
            "a tuple view requires a fixed integer position",
        ),
        (
            "def main():\n    pair = (1, 2)\n    view value = pair[2]\n",
            "tuple has no position 2",
        ),
    ];
    for (source, expected) in cases {
        let error = crate::check_source(source).expect_err(expected);
        assert_eq!(error.code, "AU3004");
        assert_eq!(error.message, expected);
    }
}

#[test]
fn adr0038_last_use_walkers_retain_slice_comparison_and_nested_block_end_spans() {
    for expression in [
        "values[needle:needle]",
        "1 < needle < needle",
        "assert true, str(needle)",
        "{needle: needle for item in [1]}",
    ] {
        let source = format!("def probe():\n    {expression}\n");
        let module = crate::parse_source(&source).unwrap();
        let Item::Function(function) = &module.items[0] else {
            panic!("function expected")
        };
        let expected = Span::new(2, expression.rfind("needle").unwrap() + 5);
        assert_eq!(
            super::last_name_reference_span_in_stmt(&function.body[0], "needle"),
            Some(expected),
            "{expression}"
        );
        assert_eq!(
            super::last_name_reference_span_in_stmt(&function.body[0], "absent"),
            None
        );
    }
    for body in [
        "    if true:\n        pass\n    else:\n        pass\n",
        "    if true:\n        pass\n    elif false:\n        pass\n",
        "    match true:\n        case true:\n            pass\n        case false:\n            pass\n",
        "    while true:\n        if false:\n            pass\n        else:\n            pass\n",
        "    for item in [1]:\n        with TaskGroup() as group:\n            pass\n",
    ] {
        let source = format!("def probe():\n{body}");
        let module = crate::parse_source(&source).unwrap();
        let Item::Function(function) = &module.items[0] else { panic!("function expected") };
        let (line, text) = source.lines().enumerate().last().unwrap();
        assert_eq!(super::block_end_span(&function.body), Some(Span::new(line + 1, text.find("pass").unwrap() + 1)));
    }
}

#[test]
fn adr0038_statement_reference_walker_covers_every_statement_shape() {
    let module = crate::parse_source(
        r#"
def probe(needle: bool):
    needle = false
    view alias = needle
    left, right = (needle, true)
    assert needle, str(needle)
    if needle:
        print(needle)
    else:
        print(needle)
    match needle:
        case true if needle:
            print(needle)
        case _:
            pass
    for item in [needle]:
        print(needle)
    with TaskGroup() as group:
        print(needle)
    while needle:
        break
    print(needle)
    return needle
"#,
    )
    .expect("reference walker input should parse");
    let body = module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == "probe" => Some(&function.body),
            _ => None,
        })
        .expect("probe function should exist");
    assert!(block_references_name(body, "needle"));
    for stmt in body {
        assert!(
            stmt_references_name(stmt, "needle"),
            "statement should retain its needle reference: {stmt:?}"
        );
    }
    let pass = Stmt::Pass(crate::ast::PassStmt {
        span: Span::new(1, 1),
    });
    let break_stmt = Stmt::Break(crate::ast::BreakStmt {
        span: Span::new(1, 1),
    });
    let continue_stmt = Stmt::Continue(crate::ast::ContinueStmt {
        span: Span::new(1, 1),
    });
    for stmt in [&pass, &break_stmt, &continue_stmt] {
        assert!(!stmt_references_name(stmt, "needle"));
    }
    assert!(!block_references_name(&[], "needle"));
}

#[test]
fn adr0038_closure_capture_loans_support_reborrowing_and_reject_overlap() {
    crate::check_source(
        r#"
def main():
    mut values = [1]
    view mut editable = values
    mut update: def(int64) -> None = lambda [mut editable] next: editable.append(next)
    update(3)
    print(editable)
"#,
    )
    .expect("a mutable closure capture may reborrow an existing mutable view");

    let error = crate::check_source(
        r#"
def main():
    mut values = [1]
    mut update: def(int64) -> None = lambda [mut values] next: values.append(next)
    inspect: def() -> int64 = lambda [values]: values.len()
    update(2)
    print(inspect())
"#,
    )
    .expect_err("overlapping closure loans must conflict while both closures remain live");
    assert_eq!(error.code, "AU3002");
}

#[test]
fn adr0038_reference_walker_exercises_nonmatching_nested_paths() {
    let module = crate::parse_source(
        r#"
def probe(value: bool):
    pair[0] = value
    assert value, str(value)
    if value:
        print(value)
    else:
        print(value)
    match value:
        case true if value:
            print(value)
    for item in [value]:
        print(value)
    with TaskGroup() as group:
        print(value)
    while value:
        print(value)
    return value
"#,
    )
    .expect("reference walker branch input should parse");
    let body = module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == "probe" => Some(&function.body),
            _ => None,
        })
        .expect("probe function should exist");
    for stmt in body {
        assert!(!stmt_references_name(stmt, "absent"));
    }
}

#[test]
fn adr0038_additional_view_diagnostics_cover_call_and_tuple_place_edges() {
    let cases = [
        (
            "def main():\n    mut values = [1]\n    view mut item = values[0]\n",
            "indexed collection elements do not have stable view identity",
        ),
        (
            "def bump(value: mut int64):\n    value += 1\n\ndef main():\n    mut values = [1, 2]\n    bump(values[0])\n",
            "must be a mutable place",
        ),
        (
            "def plain() -> int64:\n    return 1\n\ndef main():\n    view invalid = plain()\n",
            "a view source must be an addressable",
        ),
        (
            "class Box:\n    value: int64\n\n    def value_view(self) -> view int64 from self:\n        return view self.value\n\ndef main():\n    view invalid = Box(value=1).value_view()\n",
            "requires an addressable receiver place",
        ),
        (
            "def identity(value: int64) -> view int64 from value:\n    return view value\n\ndef main():\n    view invalid = identity(1 + 2)\n",
            "requires an addressable caller place",
        ),
    ];
    for (source, expected) in cases {
        let error = crate::check_source(source).expect_err(expected);
        assert!(
            error.message.contains(expected),
            "expected `{expected}`, found `{}`",
            error.message
        );
    }

    let tuple_overlap = crate::check_source(
        r#"
def main():
    mut pair = (1, 2)
    view first = pair[0]
    view mut conflicting = pair[0]
    print(first)
    print(conflicting)
"#,
    )
    .expect_err("tuple projection overlap should be diagnosed using the exact place");
    assert!(tuple_overlap.message.contains("pair[0]"));

    let mut missing_value = crate::parse_source(
        "def invalid(value: int64) -> view int64 from value:\n    return view value\n",
    )
    .expect("valid view return should parse before AST corruption");
    let function = missing_value
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Function(function) => Some(function),
            _ => None,
        })
        .expect("test function should exist");
    let Stmt::Return(return_stmt) = &mut function.body[0] else {
        panic!("test function should contain a return")
    };
    return_stmt.value = None;
    let error =
        super::check(missing_value).expect_err("view-return ASTs must retain an addressable value");
    assert!(error
        .message
        .contains("a view return requires an addressable source place"));

    let mut missing_contract = crate::parse_source(
        "def invalid(value: int64) -> view int64 from value:\n    return view value\n",
    )
    .expect("valid view return should parse before contract removal");
    let function = missing_contract
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Function(function) => Some(function),
            _ => None,
        })
        .expect("test function should exist");
    function.view_return = None;
    let error = super::check(missing_contract)
        .expect_err("return-view ASTs require matching function contracts");
    assert!(error
        .message
        .contains("requires a matching `-> view ... from source` declaration"));

    let mut literal_return = crate::parse_source(
        "def invalid(value: int64) -> view int64 from value:\n    return view value\n",
    )
    .expect("valid view return should parse before replacing its source");
    let function = literal_return
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Function(function) => Some(function),
            _ => None,
        })
        .expect("test function should exist");
    let Stmt::Return(return_stmt) = &mut function.body[0] else {
        panic!("test function should contain a return")
    };
    return_stmt.value = Some(expr(ExprKind::Int(1)));
    let error =
        super::check(literal_return).expect_err("view-return ASTs must retain a place expression");
    assert!(error.message.contains("must derive from its declared"));
}

#[test]
fn adr0038_shared_repeats_self_reborrows_and_mutable_capture_transfer_are_checked() {
    crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

    def identity(mut self) -> view mut Pair from self:
        return view mut self

def main():
    mut pair = Pair(left=1, right=2)
    view first = pair.left
    view second = pair.left
    print(first)
    print(second)
    view mut root = pair
    view mut nested = root.identity()
    nested.right = 3
    print(nested.right)
"#,
    )
    .expect("shared loans may coexist and self-returned views may reborrow an existing view");

    let mutable_lock = crate::check_source(
        r#"
def main():
    mut pair = (1, 2)
    view mut first = pair[0]
    pair[0] = 3
    print(first)
"#,
    )
    .expect_err("a live mutable tuple view must block source mutation");
    assert!(mutable_lock.message.contains("mutable view `first`"));

    let transfer = crate::check_source(
        r#"
def main():
    mut values = [1]
    mut worker: def() -> None = lambda [mut values]: values.append(2)
    with TaskGroup() as group:
        task = group.start(worker)
        task.result_or(None, timeout=1s)
"#,
    )
    .expect_err("a mutable closure loan cannot cross a task boundary");
    assert_eq!(transfer.code, "AU3008");
    assert!(transfer.message.contains("mutable"));
}

#[test]
fn adr0038_self_origin_trait_contracts_compare_by_receiver_slot() {
    crate::check_source(
        r#"
class Box:
    value: int64

trait Inspect:
    def inspect(self) -> view int64 from self

impl Inspect for Box:
    def inspect(self) -> view int64 from self:
        return view self.value
"#,
    )
    .expect("trait and implementation self-origin contracts should compare structurally");
}

#[test]
fn adr0038_view_place_helpers_cover_defensive_call_and_projection_edges() {
    let mut program = crate::check_source(
        r#"
class Box:
    value: int64

    def borrow(self) -> view int64 from self:
        return view self.value

def shared(value: int64) -> view int64 from value:
    return view value

def second(first: int64, second: int64) -> view int64 from second:
    return view second

def main():
    pass
"#,
    )
    .expect("view helper program should type check");

    let mut named_self = program.classes["Box"].methods["borrow"].clone();
    named_self.decl.name = "named_self".to_string();
    program.functions.insert(
        "named_self".to_string(),
        FunctionInfo {
            module_name: program.module_name.clone(),
            decl: named_self.decl,
            signature: named_self.signature,
            type_param_bounds: named_self.type_param_bounds,
        },
    );

    let mut missing_origin = program.functions["shared"].clone();
    missing_origin.decl.name = "missing_origin".to_string();
    missing_origin
        .decl
        .view_return
        .as_mut()
        .expect("shared has a returned-view contract")
        .origin = "missing".to_string();
    program
        .functions
        .insert("missing_origin".to_string(), missing_origin);

    let mut default_origin = program.functions["second"].clone();
    default_origin.decl.name = "default_origin".to_string();
    default_origin.decl.params[1].default = Some(expr(ExprKind::Int(0)));
    program
        .functions
        .insert("default_origin".to_string(), default_origin);

    let (type_names, type_arities) = type_maps_from_program(&program);
    let checker = FunctionChecker::new(
        &program.module_name,
        &type_names,
        &type_arities,
        empty_canonical_type_names(),
        &program.classes,
        &program.enums,
        &program.functions,
        &program.constants,
        &program.traits,
        &program.trait_impls,
        &program.imported_modules,
        &program.module_registry,
    );
    let mut locals = HashMap::from([
        (
            "tuple".to_string(),
            local_binding(
                Type::Tuple(vec![Type::named("int64"), Type::named("int64")]),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "items".to_string(),
            local_binding(
                Type::Named("list".to_string(), vec![Type::named("int64")]),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "box".to_string(),
            local_binding(
                Type::named("Box"),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
        (
            "value".to_string(),
            local_binding(
                Type::named("int64"),
                true,
                true,
                ReceiverKind::Value,
                false,
                &[],
            ),
        ),
    ]);
    let name = |name: &str| expr(ExprKind::Name(name.to_string()));
    let index = |object: &str, position: Expr| {
        expr(ExprKind::Index {
            object: Box::new(name(object)),
            index: Box::new(position),
        })
    };
    let call = |callee: Expr, args: Vec<Argument>| {
        expr(ExprKind::Call {
            callee: Box::new(callee),
            args,
        })
    };

    let missing_ordered = required_ordered_arg(
        &[],
        0,
        Span::new(1, 1),
        "required collection argument is missing",
    )
    .expect_err("the shared argument helper must diagnose a missing slot");
    assert_eq!(
        missing_ordered.message,
        "required collection argument is missing"
    );
    let missing_bound = checker
        .bound_argument(
            &[],
            0,
            Span::new(1, 2),
            "bound collection argument is missing",
        )
        .expect_err("the checker argument helper must diagnose a missing slot");
    assert_eq!(
        missing_bound.message,
        "internal error: bound collection argument is missing"
    );

    assert!(!checker
        .is_mutable_place(&index("items", expr(ExprKind::Int(0))), &mut locals)
        .expect("list indexing is not a stable mutable place"));
    assert!(!checker
        .is_mutable_place(&index("tuple", name("value")), &mut locals,)
        .expect("dynamic tuple indexing is not a mutable place"));
    let enormous = expr(ExprKind::Int(u128::MAX));
    assert!(!checker
        .is_mutable_place(&index("tuple", enormous.clone()), &mut locals)
        .expect("unrepresentable tuple positions are not mutable places"));
    assert!(!checker
        .is_mutable_place(&index("tuple", expr(ExprKind::Int(4))), &mut locals)
        .expect("out-of-bounds tuple positions are not mutable places"));
    assert!(!checker
        .is_mutable_place(&call(name("shared"), vec![arg(name("value"))]), &mut locals,)
        .expect("shared returned views are not mutable places"));
    let invalid_position = checker
        .view_place(&index("tuple", enormous), &mut locals)
        .expect_err("tuple view positions must fit usize");
    assert!(invalid_position
        .message
        .contains("invalid tuple view position"));

    assert!(checker
        .returned_view_call_place(&expr(ExprKind::Int(1)), &[], &mut locals)
        .expect("a literal is not a returned-view call")
        .is_none());
    let tuple_member = expr(ExprKind::Member {
        object: Box::new(name("tuple")),
        field: "missing".to_string(),
    });
    assert!(checker
        .returned_view_call_place(&tuple_member, &[], &mut locals)
        .expect("non-class member calls have no returned-view origin")
        .is_none());
    let box_member = expr(ExprKind::Member {
        object: Box::new(name("box")),
        field: "missing".to_string(),
    });
    assert!(checker
        .returned_view_call_place(&box_member, &[], &mut locals)
        .expect("unknown methods have no returned-view origin")
        .is_none());
    let self_error = checker
        .returned_view_call_place(&name("named_self"), &[], &mut locals)
        .expect_err("self-origin functions require a receiver");
    assert!(self_error.message.contains("requires an instance receiver"));
    assert!(checker
        .returned_view_call_place(&name("missing_origin"), &[arg(name("value"))], &mut locals,)
        .expect("an inconsistent origin name cannot resolve")
        .is_none());
    let default_error = checker
        .returned_view_call_place(&name("default_origin"), &[arg(name("value"))], &mut locals)
        .expect_err("a returned-view origin cannot be satisfied by a default");
    assert!(default_error.message.contains("must be supplied"));

    let grouped_origin = call(
        name("shared"),
        vec![arg(expr(ExprKind::Group(Box::new(name("value")))))],
    );
    assert!(checker
        .returned_view_call_parent(&grouped_origin, &mut locals)
        .is_none());
    assert!(checker
        .returned_view_call_parent(&call(tuple_member.clone(), Vec::new()), &mut locals)
        .is_none());
    assert!(checker
        .returned_view_call_parent(&call(expr(ExprKind::Int(1)), Vec::new()), &mut locals)
        .is_none());
    assert!(checker
        .returned_view_call_kind(&call(tuple_member, Vec::new()), &mut locals)
        .expect("non-class member calls have no returned-view kind")
        .is_none());
    assert!(checker
        .returned_view_call_kind(&call(expr(ExprKind::Int(1)), Vec::new()), &mut locals)
        .expect("literal callees have no returned-view kind")
        .is_none());

    let mut disjoint_loan = local_binding(
        Type::named("Box"),
        true,
        true,
        ReceiverKind::Value,
        false,
        &[],
    );
    disjoint_loan.view = Some(ViewBinding {
        kind: crate::ast::ViewKind::Mutable,
        source: PlacePath::root("box".to_string()),
        parent: None,
        ancestors: BTreeSet::new(),
        created_at: Span::new(1, 1),
        last_use: Span::new(2, 1),
    });
    locals.insert("disjoint".to_string(), disjoint_loan);
    checker
        .ensure_view_loan_available(
            &PlacePath::root("tuple".to_string()),
            crate::ast::ViewKind::Mutable,
            None,
            Span::new(3, 1),
            &locals,
        )
        .expect("disjoint active loans do not conflict");

    let not_a_tuple = checker
        .place_path_type(
            &PlacePath::root("items".to_string()).with_tuple(0),
            &locals,
            Span::new(4, 1),
        )
        .expect_err("tuple projection on a list must fail");
    assert!(not_a_tuple
        .message
        .contains("cannot project tuple position"));
    let unknown_namespace = checker
        .resolve_member_type(
            &Type::Module("missing.module".to_string()),
            "value",
            Span::new(4, 2),
        )
        .expect_err("module-typed values require a registered namespace");
    assert!(unknown_namespace
        .message
        .contains("unknown module namespace `missing.module`"));
    let out_of_bounds = checker
        .place_path_type(
            &PlacePath::root("tuple".to_string()).with_tuple(9),
            &locals,
            Span::new(5, 1),
        )
        .expect_err("out-of-bounds tuple paths must fail");
    assert!(out_of_bounds.message.contains("tuple has no position 9"));
    let member_index_overflow = checker
        .type_of_member_object_expr(&index("tuple", expr(ExprKind::Int(u128::MAX))), &mut locals)
        .expect_err("member-object tuple positions must fit usize");
    assert_eq!(member_index_overflow.code, "AU3004");
    assert!(member_index_overflow
        .message
        .contains("invalid tuple projection position"));
    let member_index_out_of_bounds = checker
        .type_of_member_object_expr(&index("tuple", expr(ExprKind::Int(8))), &mut locals)
        .expect_err("member-object tuple positions must be in bounds");
    assert_eq!(member_index_out_of_bounds.code, "AU3004");
    assert!(member_index_out_of_bounds
        .message
        .contains("tuple has no position 8"));

    let make_view = |parent: Option<&str>, ancestors: &[&str], last_use: Span| ViewBinding {
        kind: crate::ast::ViewKind::Shared,
        source: PlacePath::root("value".to_string()),
        parent: parent.map(str::to_string),
        ancestors: ancestors.iter().map(|name| (*name).to_string()).collect(),
        created_at: Span::new(10, 1),
        last_use,
    };
    let mut ancestor_locals = HashMap::new();
    let mut root = local_binding(
        Type::named("int64"),
        true,
        false,
        ReceiverKind::Value,
        false,
        &[],
    );
    root.view = Some(make_view(None, &[], Span::new(20, 1)));
    ancestor_locals.insert("root".to_string(), root);
    let mut child = local_binding(
        Type::named("int64"),
        true,
        false,
        ReceiverKind::Value,
        false,
        &[],
    );
    child.view = Some(make_view(Some("root"), &["root"], Span::new(21, 1)));
    ancestor_locals.insert("child".to_string(), child);
    let mut grandchild = local_binding(
        Type::named("int64"),
        true,
        false,
        ReceiverKind::Value,
        false,
        &[],
    );
    grandchild.view = Some(make_view(Some("child"), &[], Span::new(22, 1)));
    ancestor_locals.insert("grandchild".to_string(), grandchild);
    assert!(checker.view_descends_from("child", "root", &ancestor_locals));
    assert!(checker.view_descends_from("grandchild", "root", &ancestor_locals));
    assert!(!checker.view_descends_from("root", "child", &ancestor_locals));
    assert!(!checker.view_descends_from("missing", "root", &ancestor_locals));
    assert_eq!(
        checker.view_ancestor_names(Some("child"), &ancestor_locals),
        BTreeSet::from(["child".to_string(), "root".to_string()])
    );
    assert!(checker
        .view_ancestor_names(None, &ancestor_locals)
        .is_empty());

    checker.expire_views_before(Span::new(21, 2), &mut ancestor_locals);
    assert!(ancestor_locals["root"].view.is_none());
    assert!(ancestor_locals["child"].view.is_none());
    assert!(ancestor_locals["grandchild"].view.is_some());

    let mut cyclic_left = local_binding(
        Type::named("int64"),
        true,
        false,
        ReceiverKind::Value,
        false,
        &[],
    );
    cyclic_left.view = Some(make_view(Some("cyclic_right"), &[], Span::new(30, 1)));
    ancestor_locals.insert("cyclic_left".to_string(), cyclic_left);
    let mut cyclic_right = local_binding(
        Type::named("int64"),
        true,
        false,
        ReceiverKind::Value,
        false,
        &[],
    );
    cyclic_right.view = Some(make_view(Some("cyclic_left"), &[], Span::new(30, 1)));
    ancestor_locals.insert("cyclic_right".to_string(), cyclic_right);
    assert!(!checker.view_descends_from("cyclic_left", "absent", &ancestor_locals));

    let mut branch_only = local_binding(
        Type::named("int64"),
        true,
        false,
        ReceiverKind::Value,
        false,
        &[],
    );
    branch_only.view = Some(make_view(None, &[], Span::new(40, 1)));
    branch_only
        .closure_loans
        .push(make_view(None, &[], Span::new(40, 1)));
    ancestor_locals.insert("branch_only".to_string(), branch_only);
    checker.expire_views_unused_in_branch(
        &[Stmt::Pass(PassStmt {
            span: Span::new(41, 1),
        })],
        &BTreeMap::from([("branch_only".to_string(), Span::new(40, 1))]),
        &mut ancestor_locals,
    );
    assert!(ancestor_locals["branch_only"].view.is_none());
    assert!(ancestor_locals["branch_only"].closure_loans.is_empty());
}

#[test]
fn adr0038_lambda_capture_lists_reject_unused_and_immutable_mut_entries() {
    let unused = crate::check_source(
        "def main():\n    value = 1\n    callback: def(int64) -> int64 = lambda [value] item: item\n",
    )
    .expect_err("listed captures must be used by the lambda body");
    assert!(
        unused
            .message
            .contains("capture-list entry `value` is not used"),
        "unexpected diagnostic: {}",
        unused.message
    );

    let immutable = crate::check_source(
        "def main():\n    value = 1\n    callback: def() -> int64 = lambda [mut value]: value\n",
    )
    .expect_err("mutable capture entries require a mutable source place");
    assert!(
        immutable
            .message
            .contains("capture `mut value` requires a mutable place"),
        "unexpected diagnostic: {}",
        immutable.message
    );
}

#[test]
fn adr0038_grouped_closures_and_closure_children_retain_their_loans() {
    let grouped = crate::check_source(
        r#"
def main():
    mut value = 1
    callback: def() -> int64 = (lambda [value]: value)
    value = 2
    print(callback())
"#,
    )
    .expect_err("grouping a lambda must not erase its captured loan");
    assert_eq!(grouped.code, "AU3002");

    let suspended_parent = crate::check_source(
        r#"
def main():
    mut value = 1
    view mut parent = value
    callback: def() -> int64 = lambda [parent]: parent
    print(parent)
    print(callback())
"#,
    )
    .expect_err("a closure-held shared child must suspend its mutable parent");
    assert_eq!(suspended_parent.code, "AU3002");
}

#[test]
fn adr0038_tuple_views_use_nonconsuming_places_and_preserve_parent_capability() {
    crate::check_source(
        r#"
def main():
    pair = ("Ada", "Grace")
    view first = pair[0]
    print(first)
"#,
    )
    .expect("viewing a fixed non-Copy tuple element must not consume it");

    crate::check_source(
        r#"
def main():
    mut pair = (1, 2)
    view mut parent = pair
    view mut child = parent[0]
    child = 7
    print(parent)
"#,
    )
    .expect("a fixed tuple-position reborrow must retain its mutable parent identity");

    let escalation = crate::check_source(
        r#"
def expose(pair: mut (int64, int64)) -> view mut int64 from pair:
    view shared = pair
    return view mut shared[0]
"#,
    )
    .expect_err("a fixed tuple position must not hide shared-to-mutable escalation");
    assert_eq!(escalation.code, "AU3010");
}

#[test]
fn adr0038_loop_carried_views_and_wrapped_returned_views_do_not_escape() {
    let loop_carried = crate::check_source(
        r#"
def main():
    mut value = 1
    mut count = 0
    view alias = value
    while count < 2:
        print(alias)
        value = 2
        count += 1
"#,
    )
    .expect_err("a body use repeats across a loop backedge and must keep the loan live");
    assert_eq!(loop_carried.code, "AU3002");

    let wrapped_escape = crate::check_source(
        r#"
class Holder:
    value: int64

def borrow(value: int64) -> view int64 from value:
    return view value

def main():
    value = 1
    holder = Holder(value=borrow(value) if true else 0)
    print(holder.value)
"#,
    )
    .expect_err("a returned view cannot escape through a conditional field initializer");
    assert_eq!(wrapped_escape.code, "AU3010");
}

#[test]
fn adr0038_closure_loans_block_mutable_calls_and_owned_moves() {
    for source in [
        r#"
def change(value: mut int64):
    value = 2

def main():
    mut value = 1
    callback: def() -> int64 = lambda [value]: value
    change(value)
    print(callback())
"#,
        r#"
def take(values: own list[int64]):
    print(values)

def main():
    mut values = [1]
    callback: def() -> int64 = lambda [values]: values.len()
    take(values)
    print(callback())
"#,
        r#"
def main():
    mut values = [1]
    callback: def() -> int64 = lambda [values]: values.len()
    moved = values
    print(callback())
    print(moved)
"#,
        r#"
class Holder:
    values: list[int64]

def take(values: own list[int64]):
    print(values)

def main():
    mut holder = Holder(values=[1])
    callback: def() -> int64 = lambda [holder]: holder.values.len()
    take(holder.values)
    print(callback())
"#,
    ] {
        let error = crate::check_source(source)
            .expect_err("an active closure loan must block mutation or ownership transfer");
        assert_eq!(error.code, "AU3002", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_loan_closures_reject_unsupported_deferred_flows() {
    for source in [
        r#"
def main():
    mut value = 1
    factory = lambda [value]: lambda [value]: value
    callback = factory()
    value = 2
    print(callback())
"#,
        r#"
def choose(flag: bool):
    mut value = 1
    callback: def() -> int64 = lambda [value]: value
    selected = callback if flag else callback
    value = 2
    print(selected())
"#,
        r#"
def choose(flag: bool):
    mut value = 1
    callback: def() -> int64 = lambda [value]: value
    selected = match flag:
        case true: callback
        case false: callback
    value = 2
    print(selected())
"#,
        r#"
def main():
    mut value = 1
    inner: def() -> int64 = lambda [value]: value
    outer = lambda [own inner]: inner()
    value = 2
    print(outer())
"#,
        r#"
def main():
    mut value = 1
    callback: def() -> int64 = lambda [value]: value
    callbacks: list[def() -> int64] = [callback]
    value = 2
    print(callbacks[0]())
"#,
    ] {
        let error = crate::check_source(source)
            .expect_err("loan-bearing closures must remain in supported direct locals");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_loan_closures_reject_every_aggregate_storage_shape() {
    for body in [
        "    callbacks: set[def() -> int64] = {callback}\n    print(callbacks.len())\n",
        "    callbacks: dict[def() -> int64, int64] = {callback: 1}\n    print(callbacks.len())\n",
        "    callbacks: dict[str, def() -> int64] = {\"callback\": callback}\n    print(callbacks.len())\n",
        "    holder = Holder(callback=callback)\n    print(holder.callback())\n",
        "    packet = Packet.Callback(callback)\n    print(packet)\n",
        "    packet = Option.Some(callback)\n    print(packet)\n",
        "    packet = Option.Some((callback,))\n    print(packet)\n",
    ] {
        let source = format!(
            r#"
class Holder:
    callback: def() -> int64

enum Packet:
    Callback(def() -> int64)

def main():
    mut value = 1
    callback: def() -> int64 = lambda [value]: value
{body}"#
        );
        let error = crate::check_source(&source)
            .expect_err("a loan-bearing closure must not enter aggregate storage");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_direct_calls_cannot_erase_loan_closure_regions() {
    let error = crate::check_source(
        r#"
def consume(callback: own def() -> int64):
    print(callback())

def main():
    value = 1
    callback: def() -> int64 = lambda [value]: value
    consume(callback)
"#,
    )
    .expect_err("passing a loan-bearing closure must not erase its region at a call boundary");
    assert_eq!(error.code, "AU3010");
    assert!(error
        .message
        .contains("call cannot erase the region of a closure containing a live view"));
}

#[test]
fn adr0038_consuming_aggregate_arguments_reject_nested_loan_closures() {
    for (declaration, argument) in [
        (
            "def consume(values: own list[def() -> int64]):\n    pass",
            "[callback]",
        ),
        (
            "def consume(values: own set[def() -> int64]):\n    pass",
            "{callback}",
        ),
        (
            "def consume(values: own dict[def() -> int64, int64]):\n    pass",
            "{callback: 1}",
        ),
        (
            "def consume(values: own dict[str, def() -> int64]):\n    pass",
            "{\"callback\": callback}",
        ),
    ] {
        let source = format!(
            r#"
{declaration}

def main():
    value = 1
    callback: def() -> int64 = lambda [value]: value
    consume({argument})
"#
        );
        let error = crate::check_source(&source)
            .expect_err("an owned aggregate argument cannot erase a nested closure loan");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_functions_cannot_return_loan_bearing_closures() {
    let error = crate::check_source(
        r#"
def leak(value: int64) -> def() -> int64:
    callback: def() -> int64 = lambda [value]: value
    return callback

def main():
    callback = leak(1)
    print(callback())
"#,
    )
    .expect_err("an owned function return cannot carry a live view in a closure");
    assert_eq!(error.code, "AU3010");
    assert!(error
        .message
        .contains("function cannot return a closure containing a live view"));
}

#[test]
fn adr0038_inline_explicit_captures_acquire_their_loans_immediately() {
    for source in [
        r#"
def main():
    mut value = 1
    view mut parent = value
    print((lambda [value]: value)())
    parent = 2
"#,
        r#"
def consume(value: int64):
    print(value)

def main():
    mut value = 1
    view mut parent = value
    consume((lambda [value]: value)())
    parent = 2
"#,
    ] {
        let error = crate::check_source(source)
            .expect_err("an inline explicit capture must perform overlap acquisition checks");
        assert_eq!(error.code, "AU3002", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_copy_views_never_decay_into_owned_values() {
    for source in [
        r#"
def main():
    value = 1
    view alias = value
    owned = alias
    print(owned)
"#,
        r#"
def main():
    value = 1
    view alias = value
    stored = (alias,)
    print(stored)
"#,
        r#"
def main():
    value = 1
    view alias = value
    stored = [alias]
    print(stored)
"#,
        r#"
class Holder:
    value: int64

def main():
    value = 1
    view alias = value
    holder = Holder(value=alias)
    print(holder)
"#,
        r#"
enum Packet:
    Item(int64)

def main():
    value = 1
    view alias = value
    packet = Packet.Item(alias)
    print(packet)
"#,
        r#"
def main():
    value = 1
    view alias = value
    packet: Option[int64] = Option.Some(alias)
    print(packet)
"#,
        r#"
enum Packet:
    Item(int64)

class Box:
    value: int64

def borrow(box: mut Box) -> view mut int64 from box:
    return view mut box.value

def main():
    mut box = Box(value=1)
    packet = Packet.Item(borrow(box))
    box.value = 2
    print(packet)
"#,
    ] {
        let error = crate::check_source(source)
            .expect_err("Copy-valued views must not become owned bindings or aggregates");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_views_cannot_escape_through_assignment_or_destructuring_sinks() {
    for source in [
        r#"
class User:
    name: str

class Holder:
    value: str

def main():
    user = User(name="Ada")
    mut holder = Holder(value="empty")
    view name = user.name
    holder.value = name
"#,
        r#"
class User:
    name: str

def main():
    user = User(name="Ada")
    mut values = ["empty"]
    view name = user.name
    values[0] = name
"#,
        r#"
def main():
    pair = ("Ada", "Grace")
    view alias = pair
    (left, right) = alias
"#,
    ] {
        let error = crate::check_source(source)
            .expect_err("a view descriptor cannot enter an owned storage sink");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_returned_view_lookup_is_local_first_and_group_transparent() {
    let shadowed = crate::check_source(
        r#"
def borrow(value: int64) -> view int64 from value:
    return view value

def plain(value: int64) -> int64:
    return value

def main():
    borrow = plain
    value = 1
    view alias = borrow(value)
    print(alias)
"#,
    )
    .expect_err("a local callable must shadow a global returned-view function");
    assert!(matches!(shadowed.code.as_str(), "AU3004" | "AU3010"));

    crate::check_source(
        r#"
def borrow[T](value: T) -> view T from value:
    return view value

def main():
    value = 1
    view alias = (borrow)(value)
    view specialized = (borrow[int64])(value)
    print(alias)
    print(specialized)
"#,
    )
    .expect("grouping a free or specialized returned-view callee is transparent");
}

#[test]
fn adr0038_nested_place_types_are_nonconsuming_and_alias_relative() {
    crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def main():
    mut tuple_pair = (Pair(left=1, right=2), Pair(left=3, right=4))
    view nested = tuple_pair[0].right
    view mut editable = tuple_pair[1].left
    editable = 7
    print(nested)
"#,
    )
    .expect("nested tuple/class places must be typed without consuming tuple elements");

    crate::check_source(
        r#"
class Cell:
    value: int64

class Pair:
    left: Cell
    right: Cell

def choose(pair: mut Pair, left: bool) -> view mut Cell from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def main():
    mut pair = Pair(left=Cell(value=1), right=Cell(value=2))
    view mut selected = choose(pair, true)
    selected.value = 7
    view mut value = selected.value
    value = 8
    print(selected.value)
"#,
    )
    .expect("a child of a multi-projection returned view uses the alias pointee type");
}

#[test]
fn adr0038_branch_local_loan_expiry_is_independent_of_arm_order() {
    crate::check_source(
        r#"
def first(flag: bool):
    mut value = 1
    view alias = value
    if flag:
        value = 2
    else:
        print(alias)

def second(flag: bool):
    mut value = 1
    view alias = value
    if flag:
        print(alias)
    else:
        value = 2

def third(flag: bool):
    mut value = 1
    view alias = value
    match flag:
        case true:
            value = 2
        case false:
            print(alias)

def fourth(flag: bool):
    mut value = 1
    view alias = value
    match flag:
        case true:
            print(alias)
        case false:
            value = 2
"#,
    )
    .expect("an unused branch may end a loan regardless of textual arm order");
}

#[test]
fn adr0038_generic_calls_cannot_erase_loan_closure_regions() {
    for source in [
        r#"
def identity[T](value: own T) -> T:
    return value

def main():
    mut value = 1
    callback: def() -> int64 = lambda [value]: value
    escaped = identity(callback)
    value = 2
    print(escaped())
"#,
        r#"
class Holder[T]:
    value: T

def wrap[T](value: own T) -> Holder[T]:
    return Holder(value=value)

def main():
    mut value = 1
    callback: def() -> int64 = lambda [value]: value
    holder = wrap(callback)
    value = 2
    print(holder.value())
"#,
    ] {
        let error = crate::check_source(source)
            .expect_err("generic substitution must not erase a loan-bearing closure region");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_returned_view_arguments_retain_their_physical_places() {
    let error = crate::check_source(
        r#"
class Counter:
    value: int64

def value_mut(counter: mut Counter) -> view mut int64 from counter:
    return view mut counter.value

def update_both(left: mut int64, right: mut int64):
    left += 1
    right += 10

def main():
    mut counter = Counter(value=1)
    update_both(value_mut(counter), value_mut(counter))
"#,
    )
    .expect_err("two mutable returned-view arguments cannot overlap");
    assert_eq!(error.code, "AU3002");
}

#[test]
fn adr0038_mutable_returned_views_require_a_mutable_view_context() {
    for expression in [
        "inspect(value_mut(counter))",
        "print(value_mut(counter) + 1)",
        "value_mut(counter)",
    ] {
        let source = format!(
            r#"
class Counter:
    value: int64

def value_mut(counter: mut Counter) -> view mut int64 from counter:
    return view mut counter.value

def inspect(value: int64):
    print(value)

def main():
    mut counter = Counter(value=1)
    {expression}
"#
        );
        let error = crate::check_source(&source)
            .expect_err("a mutable returned view cannot decay into an ordinary read");
        assert_eq!(error.code, "AU3010", "{expression}: {error:?}");
    }

    crate::check_source(
        r#"
class Counter:
    value: int64

def value_mut(counter: mut Counter) -> view mut int64 from counter:
    return view mut counter.value

def bump(value: mut int64):
    value += 1

def main():
    mut counter = Counter(value=1)
    view mut selected = value_mut(counter)
    selected = 2
    bump(value_mut(counter))
"#,
    )
    .expect("mutable binding and immediate mutable reborrow contexts stay supported");
}

#[test]
fn adr0038_copy_view_reads_can_write_through_an_existing_mutable_view() {
    crate::check_source(
        r#"
def main():
    mut left = 1
    right = 2
    view mut output = left
    view input = right
    output = input
    print(left)
"#,
    )
    .expect("a Copy pointee may be read from one view and written through another");
}

#[test]
fn adr0038_if_elif_conditions_use_the_previous_false_edge_state() {
    crate::check_source(
        r#"
def bump(value: mut int64) -> bool:
    value += 1
    return true

def main():
    mut value = 1
    view alias = value
    if alias == 0:
        pass
    elif bump(value):
        pass
    print(value)
"#,
    )
    .expect("a loan used only by an earlier condition ends on its false edge");
}

#[test]
fn adr0038_recursive_singleton_forwarding_keeps_its_exact_footprint() {
    crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def select_left(pair: Pair, recurse: bool) -> view int64 from pair:
    if recurse:
        return view select_left(pair, false)
    return view pair.left

def main():
    mut pair = Pair(left=1, right=2)
    view left = select_left(pair, true)
    pair.right = 3
    print(left)
"#,
    )
    .expect("a recursive returned-view SCC with one fixed projection stays exact");
}

#[test]
fn adr0038_control_flow_forwarding_summaries_keep_exact_tuple_projections() {
    crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def through_match(pairs: (Pair, Pair), choose: bool) -> view int64 from pairs:
    match choose:
        case true:
            return view pairs[0].left
        case false:
            return view pairs[0].left

def through_for(pairs: (Pair, Pair)) -> view int64 from pairs:
    for item in [1]:
        return view pairs[0].left
    return view pairs[0].left

def through_while(pairs: (Pair, Pair), repeat: bool) -> view int64 from pairs:
    while repeat:
        return view pairs[0].left
    return view pairs[0].left

def main():
    mut matched = (Pair(left=1, right=2), Pair(left=3, right=4))
    mut iterated = (Pair(left=5, right=6), Pair(left=7, right=8))
    mut looped = (Pair(left=9, right=10), Pair(left=11, right=12))
    view first = through_match(matched, true)
    view second = through_for(iterated)
    view third = through_while(looped, false)
    matched[1].right = 20
    iterated[1].right = 30
    looped[1].right = 40
    print(first + second + third)
"#,
    )
    .expect("nested control returns must preserve the exact tuple projection footprint");
}

#[test]
fn adr0038_returned_shared_iterables_lock_their_origin() {
    let common = r#"
def borrow_values(values: list[int64]) -> view list[int64] from values:
    return view values

def mutate(values: mut list[int64]) -> bool:
    values[0] = 9
    return true
"#;
    for body in [
        r#"
def main():
    mut values = [1, 2]
    for item in borrow_values(values):
        values[0] = 9
        print(item)
"#,
        r#"
def main():
    mut values = [1, 2]
    for index, item in enumerate(borrow_values(values)):
        values[0] = 9
        print(index)
        print(item)
"#,
        r#"
def main():
    mut values = [1, 2]
    for left, right in zip(borrow_values(values), [3, 4]):
        values[0] = 9
        print(left + right)
"#,
        r#"
def main():
    mut values = [1, 2]
    selected = [item for item in borrow_values(values) if mutate(values)]
    print(selected)
"#,
    ] {
        let source = format!("{common}{body}");
        let error = crate::check_source(&source)
            .expect_err("a returned shared iterable must keep its physical origin locked");
        assert_eq!(error.code, "AU3002", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_returned_view_control_contexts_preserve_capability_and_lifetime() {
    let matched = crate::check_source(
        r#"
def borrow_option(value: Option[int64]) -> view Option[int64] from value:
    return view value

def main():
    mut value: Option[int64] = Some(1)
    match borrow_option(value):
        case Some(item):
            value = None
            print(item)
        case None:
            pass
"#,
    )
    .expect_err("a shared returned match scrutinee must lock its origin");
    assert_eq!(matched.code, "AU3002");

    let managed = crate::check_source(
        r#"
class Resource:
    value: int64

    def close(mut self):
        print(self.value)

def borrow_resource(resource: Resource) -> view Resource from resource:
    return view resource

def main():
    resource = Resource(value=7)
    with alias = borrow_resource(resource):
        print(alias.value)
"#,
    )
    .expect_err("a returned view cannot escape into an owned cleanup resource");
    assert_eq!(managed.code, "AU3010");

    crate::check_source(
        r#"
def main():
    mut value = 0
    view alias = value
    for item in range(alias, alias + 1):
        value = 9
        print(item)
"#,
    )
    .expect("a view used only to construct the iterable expires before the loop body");
}

#[test]
fn adr0038_mutable_returned_views_reject_all_read_only_control_contexts() {
    for body in [
        "    assert borrow_bool(value)\n",
        "    while borrow_bool(value):\n        break\n",
        "    match borrow_bool(value):\n        case true:\n            pass\n        case false:\n            pass\n",
        "    print(f\"{borrow_int(number)}\")\n",
        "    selected = [item for item in borrow_list(values)]\n    print(selected)\n",
        "    selected = [item for item in [1] if borrow_bool(value)]\n    print(selected)\n",
        "    selected = [borrow_int(number) for item in [1]]\n    print(selected)\n",
        "    callback: def() -> bool = lambda [mut value]: borrow_bool(value)\n    print(callback())\n",
        "    match 1:\n        case item if borrow_bool(value):\n            print(item)\n        case _:\n            pass\n",
        "    selected = match 1:\n        case item if borrow_bool(value): item\n        case _: 0\n    print(selected)\n",
        "    box.value += borrow_int(number)\n",
        "    values[0] += borrow_int(number)\n",
    ] {
        let source = format!(
            r#"
def borrow_bool(value: mut bool) -> view mut bool from value:
    return view mut value

def borrow_int(value: mut int64) -> view mut int64 from value:
    return view mut value

def borrow_list(values: mut list[int64]) -> view mut list[int64] from values:
    return view mut values

class Box:
    value: int64

def main():
    mut value = true
    mut number = 7
    mut values = [1]
    mut box = Box(value=0)
{body}"#
        );
        let error = crate::check_source(&source)
            .expect_err("mutable returned views cannot decay in read-only control contexts");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_returned_view_metadata_lookup_is_inert_for_ordinary_type_calls() {
    crate::check_source(
        r#"
class Factory:
    def make(value: int64) -> int64:
        return value

def main():
    maybe: Option[int64] = Option.Some(1)
    result: Result[int64, str] = Result.Ok(2)
    value = Factory.make(3)
    print(maybe)
    print(result)
    print(value)
"#,
    )
    .expect("returned-view metadata probing must not reinterpret ordinary associated calls");
}

#[test]
fn adr0038_mutable_result_context_checks_preserve_nested_scopes_and_moves() {
    crate::check_source(
        r#"
class Flag:
    value: bool

    def okay(self) -> bool:
        return self.value

class Token:
    value: str

    def take(own self) -> str:
        return self.value

enum Choice:
    Value(Flag)
    Empty

def main():
    flags = [Flag(value=true)]
    selected = [item.value for item in flags if item.okay()]
    choice = Choice.Value(Flag(value=true))
    result = match choice:
        case Value(flag) if flag.okay(): 1
        case _: 0
    token = Token(value="kept")
    print(selected)
    print(result)
    print(token.take())
"#,
    )
    .expect("context-only mutable-result checks must retain comprehension, arm, and move state");
}

#[test]
fn adr0038_lambda_owned_results_cannot_erase_returned_view_regions() {
    for body in ["borrow_bool(value)", "(borrow_bool(value),)"] {
        let source = format!(
            r#"
def borrow_bool(value: bool) -> view bool from value:
    return view value

def main():
    value = true
    callback = lambda [value]: {body}
    print(callback())
"#,
        );
        let error = crate::check_source(&source)
            .expect_err("an owned lambda result cannot erase a returned-view region");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_view_capabilities_follow_receiver_and_argument_passing() {
    crate::check_source(
        r#"
class Box:
    value: int64

    def inspect(self) -> int64:
        return self.value

    def change(mut self, value: int64):
        self.value = value

class Wrapper:
    box: Box

def shared(box: Box) -> view Box from box:
    return view box

def borrowed(box: mut Box) -> view mut Box from box:
    return view mut box

def borrowed_wrapper(wrapper: mut Wrapper) -> view mut Wrapper from wrapper:
    return view mut wrapper

def main():
    mut box = Box(value=1)
    mut wrapper = Wrapper(box=Box(value=3))
    print(shared(box).inspect())
    borrowed(box).change(2)
    borrowed_wrapper(wrapper).box.change(4)
    print(box.value)
    print(wrapper.box.value)
"#,
    )
    .expect("returned receivers retain their capability when the receiver mode matches");

    for body in [
        "    print(borrowed_box(box).inspect())\n",
        "    print(borrowed_box(box).take())\n",
        "    print(shared(box).take())\n",
        "    print(borrowed_values(values).len())\n",
    ] {
        let source = format!(
            r#"
class Box:
    value: int64

    def inspect(self) -> int64:
        return self.value

    def take(own self) -> int64:
        return self.value

def shared(box: Box) -> view Box from box:
    return view box

def borrowed_box(box: mut Box) -> view mut Box from box:
    return view mut box

def borrowed_values(values: mut list[int64]) -> view mut list[int64] from values:
    return view mut values

def main():
    mut box = Box(value=1)
    mut values = [1]
{body}"#
        );
        let error = crate::check_source(&source)
            .expect_err("a returned receiver cannot decay to another capability");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_owned_sinks_cannot_erase_returned_view_regions() {
    let common = r#"
trait Add[Rhs, Out]:
    def add(own self, rhs: own Rhs) -> Out

class Box:
    value: str

impl Add[Box, Box] for Box:
    def add(own self, rhs: own Box) -> Box:
        return Box(value=self.value + rhs.value)

enum Choice:
    Item(str)

def shared_box(box: Box) -> view Box from box:
    return view box

def shared_boxes(boxes: list[Box]) -> view list[Box] from boxes:
    return view boxes

def shared_choice(choice: Choice) -> view Choice from choice:
    return view choice

def shared_result(result: Result[str, str]) -> view Result[str, str] from result:
    return view result
"#;
    for body in [
        "    result = shared_box(left) + right\n    print(result.value)\n",
        "    result = left + shared_box(right)\n    print(result.value)\n",
        "    boxes.append(shared_box(left))\n",
        "    match own shared_choice(choice):\n        case Item(text):\n            print(text)\n",
        "    for item in own shared_boxes(boxes):\n        print(item.value)\n",
        "    text = try shared_result(outcome)\n    print(text)\n",
        "    text = shared_box(left).value\n    print(text)\n",
    ] {
        let source = format!(
            r#"{common}
def use() -> Result[None, str]:
    left = Box(value="left")
    right = Box(value="right")
    mut boxes = list[Box]()
    choice = Choice.Item("choice")
    outcome: Result[str, str] = Result.Ok("ok")
{body}    return Result.Ok(None)
"#
        );
        let error = crate::check_source(&source)
            .expect_err("an owned sink cannot erase a returned-view region");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }

    let constant = crate::check_source(
        r#"
class Box:
    value: str

def shared(box: Box) -> view Box from box:
    return view box

BASE = Box(value="base")
ALIAS = shared(BASE)

def main():
    print(ALIAS.value)
"#,
    )
    .expect_err("module storage cannot retain a returned view");
    assert_eq!(constant.code, "AU3010");
}

#[test]
fn adr0038_returned_views_reject_nested_literal_and_call_storage_contexts() {
    let common = r#"
class Box:
    value: int64

class Holder:
    value: int64

def shared_box(box: Box) -> view Box from box:
    return view box

def shared_int(value: int64) -> view int64 from value:
    return view value

def keep(value: own int64) -> int64:
    return value
"#;
    for body in [
        "    stored = [shared_int(value)]\n    print(stored)\n",
        "    stored: set[int64] = {shared_int(value)}\n    print(stored)\n",
        "    stored = {shared_int(value): 1}\n    print(stored)\n",
        "    stored = {\"value\": shared_int(value)}\n    print(stored)\n",
        "    stored = ((shared_int(value),),)\n    print(stored)\n",
        "    stored = shared_int(value) if flag else 0\n    print(stored)\n",
        "    stored = match flag:\n        case true: shared_int(value)\n        case false: 0\n    print(stored)\n",
        "    stored = Holder(value=shared_int(value))\n    print(stored.value)\n",
        "    stored = Option.Some(shared_int(value))\n    print(stored)\n",
        "    stored = keep(shared_int(value))\n    print(stored)\n",
    ] {
        let source = format!(
            r#"{common}
def main():
    value = 1
    flag = true
    box = Box(value=2)
{body}"#
        );
        let error = crate::check_source(&source)
            .expect_err("a returned view must not escape into owned storage");
        assert_eq!(error.code, "AU3010", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_mutable_returned_view_arguments_preserve_capability_and_overlap() {
    let common = r#"
class Pair:
    left: int64
    right: int64

def left(pair: mut Pair) -> view mut int64 from pair:
    return view mut pair.left

def read(value: int64):
    print(value)

def write(value: mut int64):
    value = 9

def write_two(first: mut int64, second: mut int64):
    first = 1
    second = 2
"#;
    for body in [
        "    read(left(pair))\n",
        "    print(left(pair))\n",
        "    write_two(left(pair), pair.left)\n",
        "    write_two(first=left(pair), second=pair.left)\n",
    ] {
        let source = format!(
            r#"{common}
def main():
    mut pair = Pair(left=1, right=2)
{body}"#
        );
        let error = crate::check_source(&source)
            .expect_err("mutable returned-view arguments must retain capability and place overlap");
        assert!(
            matches!(error.code.as_str(), "AU3002" | "AU3010"),
            "{source}: {error:?}"
        );
    }

    crate::check_source(&format!(
        r#"{common}
def main():
    mut pair = Pair(left=1, right=2)
    write(left(pair))
    print(pair.left)
"#
    ))
    .expect("a mutable returned view may flow directly to a mutable parameter");
}

#[test]
fn adr0038_recursive_projection_cycles_remain_conservative() {
    let error = crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def first(pair: Pair) -> view int64 from pair:
    return view second(pair)

def second(pair: Pair) -> view int64 from pair:
    return view first(pair)

def main():
    mut pair = Pair(left=1, right=2)
    view selected = first(pair)
    pair.right = 3
    print(selected)
"#,
    )
    .expect_err("a projection cycle with no concrete return must conservatively lock its origin");
    assert_eq!(error.code, "AU3002");
}

#[test]
fn adr0038_assignment_indices_cannot_read_mutable_returned_views() {
    let error = crate::check_source(
        r#"
def borrowed(index: mut int64) -> view mut int64 from index:
    return view mut index

def main():
    mut index = 0
    mut values = [1]
    values[borrowed(index)] = 2
"#,
    )
    .expect_err("an assignment index is a read-only value context");
    assert_eq!(error.code, "AU3010");
}

#[test]
fn adr0038_deep_mutable_reborrows_suspend_and_resume_every_ancestor() {
    crate::check_source(
        r#"
def main():
    mut value = 1
    view mut first = value
    view mut second = first
    view mut third = second
    third = 2
    print(first)
"#,
    )
    .expect("a deep contained reborrow must release every ancestor after its last use");

    let suspended = crate::check_source(
        r#"
def main():
    mut value = 1
    view mut first = value
    view mut second = first
    view mut third = second
    print(first)
    third = 2
"#,
    )
    .expect_err("a live deep descendant must keep every mutable ancestor suspended");
    assert_eq!(suspended.code, "AU3002");
}

#[test]
fn adr0038_whole_copy_views_cannot_escape_through_owned_returns() {
    let error = crate::check_source(
        r#"
def leak(value: int64) -> int64:
    view alias = value
    return alias

def main():
    print(leak(7))
"#,
    )
    .expect_err("an ordinary return cannot erase a Copy-valued view descriptor");
    assert_eq!(error.code, "AU3010");
}

#[test]
fn adr0038_shared_views_support_direct_read_contexts_and_copy_projections() {
    crate::check_source(
        r#"
class Box:
    value: int64

def borrow_bool(value: bool) -> view bool from value:
    return view value

def borrow_index(index: int64) -> view int64 from index:
    return view index

def borrow_box(box: Box) -> view Box from box:
    return view box

def main():
    flag = true
    index = 0
    box = Box(value=7)
    mut target_values = [1, 2]
    source_values = [1, 2]
    view local_flag = flag
    view local_index = index
    view local_box = box
    view local_values = source_values
    assert borrow_bool(flag)
    selected = [item for item in [1] if local_flag]
    target_values[borrow_index(index)] = 2
    first = local_box.value
    second = borrow_box(box).value
    third = local_values[0]
    converted = local_index as int64
    print(selected)
    print(first + second + third + converted)
"#,
    )
    .expect("direct reads copy pointee values without storing their view descriptors");
}

#[test]
fn adr0038_projected_returned_views_preserve_capability_and_parentage() {
    let escalation = crate::check_source(
        r#"
class Box:
    value: int64

def shared(box: Box) -> view Box from box:
    return view box

def escalate(box: mut Box) -> view mut int64 from box:
    return view mut shared(box).value
"#,
    )
    .expect_err("a projection cannot hide a shared returned-view capability");
    assert_eq!(escalation.code, "AU3010");

    crate::check_source(
        r#"
class Box:
    value: int64

class Wrapper:
    box: Box

def borrowed(wrapper: mut Wrapper) -> view mut Wrapper from wrapper:
    return view mut wrapper

def identity(wrapper: mut Wrapper) -> view mut Wrapper from wrapper:
    return view mut wrapper

def main():
    mut wrapper = Wrapper(box=Box(value=1))
    view mut root = wrapper
    view mut field = borrowed(root).box
    field.value = 9
    print(root.box.value)

    view mut nested = identity(borrowed(root)).box
    nested.value = 10
    print(root.box.value)
"#,
    )
    .expect("a projected mutable returned view remains a contained reborrow of its parent");
}

#[test]
fn adr0038_match_and_iteration_locks_use_canonical_physical_places() {
    crate::check_source(
        r#"
enum Slot:
    Filled(str)
    Empty

def main():
    mut slot = Slot.Filled("old")
    match mut slot:
        case Filled(value):
            slot = Slot.Empty
        case Empty:
            pass
    match mut slot:
        case Filled(value):
            print(value)
        case Empty:
            print("empty")
"#,
    )
    .expect("the syntactic `match mut` scrutinee remains an authorized writeback route");

    for source in [
        r#"
enum Slot:
    Filled(str)

def main():
    mut slot = Slot.Filled("old")
    view parent = slot
    match parent:
        case Filled(value):
            slot = Slot.Filled("new")
            print(value)
"#,
        r#"
enum Slot:
    Filled(str)

def main():
    mut slot = Slot.Filled("old")
    view mut parent = slot
    match mut parent:
        case Filled(value):
            slot = Slot.Filled("new")
            print(value)
"#,
        r#"
enum Slot:
    Filled(int64)

def main():
    mut slot = Slot.Filled(1)
    view mut parent = slot
    match mut parent:
        case Filled(value):
            view mut child = parent
            match mut child:
                case Filled(other):
                    other = 2
"#,
        r#"
def main():
    mut values = [1, 2]
    view mut parent = values
    for item in mut parent:
        view mut child = parent
        child[0] = 9
        print(item)
"#,
    ] {
        let error = crate::check_source(source)
            .expect_err("a syntactic view alias must not bypass a physical match or loop lock");
        assert_eq!(error.code, "AU3002", "{source}: {error:?}");
    }
}

#[test]
fn adr0038_call_initialized_view_aliases_keep_exact_returned_footprints() {
    crate::check_source(
        r#"
class Pair:
    left: int64
    right: int64

def inner(pair: Pair) -> view int64 from pair:
    return view pair.left

def outer(pair: Pair) -> view int64 from pair:
    view alias = inner(pair)
    return view alias

def main():
    mut pair = Pair(left=1, right=2)
    view selected = outer(pair)
    pair.right = 3
    print(selected)
"#,
    )
    .expect("a call-initialized local alias must retain its unique transitive projection");
}

#[test]
fn adr0038_concrete_generic_trait_calls_narrow_returned_view_footprints() {
    crate::check_source(
        r#"
trait Project:
    def get(self) -> view int64 from self

trait Alternate:
    def get(self) -> view int64 from self

class LeftBox:
    left: int64
    right: int64

class RightBox:
    left: int64
    right: int64

class Envelope[T]:
    value: T

impl Project for LeftBox:
    def get(self) -> view int64 from self:
        return view self.left

impl Project for RightBox:
    def get(self) -> view int64 from self:
        return view self.right

impl Alternate for LeftBox:
    def get(self) -> view int64 from self:
        return view self.right

def forward[T: Project](value: T) -> view int64 from value:
    view selected = value.get()
    return view selected

def identity[T](value: T) -> view T from value:
    return view value

def through_envelope[T: Project](envelope: Envelope[T]) -> view int64 from envelope:
    view selected = forward(envelope.value)
    return view selected

def through_alias[T: Project](value: T) -> view int64 from value:
    view alias = identity(value)
    return view forward(alias)

def main():
    mut explicit_box = LeftBox(left=1, right=2)
    mut inferred_box = LeftBox(left=3, right=4)
    mut grouped_box = LeftBox(left=5, right=6)
    mut named_box = LeftBox(left=7, right=8)
    mut envelope = Envelope(value=LeftBox(left=9, right=10))
    mut aliased_box = LeftBox(left=11, right=12)
    view explicit = forward[LeftBox](explicit_box)
    explicit_box.right = 20
    view inferred = forward(inferred_box)
    inferred_box.right = 40
    view grouped = (forward[LeftBox])(grouped_box)
    grouped_box.right = 60
    view named = forward[LeftBox](value=named_box)
    named_box.right = 80
    view parameterized = through_envelope[LeftBox](envelope)
    envelope.value.right = 100
    view aliased = through_alias[LeftBox](aliased_box)
    aliased_box.right = 120
    print(explicit + inferred + grouped + named + parameterized + aliased)
"#,
    )
    .expect("concrete generic dispatch must select one implementation footprint at each call");
}

#[test]
fn adr0038_imported_generic_specializations_keep_owner_relative_footprints() {
    let unique = format!(
        "aura-sema-returned-view-specialization-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos()
    );
    let package = std::env::temp_dir().join(unique);
    let src = package.join("src");
    std::fs::create_dir_all(&src).expect("temporary specialization package should exist");
    std::fs::write(
        package.join("Aura.toml"),
        "[package]\nname = \"returned_view_specialization\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .expect("temporary package manifest should be writable");
    std::fs::write(
        src.join("api.au"),
        r#"public trait Project:
    def get(self) -> view int64 from self

public trait Alternate:
    def get(self) -> view int64 from self

public class LeftBox:
    public left: int64
    public right: int64

    public def direct(self) -> view int64 from self:
        return view self.left

    public def forwarded(self) -> view int64 from self:
        return view self.direct()

    public def associated(value: LeftBox) -> view int64 from value:
        return view value.left

public class RightBox:
    public left: int64
    public right: int64

impl Project for LeftBox:
    def get(self) -> view int64 from self:
        return view self.left

impl Project for RightBox:
    def get(self) -> view int64 from self:
        return view self.right

impl Alternate for LeftBox:
    def get(self) -> view int64 from self:
        return view self.right

public def forward[T: Project](value: T) -> view int64 from value:
    view selected = value.get()
    return view selected

public def forward_twice[T: Project](value: T) -> view int64 from value:
    return view forward(value)
"#,
    )
    .expect("temporary specialization module should be writable");
    let main_path = src.join("main.au");
    std::fs::write(
        &main_path,
        r#"import api

def main():
    mut explicit_box = api.LeftBox(left=1, right=2)
    mut inferred_box = api.LeftBox(left=3, right=4)
    view explicit = api.forward[api.LeftBox](explicit_box)
    explicit_box.right = 20
    view inferred = api.forward(inferred_box)
    inferred_box.right = 40
    view method = explicit_box.forwarded()
    explicit_box.right = 30
    view associated = api.LeftBox.associated(inferred_box)
    inferred_box.right = 50
    view nested_generic = api.forward_twice[api.LeftBox](explicit_box)
    explicit_box.right = 60
    print(explicit + inferred + method + associated + nested_generic)
"#,
    )
    .expect("temporary specialization entry should be writable");

    crate::check_path(&main_path)
        .expect("imported explicit and inferred calls must retain declaration-owner precision");

    for (expression, expected) in [
        (
            "api.LeftBox.associated",
            "associated method values are not supported",
        ),
        (
            "api.forward",
            "cannot be stored as a structural function value",
        ),
        ("api.LeftBox", "must be constructed with `(...)`"),
    ] {
        std::fs::write(
            &main_path,
            format!("import api\n\ndef main():\n    value = {expression}\n    print(value)\n"),
        )
        .expect("temporary module-member diagnostic entry should be writable");
        let error = crate::check_path(&main_path)
            .expect_err("imported module items cannot decay into unsupported values");
        assert!(error.message.contains(expected), "{expression}: {error:?}");
    }
    let _ = std::fs::remove_dir_all(&package);
}
