use super::{
    analysis_diagnostic, analysis_type_contains_unknown, analyze_path_source, analyze_source,
    base_type_name, block_contains_line, builtin_enum_hover, builtin_enum_variant_completions,
    builtin_function_hover, builtin_function_return_type, builtin_member_completions,
    callable_contains_line, complete_path_source, complete_source,
    enclosing_function_return_placeholder, expression_end_line, extract_receiver_before_dot,
    extract_receiver_ending_before, find_identifier_in_line, find_receiver_start,
    first_dangling_member_line, format_class_hover, format_enum_hover_named,
    format_function_detail, format_function_hover, format_method_hover, format_value_hover,
    format_variant_hover, infer_builtin_variant_call, lower_type_ref,
    placeholder_stmt_for_return_type, range_from_span, range_from_span_with_path,
    recover_checked_program_after_member_errors, recover_checked_program_after_member_errors_with,
    recover_checked_program_after_parse_error_with, recover_checked_program_after_position,
    render_view_source, replace_dangling_member_stmt_with_recovery_stmt,
    sanitize_member_completion_source, stmt_end_line, stmt_start_line, symbols_from_module,
    view_source_root, AnalysisBuilder, TypeExt,
};
use crate::ast::{
    Argument, AssignStmt, AssignTarget, BinaryOp, ClassDecl, Expr, ExprKind, FunctionDecl, Item,
    ParamMode, PassStmt, ReceiverKind, ReturnStmt, TypeRef, VariantPattern, ViewStmt,
};
use crate::diag::{Diagnostic, RuntimeCallFrame, RuntimeSourceSpan, RuntimeTaskFrame, Span};
use crate::sema::{
    ClassInfo, EnumInfo, EnumVariantInfo, FieldInfo, FunctionSignature, MethodInfo, TraitBound,
    Type,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir {
    path: PathBuf,
}

#[test]
fn analysis_resolves_canonical_enums_from_the_module_registry() {
    let mut program = checked_program("def main():\n    pass\n");
    let remote = checked_program("enum Value:\n    Null\n    Int(int64)\n");
    let remote_value = remote
        .enums
        .get("Value")
        .expect("remote Value enum should exist")
        .clone();
    program.module_registry.insert(
        "json".to_string(),
        crate::sema::ModuleNamespace {
            constants: BTreeMap::new(),
            all_constants: BTreeMap::new(),
            name: "json".to_string(),
            path: "json".to_string(),
            source_path: None,
            closures: Default::default(),
            comprehensions: Default::default(),
            modules: Default::default(),
            functions: Default::default(),
            extern_functions: Default::default(),
            opaque_handles: Default::default(),
            classes: Default::default(),
            enums: remote.enums.clone(),
            traits: Default::default(),
            trait_impls: Vec::new(),
            all_functions: Default::default(),
            all_extern_functions: Default::default(),
            all_opaque_handles: Default::default(),
            all_classes: Default::default(),
            all_enums: remote.enums,
            all_traits: Default::default(),
            imported_modules: Default::default(),
        },
    );
    program
        .enums
        .insert("Value".to_string(), remote_value.clone());
    program
        .canonical_type_names
        .insert("Value".to_string(), "json.Value".to_string());
    let builder = AnalysisBuilder::new("", &program, Vec::new());

    assert!(builder.resolve_named_enum_info("json.Value").is_some());
    assert_eq!(
        builder.canonical_enum_identity("Value", &remote_value),
        "json.Value"
    );
    for surface_name in ["json.Value", "Value"] {
        let details = builder
            .member_completions(&Type::named(surface_name))
            .into_iter()
            .map(|item| (item.name, item.detail))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(details.get("Null"), Some(&"Null -> json.Value".to_string()));
        assert_eq!(
            details.get("Int"),
            Some(&"Int(own int64) -> json.Value".to_string())
        );
    }
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

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .unwrap()
        .to_path_buf()
}

fn run_with_large_stack<T, F>(operation: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(operation)
        .expect("large-stack helper thread should spawn")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

fn collect_aura_files(dir: &PathBuf) -> Vec<PathBuf> {
    let mut files = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {}", dir.display(), error))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let is_aura = path.extension().and_then(|ext| ext.to_str()) == Some("au");
            if is_aura {
                Some(path)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn type_ref(name: &str) -> TypeRef {
    TypeRef::named(name, Vec::new(), false, Span::new(1, 1))
}

fn checked_program(source: &str) -> crate::sema::Program {
    crate::check_source(source).expect("source should type check")
}

#[test]
fn typed_select_analysis_exposes_inferred_outcomes_and_builtin_surface() {
    let program = checked_program("def main():\n    pass\n");
    let builder = AnalysisBuilder::new("", &program, Vec::new());
    let binding = |name: &str, ty: Type| {
        (
            name.to_string(),
            super::BindingInfo {
                ty,
                trait_bounds: Vec::new(),
                definition: super::AnalysisRange {
                    file_path: None,
                    line: 0,
                    start_character: 0,
                    end_character: name.len(),
                },
                hover: format!("binding {name}"),
            },
        )
    };
    let scope = BTreeMap::from([
        binding(
            "jobs",
            Type::Named("Queue".to_string(), vec![Type::named("str")]),
        ),
        binding(
            "task",
            Type::Named("Task".to_string(), vec![Type::named("int32")]),
        ),
    ]);

    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Name("select".to_string())),
            &[
                arg(expr(ExprKind::Name("jobs".to_string()))),
                arg(expr(ExprKind::DurationNanos(1_000_000))),
                arg(expr(ExprKind::Name("task".to_string()))),
            ],
            &scope,
        ),
        Some(Type::Named(
            "SelectOutcome".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        ))
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Name("select".to_string())),
            &[arg(expr(ExprKind::DurationNanos(0)))],
            &scope,
        ),
        Some(Type::Named(
            "SelectOutcome".to_string(),
            vec![Type::Unit, Type::Unit],
        ))
    );

    let top_level = builder
        .top_level_completions()
        .into_iter()
        .map(|completion| (completion.name, completion.detail))
        .collect::<BTreeMap<_, _>>();
    assert!(top_level
        .get("select")
        .is_some_and(|detail| detail.contains("SelectOutcome[Q, T]")));
    assert_eq!(
        top_level.get("SelectOutcome"),
        Some(&"enum SelectOutcome[Q, T]".to_string())
    );

    let variants = builtin_enum_variant_completions("SelectOutcome")
        .into_iter()
        .map(|completion| (completion.name, completion.detail))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(variants.len(), 4);
    assert!(variants
        .get("Queue")
        .is_some_and(|detail| detail == "Queue(own int64, own QueueReceive[Q]) -> SelectOutcome"));
    assert!(variants
        .get("Task")
        .is_some_and(|detail| detail == "Task(own int64, own TaskResult[T]) -> SelectOutcome"));
    assert_eq!(
        variants.get("Deadline"),
        Some(&"Deadline(own int64) -> SelectOutcome".to_string())
    );
    for variant in ["Queue", "Task", "Deadline", "Cancelled"] {
        assert!(
            builder
                .resolve_member_type(&Type::named("SelectOutcome"), variant)
                .is_some(),
            "{variant}"
        );
    }
}

#[test]
fn typed_select_analysis_withholds_types_for_invalid_or_ambiguous_sources() {
    let program = checked_program("def main():\n    pass\n");
    let builder = AnalysisBuilder::new("", &program, Vec::new());
    let binding = |name: &str, ty: Type| {
        (
            name.to_string(),
            super::BindingInfo {
                ty,
                trait_bounds: Vec::new(),
                definition: super::AnalysisRange {
                    file_path: None,
                    line: 0,
                    start_character: 0,
                    end_character: name.len(),
                },
                hover: format!("binding {name}"),
            },
        )
    };
    let scope = BTreeMap::from([
        binding(
            "int_jobs",
            Type::Named("Queue".to_string(), vec![Type::named("int32")]),
        ),
        binding(
            "text_jobs",
            Type::Named("Queue".to_string(), vec![Type::named("str")]),
        ),
        binding(
            "int_task",
            Type::Named("Task".to_string(), vec![Type::named("int32")]),
        ),
        binding(
            "text_task",
            Type::Named("Task".to_string(), vec![Type::named("str")]),
        ),
    ]);
    let select = expr(ExprKind::Name("select".to_string()));
    let named_source = |name: &str| arg(expr(ExprKind::Name(name.to_string())));
    let keyword_source = |name: &str| {
        let mut argument = named_source(name);
        argument.name = Some("source".to_string());
        argument
    };

    for sources in [
        vec![named_source("int_jobs"), named_source("text_jobs")],
        vec![named_source("int_task"), named_source("text_task")],
        vec![keyword_source("int_jobs")],
        vec![arg(expr(ExprKind::Int(1)))],
        vec![named_source("missing")],
        Vec::new(),
    ] {
        assert_eq!(
            builder.infer_call_type(&select, &sources, &scope),
            None,
            "analysis must not advertise a SelectOutcome type for invalid or incomplete sources"
        );
    }
}

fn function_decl(name: &str, return_type: &str) -> FunctionDecl {
    FunctionDecl {
        public: true,
        name: name.to_string(),
        type_params: Vec::new(),
        type_param_bounds: Default::default(),
        receiver: Some(ReceiverKind::Borrow),
        params: vec![crate::ast::Param {
            name: "value".to_string(),
            mode: ParamMode::Default,
            ty: type_ref("int32"),
            default: None,
            span: Span::new(1, 1),
        }],
        return_type: type_ref(return_type),
        view_return: None,
        body: Vec::new(),
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

fn expr(kind: ExprKind) -> Expr {
    Expr {
        kind,
        span: Span::new(1, 1),
    }
}

#[test]
fn d5_analysis_renders_canonical_receiver_modes_and_completes_own_keyword() {
    let mut method = function_decl("render", "bool");
    assert_eq!(
        format_function_detail(&method),
        "render(self, value: int32) -> bool"
    );
    assert_eq!(
        format_method_hover(&method),
        "```aura\nmethod render(self, value: int32) -> bool\n```"
    );

    method.receiver = Some(ReceiverKind::Value);
    assert_eq!(
        format_function_detail(&method),
        "render(own self, value: int32) -> bool"
    );
    assert_eq!(
        format_method_hover(&method),
        "```aura\nmethod render(own self, value: int32) -> bool\n```"
    );

    method.receiver = Some(ReceiverKind::BorrowMut);
    assert_eq!(
        format_function_detail(&method),
        "render(mut self, value: int32) -> bool"
    );
    assert_eq!(
        format_method_hover(&method),
        "```aura\nmethod render(mut self, value: int32) -> bool\n```"
    );

    let completions = complete_source("def main():\n    pass\n", 0, 0, None)
        .expect("top-level completion should succeed");
    assert!(completions
        .iter()
        .any(|completion| completion.name == "own" && completion.kind == "keyword"));
}

#[test]
fn random_analysis_exposes_single_rng_binding_and_stateful_members() {
    let module_source = "import random\n\ndef main() -> int32:\n    random.\n    return 0\n";
    let module_items = complete_source(module_source, 3, 11, Some('.'))
        .expect("random module completion should recover");
    let rng_entries = module_items
        .iter()
        .filter(|item| item.name == "Rng")
        .collect::<Vec<_>>();
    assert_eq!(rng_entries.len(), 1, "Rng must have one completion entry");
    assert_eq!(rng_entries[0].kind, "class");
    assert_eq!(rng_entries[0].detail, "Rng(seed: int64)");
    assert!(module_items.iter().any(|item| item.name == "secure_int"));
    let secure_bytes = module_items
        .iter()
        .find(|item| item.name == "secure_bytes")
        .expect("secure_bytes completion");
    assert_eq!(secure_bytes.detail, "secure_bytes(n: int64) -> list[uint8]");
    assert!(!module_items.iter().any(|item| item.name == "secure_float"));

    let random_namespace =
        crate::builtin_modules::builtin_module_namespace(&["random".to_string()])
            .expect("random namespace");
    let rng_hover = format_class_hover(&random_namespace.classes["Rng"]);
    assert!(rng_hover.contains("class Rng(seed: int64)"));
    assert!(rng_hover.contains("deterministic"));

    let member_source = "import random\n\ndef main() -> int32:\n    mut rng = random.Rng(seed=1)\n    rng.\n    return 0\n";
    let members = complete_source(member_source, 4, 8, Some('.'))
        .expect("Rng member completion should recover");
    for (name, detail) in [
        ("next_int", "next_int(lo: int64, hi: int64) -> int64"),
        ("next_float", "next_float() -> float64"),
        ("shuffle", "shuffle(values: mut list[T]) -> None"),
    ] {
        let matches = members
            .iter()
            .filter(|item| item.name == name)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{name} should complete exactly once");
        assert_eq!(matches[0].kind, "method");
        assert_eq!(matches[0].detail, detail);
    }

    let imported_source = "from random import Rng\n\ndef main() -> int32:\n    mut rng: Rng = Rng(seed=1)\n    return 0\n";
    let top_level = complete_source(imported_source, 1, 0, None)
        .expect("imported Rng should participate in top-level completion");
    assert_eq!(
        top_level.iter().filter(|item| item.name == "Rng").count(),
        1,
        "an imported Rng class must not be duplicated by constructor metadata"
    );

    let imported_member_source = "from random import Rng\n\ndef main() -> int32:\n    mut rng: Rng = Rng(seed=1)\n    rng.\n    return 0\n";
    let imported_members = complete_source(imported_member_source, 4, 8, Some('.'))
        .expect("an imported Rng annotation should retain random-module provenance");
    for name in ["next_int", "next_float", "shuffle"] {
        assert_eq!(
            imported_members
                .iter()
                .filter(|item| item.name == name)
                .count(),
            1,
            "imported random.Rng should complete `{name}` exactly once"
        );
    }

    let result_source = r#"import random

def main() -> int32:
    mut rng = random.Rng(seed=1)
    roll = rng.next_int(lo=0, hi=10)
    fraction = rng.next_float()
    mut values = [1, 2]
    rng.shuffle(values)
    print(roll)
    print(fraction)
    return 0
"#;
    let result_analysis = analyze_source(result_source);
    assert!(
        result_analysis.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        result_analysis.diagnostics
    );
    for expected_hover in ["binding roll: int64", "binding fraction: float64"] {
        assert!(
            result_analysis
                .occurrences
                .iter()
                .any(|occurrence| occurrence.hover.contains(expected_hover)),
            "missing Rng result hover `{expected_hover}`"
        );
    }
}

#[test]
fn math_analysis_completes_and_hovers_the_exact_public_surface() {
    let member_source = "import math\n\ndef main():\n    math.\n";
    let completions = complete_source(member_source, 3, 9, Some('.'))
        .expect("math member completion should recover");
    let names = completions
        .iter()
        .filter(|completion| completion.kind == "function")
        .map(|completion| completion.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        std::collections::BTreeSet::from([
            "ceil", "cos", "exp", "floor", "log", "log10", "log2", "pow", "sin", "tan", "trunc",
        ])
    );
    let pow = completions
        .iter()
        .find(|completion| completion.name == "pow")
        .expect("math.pow completion should exist");
    assert_eq!(
        pow.detail,
        "pow(base: float64, exponent: float64) -> float64"
    );

    let source = "import math\n\ndef main():\n    print(math.pow(2.0, 3.0))\n";
    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 3
            && occurrence.hover.contains("function pow")
            && occurrence.hover.contains("float64")
    }));
}

#[test]
fn math_analysis_exposes_qualified_and_aliased_constant_details() {
    let member_source = "import math\n\ndef main():\n    math.\n";
    let completions = complete_source(member_source, 3, 9, Some('.'))
        .expect("math member completion should recover");
    for name in ["pi", "e", "inf", "nan"] {
        let completion = completions
            .iter()
            .find(|completion| completion.name == name)
            .unwrap_or_else(|| panic!("missing math.{name} completion"));
        assert_eq!(completion.kind, "constant");
        assert_eq!(completion.detail, "float64");
    }

    let source = concat!(
        "import math\n",
        "from math import pi as circle\n\n",
        "def main():\n",
        "    print(math.e)\n",
        "    print(circle)\n",
    );
    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 4
            && occurrence.hover.contains("module constant e")
            && occurrence.hover.contains("float64")
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 5
            && occurrence.hover.contains("module constant circle")
            && occurrence.hover.contains("float64")
    }));
}

#[test]
fn user_defined_rng_completion_uses_only_its_declared_surface() {
    let source = r#"import random

class Rng:
    value: int64

    def next_int(self) -> str:
        return "local"

def main() -> int32:
    rng = Rng(5)
    rng.
    return 0
"#;
    let members = complete_source(source, 10, 8, Some('.'))
        .expect("a user-defined Rng member completion should recover");

    let next_int = members
        .iter()
        .filter(|item| item.name == "next_int")
        .collect::<Vec<_>>();
    assert_eq!(next_int.len(), 1, "the local method must not be duplicated");
    assert_eq!(next_int[0].detail, "next_int(self) -> str");
    assert!(!members.iter().any(|item| item.name == "next_float"));
    assert!(!members.iter().any(|item| item.name == "shuffle"));
}

#[test]
fn path_named_random_keeps_user_rng_analysis_distinct_from_the_builtin() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run-pass/random.au");
    let source = fs::read_to_string(&path).expect("path-level user Rng fixture should be readable");
    let program = crate::check_path(&path).expect("path-level user Rng fixture should type check");
    let hover = format_class_hover(&program.classes["Rng"]);
    assert!(hover.contains("value: int64"));
    assert!(!hover.contains("seed: int64"));

    let analysis = analyze_path_source(&path, &source);
    assert!(
        analysis.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        analysis.diagnostics
    );
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("binding local: Rng")));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence
            .hover
            .contains("binding imported: user_rng_origin_support.random.Rng")
    }));
    assert!(!analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("class Rng(seed: int64)")));
}

#[test]
fn d6_analysis_renders_source_parameter_and_transfer_ownership() {
    let source = r#"
class Box:
    value: str

enum Message:
    Text(str)

def inspect(value: str):
    print(value)

def consume(value: own str):
    print(value)
"#;
    let completions = complete_source(source, 0, 0, None).expect("D6 source should complete");
    assert_eq!(
        completions
            .iter()
            .find(|item| item.name == "inspect")
            .map(|item| item.detail.as_str()),
        Some("inspect(value: str) -> None")
    );
    assert_eq!(
        completions
            .iter()
            .find(|item| item.name == "consume")
            .map(|item| item.detail.as_str()),
        Some("consume(value: own str) -> None")
    );
    assert_eq!(
        completions
            .iter()
            .find(|item| item.name == "Box")
            .map(|item| item.detail.as_str()),
        Some("Box(value: own str)")
    );

    let vec_members =
        builtin_member_completions(&Type::Named("list".to_string(), vec![Type::named("str")]));
    assert!(vec_members
        .iter()
        .any(|item| item.name == "append" && item.detail.contains("own T")));
    let message_members = builtin_enum_variant_completions("Option");
    assert!(message_members
        .iter()
        .any(|item| item.name == "Some" && item.detail.contains("own T")));
}

#[test]
fn d3_analysis_reports_canonical_int64_for_aliases_and_defaulted_expressions() {
    assert_eq!(lower_type_ref(&type_ref("int")), Type::named("int64"));

    let source = r#"
def main() -> int32:
    scalar = 1
    numbers = [1, 2]
    maybe = Option.Some(1)
    print(scalar)
    print(numbers.len())
    print(maybe != Option.None)
    return 0
"#;
    let output = analyze_source(source);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);

    for expected_hover in [
        "binding scalar: int64",
        "binding numbers: list[int64]",
        "binding maybe: Option[int64]",
    ] {
        assert!(
            output
                .occurrences
                .iter()
                .any(|occurrence| occurrence.hover.contains(expected_hover)),
            "missing hover `{expected_hover}` in {:?}",
            output.occurrences
        );
    }
}

#[test]
fn phase6_analysis_specializes_vec_map_and_explicit_generic_module_results() {
    let source = r#"import control

def render(value: int64) -> str:
    return str(value)

def worker() -> Result[int32, str]:
    return Result.Ok(7)

def main():
    values = [1, 2]
    mapped = values.map(render)
    retried = control.retry[int32, str](worker)
    print(mapped)
    print(retried)
"#;
    let output = analyze_source(source);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);

    for expected_hover in [
        "binding mapped: list[str]",
        "binding retried: Result[int32, str]",
    ] {
        assert!(
            output
                .occurrences
                .iter()
                .any(|occurrence| occurrence.hover.contains(expected_hover)),
            "missing hover `{expected_hover}` in {:?}",
            output.occurrences
        );
    }
}

#[test]
fn array_analysis_infers_results_and_exposes_constructor_and_member_completions() {
    let source = r#"
def widen(value: int32) -> float64:
    return value.to_float()

def main():
    zeros = Array[int32].zeros(shape=[2, 2])
    full = Array[int32].full(shape=[2, 2], value=1)
    source: list[int32] = [1, 2, 3, 4]
    mut values = Array[int32].from_list(values=source, shape=[2, 2])
    mapped = values.map[float64](f=widen)
    average = values.mean()
    count = values.len()
    copied = values.clone()
    maybe = values.get([0, 1])
    previous = values.set([0, 1], 7)
    values.fill(0)
    wrapped = values.wrapping_add(1)
    scalar: int32 = 7
    scalar_wrapped = scalar.wrapping_add(1)
    builtin_count = len(source)
    item = values[0, 1]
    rows = values[:1]
    print(mapped)
    print(average)
    print(count)
    print(copied)
    print(maybe)
    print(previous)
    print(wrapped)
    print(scalar_wrapped)
    print(item)
    print(rows)
"#;
    let output = analyze_source(source);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    for expected_hover in [
        "binding zeros: Array[int32]",
        "binding full: Array[int32]",
        "binding values: Array[int32]",
        "binding mapped: Array[float64]",
        "binding average: float64",
        "binding count: int64",
        "binding copied: Array[int32]",
        "binding maybe: Option[int32]",
        "binding previous: Option[int32]",
        "binding wrapped: Array[int32]",
        "binding scalar_wrapped: int32",
        "binding builtin_count: int64",
        "binding item: int32",
        "binding rows: Array[int32]",
    ] {
        assert!(
            output
                .occurrences
                .iter()
                .any(|occurrence| occurrence.hover.contains(expected_hover)),
            "missing hover `{expected_hover}` in {:?}",
            output.occurrences
        );
    }

    let constructor_source =
        "def main():\n    values = Array[int32].zeros(shape=[1])\n    Array[int32].\n";
    let constructors = complete_source(constructor_source, 2, 17, Some('.'))
        .expect("Array associated completion should recover");
    for name in ["zeros", "full", "from_list"] {
        assert!(
            constructors.iter().any(|item| item.name == name),
            "missing Array constructor completion `{name}`"
        );
    }

    let member_source = "def main():\n    values = Array[int32].zeros(shape=[1])\n    values.\n";
    let members = complete_source(member_source, 2, 11, Some('.'))
        .expect("Array member completion should recover");
    for name in [
        "shape",
        "len",
        "clone",
        "get",
        "set",
        "fill",
        "map",
        "sum",
        "min",
        "max",
        "mean",
        "wrapping_add",
        "saturating_mul",
    ] {
        assert!(
            members.iter().any(|item| item.name == name),
            "missing Array member completion `{name}`"
        );
    }
    let float_members = builtin_member_completions(&Type::Named(
        "Array".to_string(),
        vec![Type::named("float64")],
    ));
    assert!(!float_members.iter().any(|item| {
        matches!(
            item.name.as_str(),
            "wrapping_add"
                | "wrapping_sub"
                | "wrapping_mul"
                | "saturating_add"
                | "saturating_sub"
                | "saturating_mul"
        )
    }));

    let top_level = complete_source("def main():\n    pass\n", 0, 0, None)
        .expect("top-level Array completion should succeed");
    assert!(top_level
        .iter()
        .any(|item| item.name == "Array" && item.kind == "class"));
}

#[test]
fn s1_frontend_specialized_collection_completion_reports_the_concrete_result_type() {
    for (receiver, expected_detail) in [
        (
            "list[int64]",
            "with_capacity(minimum: int64) -> list[int64]",
        ),
        (
            "dict[str, int64]",
            "with_capacity(minimum: int64) -> dict[str, int64]",
        ),
        ("set[str]", "with_capacity(minimum: int64) -> set[str]"),
    ] {
        let source = format!("def main():\n    {receiver}.\n");
        let line = source.lines().nth(1).expect("completion line");
        let character = line.find('.').expect("receiver dot") + 1;
        let completions = complete_source(&source, 1, character, Some('.'))
            .expect("specialized collection completion should recover");
        let with_capacity = completions
            .iter()
            .find(|item| item.name == "with_capacity")
            .expect("with_capacity completion should exist");
        assert_eq!(with_capacity.detail, expected_detail, "{receiver}");
    }
}

#[test]
fn s1_frontend_analysis_resolves_trait_symbols_and_integer_round_results() {
    let source = "trait Show:\n    def show(self) -> str\n\nclass Item:\n    value: int64\n\nimpl Show for Item:\n    def show(self) -> str:\n        return self.value.to_string()\n\ndef main():\n    rounded = round(7)\n    print(rounded)\n";
    let program = checked_program(source);
    let builder = AnalysisBuilder::new(source, &program, Vec::new());
    let resolved = builder
        .resolve_name("Show", &BTreeMap::new())
        .expect("trait names must resolve for hover and definition");
    assert_eq!(resolved.hover, "```aura\ntrait Show\n```");
    assert_eq!(
        resolved.definition.expect("trait definition"),
        range_from_span(Span::new(1, 1), "Show".len())
    );

    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("binding rounded: int64")));
}

#[test]
fn s1_frontend_range_loop_analysis_matches_the_int64_index_domain() {
    let source = concat!(
        "def main():\n",
        "    for index in range(0, 3):\n",
        "        checked: int64 = index\n",
        "        advanced = index.wrapping_add(1)\n",
        "        print(checked)\n",
        "        print(advanced)\n",
    );
    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "the checker must accept range bindings as int64: {:?}",
        analysis.diagnostics
    );
    for expected_hover in ["local index: int64", "binding advanced: int64"] {
        assert!(
            analysis
                .occurrences
                .iter()
                .any(|occurrence| occurrence.hover.contains(expected_hover)),
            "missing range-loop hover `{expected_hover}` in {:?}",
            analysis.occurrences
        );
    }

    let completion_source = "def main():\n    for index in range(0, 3):\n        index.\n";
    let member_line = completion_source.lines().nth(2).unwrap();
    let completions = complete_source(completion_source, 2, member_line.len(), Some('.'))
        .expect("range binding completion should recover from a dangling member");
    assert!(
        completions
            .iter()
            .any(|completion| completion.name == "wrapping_add"),
        "range bindings must expose the fixed-width int64 member surface: {completions:?}"
    );
}

#[test]
fn incomplete_expression_inference_preserves_stable_editor_types() {
    let program = checked_program("def main():\n    pass\n");
    let builder = AnalysisBuilder::new("", &program, Vec::new());
    let binding = |ty: Type| super::BindingInfo {
        ty,
        trait_bounds: Vec::new(),
        definition: super::AnalysisRange {
            file_path: None,
            line: 0,
            start_character: 0,
            end_character: 1,
        },
        hover: String::new(),
    };
    let scope = BTreeMap::from([
        ("unit".to_string(), binding(Type::Unit)),
        ("number".to_string(), binding(Type::named("int32"))),
        ("text".to_string(), binding(Type::named("str"))),
        (
            "pair".to_string(),
            binding(Type::Tuple(vec![Type::named("int32"), Type::named("str")])),
        ),
        (
            "vector".to_string(),
            binding(Type::Named("list".to_string(), vec![Type::named("int32")])),
        ),
        (
            "array".to_string(),
            binding(Type::Named("Array".to_string(), vec![Type::named("int32")])),
        ),
        ("not_callable".to_string(), binding(Type::named("int32"))),
    ]);
    let conditional = |then_name: &str, else_name: &str| {
        expr(ExprKind::Conditional {
            then_expr: Box::new(expr(ExprKind::Name(then_name.to_string()))),
            condition: Box::new(expr(ExprKind::Bool(true))),
            else_expr: Box::new(expr(ExprKind::Name(else_name.to_string()))),
        })
    };

    assert_eq!(
        builder.infer_expr_type(&conditional("unit", "number"), &scope),
        Some(Type::named("int32")),
        "a contextual None arm must not erase the concrete editor type"
    );
    assert_eq!(
        builder.infer_expr_type(&conditional("number", "unit"), &scope),
        Some(Type::named("int32")),
        "a trailing contextual None arm must preserve the concrete editor type"
    );
    assert_eq!(
        builder.infer_expr_type(&conditional("number", "text"), &scope),
        Some(Type::named("int32")),
        "during incomplete editing, incompatible arms retain the leading inferred type"
    );

    let dynamic_tuple_index = expr(ExprKind::Index {
        object: Box::new(expr(ExprKind::Name("pair".to_string()))),
        index: Box::new(expr(ExprKind::Name("number".to_string()))),
    });
    assert_eq!(
        builder.infer_expr_type(&dynamic_tuple_index, &scope),
        None,
        "analysis must not invent a tuple element type for a dynamic index"
    );

    for collection in ["vector", "array"] {
        let invalid_map = expr(ExprKind::Call {
            callee: Box::new(expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name(collection.to_string()))),
                field: "map".to_string(),
            })),
            args: vec![arg(expr(ExprKind::Name("not_callable".to_string())))],
        });
        assert_eq!(
            builder.infer_expr_type(&invalid_map, &scope),
            None,
            "{collection}.map must not advertise a result type for a non-callable callback"
        );
    }
}

#[test]
fn machine_readable_analysis_covers_symbols_and_occurrences() {
    let source = include_str!("../../../examples/point.au");
    let analysis = analyze_source(source);

    assert!(analysis.diagnostics.is_empty());
    assert!(analysis
        .symbols
        .iter()
        .any(|symbol| symbol.kind == "class" && symbol.name == "Point"));
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("sqrt() -> float64")));
}

#[test]
fn p64_extern_items_are_preserved_in_machine_readable_symbols() {
    let module = crate::parser::parse(concat!(
        "public extern \"C\" opaque class ProcessHandle\n",
        "public extern \"C\" def getpid() -> int32\n",
    ))
    .expect("extern declarations should parse");
    let symbols = symbols_from_module(&module);

    assert!(symbols.iter().any(|symbol| {
        symbol.kind == "class"
            && symbol.name == "ProcessHandle"
            && symbol.detail == "extern \"C\" opaque"
    }));
    assert!(symbols.iter().any(|symbol| {
        symbol.kind == "function"
            && symbol.name == "getpid"
            && symbol.detail == "extern \"C\" -> int32"
    }));

    let serialized = serde_json::to_value(&symbols).expect("analysis symbols should serialize");
    assert!(serialized.to_string().contains("ProcessHandle"));
    assert!(serialized.to_string().contains("getpid"));
}

#[test]
fn p64_path_analysis_exposes_local_extern_completions_hovers_and_definitions() {
    let temp_dir = TempDir::new("aura-analysis-local-ffi");
    let main_path = temp_dir.path().join("src/main.au");
    fs::create_dir_all(main_path.parent().expect("main path should have a parent"))
        .expect("failed to create package source dir");
    fs::write(
        temp_dir.path().join("Aura.toml"),
        concat!(
            "[package]\n",
            "name = \"ffi_analysis\"\n",
            "version = \"0.1.0\"\n",
            "edition = \"2026\"\n",
            "allow_ffi = true\n",
        ),
    )
    .expect("failed to write package manifest");
    let source = concat!(
        "public extern \"C\" opaque class Handle\n",
        "public extern \"C\" def acquire() -> Handle\n",
        "public extern \"C\" def release(handle: own Handle) -> int32\n",
        "\n",
        "def main() -> int32:\n",
        "    handle = acquire()\n",
        "    return release(handle)\n",
    );
    fs::write(&main_path, source).expect("failed to write FFI analysis source");

    let completions = complete_path_source(&main_path, source, 3, 0, None)
        .expect("an opted-in FFI package should provide completions");
    assert!(completions.iter().any(|completion| {
        completion.name == "Handle"
            && completion.kind == "class"
            && completion.detail == "extern \"C\" opaque class"
    }));
    assert!(completions.iter().any(|completion| {
        completion.name == "acquire"
            && completion.kind == "function"
            && completion.detail == "extern \"C\" acquire() -> Handle"
    }));
    assert!(completions.iter().any(|completion| {
        completion.name == "release"
            && completion.kind == "function"
            && completion.detail == "extern \"C\" release(handle: own Handle) -> int32"
    }));

    let program = crate::check_path(&main_path).expect("the local FFI source should type check");
    let builder = AnalysisBuilder::new(source, &program, Vec::new());
    let handle = builder
        .resolve_name("Handle", &BTreeMap::new())
        .expect("the local opaque handle should resolve");
    assert_eq!(
        handle.hover,
        "```aura\nextern \"C\" opaque class Handle\n```"
    );
    let handle_definition = handle
        .definition
        .expect("the local opaque handle should have a definition");
    assert_eq!(handle_definition.line, 0);
    assert_eq!(handle_definition.start_character, 31);
    assert_eq!(handle_definition.end_character, 37);

    let analysis = analyze_path_source(&main_path, source);
    assert!(
        analysis.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        analysis.diagnostics
    );
    for (name, hover, definition_line) in [
        ("acquire", "extern \"C\" function acquire() -> Handle", 1),
        (
            "release",
            "extern \"C\" function release(handle: own Handle) -> int32",
            2,
        ),
    ] {
        let occurrence = analysis
            .occurrences
            .iter()
            .find(|occurrence| occurrence.hover.contains(hover))
            .unwrap_or_else(|| panic!("missing `{name}` FFI occurrence"));
        let definition = occurrence
            .definition
            .as_ref()
            .unwrap_or_else(|| panic!("missing `{name}` FFI definition"));
        assert_eq!(definition.line, definition_line);
        assert_eq!(
            definition.file_path.as_deref(),
            Some(
                fs::canonicalize(&main_path)
                    .expect("main path should canonicalize")
                    .display()
                    .to_string()
                    .as_str()
            )
        );
    }
}

#[test]
fn p64_path_analysis_exposes_imported_extern_members_and_handle_types() {
    let temp_dir = TempDir::new("aura-analysis-imported-ffi");
    let source_dir = temp_dir.path().join("src");
    fs::create_dir_all(&source_dir).expect("failed to create package source dir");
    fs::write(
        temp_dir.path().join("Aura.toml"),
        concat!(
            "[package]\n",
            "name = \"ffi_analysis\"\n",
            "version = \"0.1.0\"\n",
            "edition = \"2026\"\n",
            "allow_ffi = true\n",
        ),
    )
    .expect("failed to write package manifest");
    let native_path = source_dir.join("native.au");
    fs::write(
        &native_path,
        concat!(
            "public extern \"C\" opaque class Handle\n",
            "public extern \"C\" def acquire() -> Handle\n",
            "public extern \"C\" def release(handle: own Handle) -> int32\n",
        ),
    )
    .expect("failed to write imported FFI module");
    let main_path = source_dir.join("main.au");
    let source = concat!(
        "import native\n",
        "\n",
        "def main() -> int32:\n",
        "    handle = native.acquire()\n",
        "    return native.release(handle)\n",
    );
    fs::write(&main_path, source).expect("failed to write importing module");

    let completion_source = concat!(
        "import native\n",
        "\n",
        "def main() -> int32:\n",
        "    native.\n",
        "    return 0\n",
    );
    let completions = complete_path_source(&main_path, completion_source, 3, 11, Some('.'))
        .expect("an imported FFI module should provide member completions");
    assert!(completions.iter().any(|completion| {
        completion.name == "Handle"
            && completion.kind == "class"
            && completion.detail == "extern \"C\" opaque class"
    }));
    assert!(completions.iter().any(|completion| {
        completion.name == "acquire"
            && completion.kind == "function"
            && completion.detail == "extern \"C\" acquire() -> Handle"
    }));
    assert!(completions.iter().any(|completion| {
        completion.name == "release"
            && completion.kind == "function"
            && completion.detail == "extern \"C\" release(handle: own Handle) -> int32"
    }));

    let program = crate::check_path(&main_path).expect("the importing source should type check");
    let builder = AnalysisBuilder::new(source, &program, Vec::new());
    let handle = builder
        .resolve_member_type(&Type::Module("native".to_string()), "Handle")
        .expect("the imported opaque handle should resolve as a module member");
    assert_eq!(
        handle.hover,
        "```aura\nextern \"C\" opaque class Handle\n```"
    );
    assert_eq!(handle.ty, Some(Type::named("native.Handle")));
    let handle_definition = handle
        .definition
        .expect("the imported opaque handle should have a definition");
    assert_eq!(handle_definition.line, 0);
    assert_eq!(handle_definition.start_character, 31);
    assert_eq!(handle_definition.end_character, 37);

    let analysis = analyze_path_source(&main_path, source);
    assert!(
        analysis.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        analysis.diagnostics
    );
    let canonical_native_path = fs::canonicalize(&native_path)
        .expect("native module should canonicalize")
        .display()
        .to_string();
    for hover in [
        "extern \"C\" function acquire() -> Handle",
        "extern \"C\" function release(handle: own Handle) -> int32",
    ] {
        assert!(analysis.occurrences.iter().any(|occurrence| {
            occurrence.hover.contains(hover)
                && occurrence
                    .definition
                    .as_ref()
                    .and_then(|definition| definition.file_path.as_deref())
                    == Some(canonical_native_path.as_str())
        }));
    }
}

#[test]
fn import_alias_analysis_preserves_visible_names_and_canonical_definitions() {
    let temp_dir = TempDir::new("aura-analysis-import-aliases");
    let source_dir = temp_dir.path().join("src");
    let package_dir = source_dir.join("pkg");
    fs::create_dir_all(&package_dir).expect("failed to create package source directories");
    fs::write(
        temp_dir.path().join("Aura.toml"),
        concat!(
            "[package]\n",
            "name = \"alias_analysis\"\n",
            "version = \"0.1.0\"\n",
            "edition = \"2026\"\n",
        ),
    )
    .expect("failed to write package manifest");
    let math_path = package_dir.join("math.au");
    fs::write(
        &math_path,
        concat!(
            "public def double(value: int32) -> int32:\n",
            "    return value * 2\n",
        ),
    )
    .expect("failed to write imported module");
    let canonical_math_path = fs::canonicalize(&math_path)
        .expect("imported module path should canonicalize")
        .display()
        .to_string();
    let main_path = source_dir.join("main.au");

    let module_alias_source = concat!(
        "import pkg.math as numbers\n",
        "\n",
        "def main() -> int32:\n",
        "    return numbers.double(21)\n",
    );
    fs::write(&main_path, module_alias_source).expect("failed to write module-alias source");
    let analysis = analyze_path_source(&main_path, module_alias_source);
    assert!(
        analysis.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        analysis.diagnostics
    );
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 0
            && occurrence.hover.contains("module numbers = pkg.math")
            && occurrence
                .definition
                .as_ref()
                .and_then(|definition| definition.file_path.as_deref())
                == Some(canonical_math_path.as_str())
    }));
    for hover in ["module numbers = pkg.math", "function double"] {
        assert!(analysis.occurrences.iter().any(|occurrence| {
            occurrence.line == 3
                && occurrence.hover.contains(hover)
                && occurrence
                    .definition
                    .as_ref()
                    .and_then(|definition| definition.file_path.as_deref())
                    == Some(canonical_math_path.as_str())
        }));
    }

    let top_level = complete_path_source(&main_path, module_alias_source, 2, 0, None)
        .expect("module alias should participate in completion");
    assert!(top_level
        .iter()
        .any(|completion| completion.name == "numbers" && completion.kind == "module"));
    assert!(!top_level.iter().any(|completion| completion.name == "math"));

    let member_source = concat!(
        "import pkg.math as numbers\n",
        "\n",
        "def main() -> int32:\n",
        "    numbers.\n",
        "    return 0\n",
    );
    let members = complete_path_source(&main_path, member_source, 3, 12, Some('.'))
        .expect("module alias should retain its imported namespace");
    assert!(members
        .iter()
        .any(|completion| completion.name == "double" && completion.kind == "function"));

    let binding_alias_source = concat!(
        "from pkg.math import double as twice\n",
        "\n",
        "def main() -> int32:\n",
        "    return twice(21)\n",
    );
    fs::write(&main_path, binding_alias_source).expect("failed to write binding-alias source");
    let binding_analysis = analyze_path_source(&main_path, binding_alias_source);
    assert!(
        binding_analysis.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        binding_analysis.diagnostics
    );
    assert!(binding_analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 0
            && occurrence
                .hover
                .contains("Alias `twice` for `pkg.math.double`")
            && occurrence
                .definition
                .as_ref()
                .and_then(|definition| definition.file_path.as_deref())
                == Some(canonical_math_path.as_str())
    }));
    assert!(binding_analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 3
            && occurrence.hover.contains("function double")
            && occurrence
                .definition
                .as_ref()
                .and_then(|definition| definition.file_path.as_deref())
                == Some(canonical_math_path.as_str())
    }));
    let binding_completions = complete_path_source(&main_path, binding_alias_source, 2, 0, None)
        .expect("from-import alias should participate in completion");
    assert!(binding_completions
        .iter()
        .any(|completion| completion.name == "twice" && completion.kind == "function"));
    assert!(!binding_completions
        .iter()
        .any(|completion| completion.name == "double"));
}

#[test]
fn builtin_module_alias_analysis_uses_the_visible_name_and_canonical_members() {
    let source = concat!(
        "import path as paths\n",
        "\n",
        "def main() -> int32:\n",
        "    print(paths.join(\"root\", \"item.au\"))\n",
        "    return 0\n",
    );
    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        analysis.diagnostics
    );
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 0 && occurrence.hover.contains("module paths = path")
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 3 && occurrence.hover.contains("module paths = path")
    }));
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| { occurrence.line == 3 && occurrence.hover.contains("function join") }));

    let top_level = complete_source(source, 2, 0, None).expect("builtin alias completion");
    assert!(top_level
        .iter()
        .any(|completion| completion.name == "paths" && completion.kind == "module"));
    assert!(!top_level.iter().any(|completion| completion.name == "path"));

    let member_source =
        source.replace("    print(paths.join(\"root\", \"item.au\"))", "    paths.");
    let members =
        complete_source(&member_source, 3, 10, Some('.')).expect("builtin alias member completion");
    assert!(members
        .iter()
        .any(|completion| completion.name == "join" && completion.kind == "function"));
}

#[test]
fn d3_assert_analysis_visits_condition_and_lazy_message_without_defining_scope() {
    let source = r#"
def verify(ready: bool, message: str):
    assert ready, message
    assert ready
"#;
    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    // Analysis positions are zero-based: these are the uses on source lines
    // three and four, not the parameter declarations on source line two.
    for (line, start, end, hover) in [
        (2, 11, 16, "param ready: bool"),
        (2, 18, 25, "param message: str"),
        (3, 11, 16, "param ready: bool"),
    ] {
        assert!(
            analysis.occurrences.iter().any(|occurrence| {
                occurrence.line == line
                    && occurrence.start_character == start
                    && occurrence.end_character == end
                    && occurrence.hover.contains(hover)
            }),
            "missing assertion-use occurrence at {line}:{start}-{end}"
        );
    }

    let module = crate::parser::parse(source).expect("assertions should parse");
    let crate::ast::Item::Function(function) = &module.items[0] else {
        panic!("expected function");
    };
    assert_eq!(stmt_start_line(&function.body[0]), 3);
    assert_eq!(stmt_end_line(&function.body[0]), 3);

    let completions =
        complete_source("", 0, 0, None).expect("top-level completion should remain available");
    assert!(completions
        .iter()
        .any(|completion| completion.kind == "keyword" && completion.name == "assert"));
}

#[test]
fn conditional_expression_analysis_visits_all_operands_and_keeps_result_type() {
    let source = r#"def choose(ready: bool, left: str, right: str) -> str:
    selected = left.clone() if ready else right.clone()
    return selected
"#;
    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    for (start, end, hover) in [
        (15, 19, "param left: str"),
        (31, 36, "param ready: bool"),
        (42, 47, "param right: str"),
    ] {
        assert!(
            analysis.occurrences.iter().any(|occurrence| {
                occurrence.line == 1
                    && occurrence.start_character == start
                    && occurrence.end_character == end
                    && occurrence.hover.contains(hover)
            }),
            "missing conditional operand occurrence at {start}-{end}"
        );
    }
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("binding selected: str")));
}

#[test]
fn conditional_expression_analysis_uses_the_contextual_arm_type() {
    let source = r#"def choose(
    ready: bool,
    exact_float: float32,
    reverse_float: float32,
    exact_integer: int32,
    reverse_integer: int32,
    values: own list[int32],
    reverse_values: own list[int32],
    tuple_values: own list[int32]
):
    decimal = (1.5) if ready else exact_float
    reverse_decimal = reverse_float if ready else (2.5)
    promoted_integer = 1 if ready else exact_float
    reverse_promoted_integer = reverse_float if ready else 2
    negative_integer = (-1) if ready else exact_integer
    reverse_negative_integer = reverse_integer if ready else (-2)
    integers = [] if ready else values
    reverse_integers = reverse_values if ready else []
    nested_integers = ([], 1) if ready else (tuple_values, 2)
    optional = None if ready else Option.Some(exact_integer)
    reverse_optional = Option.Some(reverse_integer) if ready else None
"#;
    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    for expected_hover in [
        "binding decimal: float32",
        "binding reverse_decimal: float32",
        "binding promoted_integer: float32",
        "binding reverse_promoted_integer: float32",
        "binding negative_integer: int32",
        "binding reverse_negative_integer: int32",
        "binding integers: list[int32]",
        "binding reverse_integers: list[int32]",
        "binding nested_integers: (list[int32], int64)",
        "binding optional: Option[int32]",
        "binding reverse_optional: Option[int32]",
    ] {
        assert!(
            analysis
                .occurrences
                .iter()
                .any(|occurrence| occurrence.hover.contains(expected_hover)),
            "missing conditional result hover `{expected_hover}` in {:?}",
            analysis.occurrences
        );
    }
}

#[test]
fn membership_and_comparison_chain_operands_keep_analysis_coverage() {
    let source = r#"
def probe(ports: list[int32], port: int32, low: int32, high: int32):
    present = port in ports
    absent = port not in ports
    bounded = low <= port < high
    print(present)
    print(absent)
    print(bounded)
"#;
    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    for expected_hover in [
        "binding present: bool",
        "binding absent: bool",
        "binding bounded: bool",
        "param ports: list[int32]",
        "param port: int32",
        "param low: int32",
        "param high: int32",
    ] {
        assert!(
            analysis
                .occurrences
                .iter()
                .any(|occurrence| occurrence.hover.contains(expected_hover)),
            "missing membership or chain hover `{expected_hover}` in {:?}",
            analysis.occurrences
        );
    }
}

#[test]
fn conditional_expression_result_type_drives_member_completion() {
    for source in [
        "def inspect(flag: bool, values: own list[int32]):\n    selected = [] if flag else values\n    selected.\n",
        "def inspect(flag: bool, values: own list[int32]):\n    selected = values if flag else []\n    selected.\n",
    ] {
        let line_index = source
            .lines()
            .position(|line| line.contains("selected."))
            .expect("completion source should contain a member marker");
        let character = source.lines().nth(line_index).unwrap().len();
        let completions = complete_source(source, line_index, character, Some('.'))
            .expect("conditional result member completion should work");
        assert!(
            completions.iter().any(|item| item.name == "append"),
            "list completion should be preserved through either conditional arm"
        );
        assert!(completions.iter().any(|item| item.name == "len"));
    }
}

#[test]
fn machine_readable_analysis_reports_diagnostics() {
    let source = "def main():\n    print(total)\n";
    let analysis = analyze_source(source);

    assert_eq!(analysis.diagnostics.len(), 1);
    assert_eq!(analysis.diagnostics[0].code, "AU2001");
    assert_eq!(analysis.diagnostics[0].severity, 1);
    assert!(analysis.diagnostics[0].secondary_spans.is_empty());
    assert!(analysis.diagnostics[0].notes.is_empty());
    assert!(analysis.diagnostics[0].help.is_empty());
    assert!(analysis.diagnostics[0].edits.is_empty());
    assert!(analysis.diagnostics[0].call_frames.is_empty());
    assert!(analysis.diagnostics[0].task_ancestry.is_empty());
    assert!(analysis.diagnostics[0]
        .message
        .contains("unknown name `total`"));

    let serialized = serde_json::to_value(&analysis).expect("analysis should serialize");
    assert_eq!(
        serialized["diagnostics"][0]["call_frames"],
        serde_json::json!([])
    );
    assert_eq!(
        serialized["diagnostics"][0]["task_ancestry"],
        serde_json::json!([])
    );
}

#[test]
fn machine_readable_analysis_preserves_zero_based_runtime_frames() {
    let mut diagnostic =
        Diagnostic::coded_at("AU4003", Span::new(9, 18), "list index is out of bounds");
    assert!(diagnostic.capture_runtime_frames_once(
        vec![
            RuntimeCallFrame {
                function: "worker.child".to_string(),
                span: RuntimeSourceSpan::point(
                    Some("/workspace/worker.au".to_string()),
                    Span::new(3, 5),
                ),
            },
            RuntimeCallFrame {
                function: "source_only".to_string(),
                span: RuntimeSourceSpan::point(None, Span::new(1, 1)),
            },
        ],
        vec![RuntimeTaskFrame {
            task_function: "worker.child".to_string(),
            task_entry_span: RuntimeSourceSpan::point(
                Some("/workspace/worker.au".to_string()),
                Span::new(3, 5),
            ),
            parent_function: "main".to_string(),
            spawn_span: RuntimeSourceSpan::point(
                Some("/workspace/main.au".to_string()),
                Span::new(8, 15),
            ),
        }],
    ));

    let analysis = analysis_diagnostic(&diagnostic);
    assert_eq!(analysis.call_frames.len(), 2);
    assert_eq!(analysis.call_frames[0].function, "worker.child");
    assert_eq!(
        (
            analysis.call_frames[0].span.file_path.as_deref(),
            analysis.call_frames[0].span.line,
            analysis.call_frames[0].span.start_character,
            analysis.call_frames[0].span.end_character,
        ),
        (Some("/workspace/worker.au"), 2, 4, 5)
    );
    assert_eq!(analysis.call_frames[1].function, "source_only");
    assert_eq!(analysis.call_frames[1].span.file_path, None);
    assert_eq!(
        (
            analysis.call_frames[1].span.line,
            analysis.call_frames[1].span.start_character,
            analysis.call_frames[1].span.end_character,
        ),
        (0, 0, 1)
    );

    assert_eq!(analysis.task_ancestry.len(), 1);
    let task = &analysis.task_ancestry[0];
    assert_eq!(task.task_function, "worker.child");
    assert_eq!(task.parent_function, "main");
    assert_eq!(
        (
            task.task_entry_span.file_path.as_deref(),
            task.task_entry_span.line,
            task.task_entry_span.start_character,
            task.task_entry_span.end_character,
        ),
        (Some("/workspace/worker.au"), 2, 4, 5)
    );
    assert_eq!(
        (
            task.spawn_span.file_path.as_deref(),
            task.spawn_span.line,
            task.spawn_span.start_character,
            task.spawn_span.end_character,
        ),
        (Some("/workspace/main.au"), 7, 14, 15)
    );

    let cloned = analysis.clone();
    assert_eq!(cloned, analysis);
    let serialized = serde_json::to_value(&analysis).expect("analysis should serialize");
    assert_eq!(
        serialized["call_frames"][0]["span"]["file_path"],
        "/workspace/worker.au"
    );
    assert_eq!(
        serialized["task_ancestry"][0]["spawn_span"]["line"],
        serde_json::json!(7)
    );
}

#[test]
fn queue_timeout_analysis_does_not_treat_queue_receive_or_ms_as_unknown() {
    let source = "def main() -> int32:\n    jobs = Queue[int32]()\n    match jobs.get(timeout=5ms):\n        case QueueReceive.Item(value):\n            print(value)\n        case QueueReceive.TimedOut:\n            print(\"timeout\")\n        case _:\n            pass\n    return 0\n";
    let analysis = analyze_source(source);

    assert!(analysis.diagnostics.is_empty());
    assert!(analysis.occurrences.iter().any(|occurrence| occurrence
        .hover
        .contains("get(timeout: Duration = ...) -> QueueReceive[T]")));
}

#[test]
fn compiler_member_completion_returns_class_fields() {
    let source = include_str!("../../../examples/point.au");
    let line_index = source
        .lines()
        .position(|line| line.contains("a.x"))
        .expect("point example should contain member access");
    let line_text = source.lines().nth(line_index).unwrap();
    let character = line_text.find('.').unwrap() + 1;

    let completions =
        complete_source(source, line_index, character, Some('.')).expect("completion should work");
    let names = completions
        .into_iter()
        .map(|item| item.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"x".to_string()));
    assert!(names.contains(&"y".to_string()));
}

#[test]
fn compiler_member_completion_for_string_exposes_string_methods() {
    let source = "def main() -> int32:\n    text = \"  aura  \"\n    text.\n    return 0\n";
    let line_index = source
        .lines()
        .position(|line| line.contains("text."))
        .expect("string clone example should contain member access");
    let line_text = source.lines().nth(line_index).unwrap();
    let character = line_text.find('.').unwrap() + 1;

    let completions =
        complete_source(source, line_index, character, Some('.')).expect("completion should work");
    let names = completions
        .into_iter()
        .map(|item| item.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"len".to_string()));
    assert!(names.contains(&"byte_len".to_string()));
    assert!(names.contains(&"contains".to_string()));
    assert!(names.contains(&"starts_with".to_string()));
    assert!(names.contains(&"ends_with".to_string()));
    assert!(names.contains(&"trim".to_string()));
    assert!(names.contains(&"split".to_string()));
    assert!(names.contains(&"replace".to_string()));
    assert!(names.contains(&"to_lower".to_string()));
    assert!(names.contains(&"to_upper".to_string()));
    assert!(names.contains(&"strip_prefix".to_string()));
    assert!(names.contains(&"strip_suffix".to_string()));
    assert!(names.contains(&"to_bytes".to_string()));
    assert!(names.contains(&"clone".to_string()));
    assert!(!names.contains(&"from_bytes".to_string()));
    assert!(!names.contains(&"as_str".to_string()));
}

#[test]
fn analysis_and_completion_report_public_length_members_as_int64() {
    let program = checked_program("def main():\n    pass\n");
    let builder = AnalysisBuilder::new("", &program, Vec::new());
    let cases = [
        (Type::named("str"), vec!["len", "byte_len"]),
        (
            Type::Named("list".to_string(), vec![Type::named("str")]),
            vec!["len"],
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            vec!["len"],
        ),
        (
            Type::Named("set".to_string(), vec![Type::named("str")]),
            vec!["len"],
        ),
    ];

    for (receiver, fields) in cases {
        let completions = builder.member_completions(&receiver);
        for (index, completion) in completions.iter().enumerate() {
            assert!(
                completions[..index]
                    .iter()
                    .all(|earlier| earlier.name != completion.name),
                "{receiver}.{} must complete exactly once: {completions:?}",
                completion.name
            );
        }
        for field in fields {
            let expected_detail = format!("{field}() -> int64");
            let member = builder
                .resolve_member_type(&receiver, field)
                .unwrap_or_else(|| panic!("expected public member {receiver}.{field}"));
            assert_eq!(
                member.ty,
                Some(Type::named("int64")),
                "{receiver}.{field} must analyze as int64"
            );

            let matching_completions = completions
                .iter()
                .filter(|item| item.name == field)
                .collect::<Vec<_>>();
            assert_eq!(
                matching_completions.len(),
                1,
                "{receiver}.{field} must complete exactly once: {matching_completions:?}"
            );
            let completion = matching_completions[0];
            assert_eq!(completion.kind, "method", "{receiver}.{field}");
            assert_eq!(completion.detail, expected_detail, "{receiver}.{field}");
        }
    }
}

#[test]
fn analysis_reports_collection_search_and_projection_result_types() {
    let program = checked_program("def main():\n    pass\n");
    let builder = AnalysisBuilder::new("", &program, Vec::new());
    let values = Type::Named("list".to_string(), vec![Type::named("int64")]);
    let mapping = Type::Named(
        "dict".to_string(),
        vec![Type::named("str"), Type::named("int64")],
    );

    for member_name in ["index", "count"] {
        let member = builder
            .resolve_member_type(&values, member_name)
            .unwrap_or_else(|| panic!("list.{member_name} should resolve for editor analysis"));
        assert_eq!(member.ty, Some(Type::named("int64")), "list.{member_name}");
    }
    assert_eq!(
        builder
            .resolve_member_type(&mapping, "keys")
            .expect("dict.keys should resolve for editor analysis")
            .ty,
        Some(Type::Named("list".to_string(), vec![Type::named("str")]))
    );
    assert_eq!(
        builder
            .resolve_member_type(&mapping, "values")
            .expect("dict.values should resolve for editor analysis")
            .ty,
        Some(Type::Named("list".to_string(), vec![Type::named("int64")]))
    );
    assert_eq!(
        builder
            .resolve_member_type(&mapping, "items")
            .expect("dict.items should resolve for editor analysis")
            .ty,
        Some(Type::Named(
            "list".to_string(),
            vec![Type::Tuple(vec![Type::named("str"), Type::named("int64")])]
        ))
    );
}

#[test]
fn analysis_infers_enumerate_and_zip_loop_binding_types() {
    let source = concat!(
        "def main():\n",
        "    names = [\"Aura\"]\n",
        "    values = [7]\n",
        "    for index, name in enumerate(names):\n",
        "        print(index)\n",
        "        print(name)\n",
        "    for name, value in zip(names, values):\n",
        "        print(name)\n",
        "        print(value)\n",
    );
    let analysis = analyze_source(source);

    assert!(
        analysis.diagnostics.is_empty(),
        "valid lockstep loops should analyze without diagnostics: {:?}",
        analysis.diagnostics
    );
    for expected_hover in [
        "local index: int64",
        "local name: str",
        "local value: int64",
    ] {
        assert!(
            analysis
                .occurrences
                .iter()
                .any(|occurrence| occurrence.hover.contains(expected_hover)),
            "analysis should expose `{expected_hover}`"
        );
    }
}

#[test]
fn compiler_string_byte_tooling_separates_static_decode_from_instance_encode() {
    let static_source = "def main() -> int32:\n    str.\n    return 0\n";
    let static_names = completion_names_after_marker(static_source, "str.");
    assert!(static_names.contains(&"from_bytes".to_string()));
    assert!(!static_names.contains(&"to_bytes".to_string()));

    let analysis = analyze_source(
        r#"
import bytes

def decode(value: list[uint8]) -> Result[str, bytes.Error]:
    encoded = "Aura".to_bytes()
    digest = bytes.sha256(encoded)
    return str.from_bytes(value)

def main() -> int32:
    return 0
"#,
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("to_bytes() -> list[uint8]")));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence
            .hover
            .contains("from_bytes(bytes: list[uint8]) -> Result[str, bytes.Error]")
    }));

    let shadowed = analyze_source(
        r#"
class Decoder:
    def from_bytes(self, value: int32) -> int32:
        return value + 1

def main():
    String = Decoder()
    print(String.from_bytes(7))
"#,
    );
    assert!(
        shadowed.diagnostics.is_empty(),
        "{:?}",
        shadowed.diagnostics
    );
    assert!(shadowed.occurrences.iter().any(|occurrence| occurrence
        .hover
        .contains("method from_bytes(self, value: int32) -> int32")));
    assert!(shadowed.occurrences.iter().all(|occurrence| {
        !occurrence
            .hover
            .contains("from_bytes(bytes: list[uint8]) -> Result[str, bytes.Error]")
    }));
}

#[test]
fn compiler_duration_tooling_exposes_static_constructors_and_instance_conversions() {
    let static_source = "def main() -> int32:\n    Duration.\n    return 0\n";
    let static_names = completion_names_after_marker(static_source, "Duration.");
    assert!(static_names.contains(&"ms".to_string()));
    assert!(static_names.contains(&"seconds".to_string()));
    assert!(static_names.contains(&"minutes".to_string()));
    assert!(!static_names.contains(&"to_ms".to_string()));

    let instance_source =
        "def inspect(duration: Duration):\n    duration.\n\ndef main() -> int32:\n    return 0\n";
    let instance_names = completion_names_after_marker(instance_source, "duration.");
    assert!(instance_names.contains(&"to_ms".to_string()));
    assert!(instance_names.contains(&"to_seconds".to_string()));
    assert!(!instance_names.contains(&"seconds".to_string()));

    let analysis = analyze_source(
        r#"
def convert(value: int64, duration: Duration) -> float64:
    built = Duration.ms(value)
    scaled = duration * value
    return built.to_ms() + scaled.to_seconds()

def main() -> int32:
    return 0
"#,
    );
    assert!(analysis.diagnostics.is_empty());
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("type Duration")));
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("ms(value: int64) -> Duration")));
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("to_ms() -> float64")));
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("to_seconds() -> float64")));
}

#[test]
fn analysis_ignores_builtin_omitted_defaults_outside_source_inference() {
    let program = checked_program("def main() -> int32:\n    return 0\n");
    let marker = expr(ExprKind::BuiltinOmitted);
    let mut builder = AnalysisBuilder::new("", &program, Vec::new());

    assert_eq!(builder.infer_expr_type(&marker, &BTreeMap::new()), None);
    builder.visit_expr(&marker, &BTreeMap::new());
    assert!(builder.output.occurrences.is_empty());
}

#[test]
fn compiler_member_completion_for_map_exposes_map_methods() {
    let source =
        "def main() -> int32:\n    mut counts: dict[str, int32] = {}\n    counts.\n    return 0\n";
    let line_index = source
        .lines()
        .position(|line| line.contains("counts."))
        .expect("map source should contain member access");
    let line_text = source.lines().nth(line_index).unwrap();
    let character = line_text.find('.').unwrap() + 1;

    let completions =
        complete_source(source, line_index, character, Some('.')).expect("completion should work");
    let names = completions
        .into_iter()
        .map(|item| item.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"len".to_string()));
    assert!(names.contains(&"is_empty".to_string()));
    assert!(names.contains(&"copy".to_string()));
    assert!(names.contains(&"get".to_string()));
    assert!(names.contains(&"remove".to_string()));
    assert!(names.contains(&"keys".to_string()));
    assert!(names.contains(&"values".to_string()));
    assert!(names.contains(&"items".to_string()));
    assert!(names.contains(&"update".to_string()));
    assert!(names.contains(&"reserve".to_string()));
}

#[test]
fn compiler_member_completion_for_list_reports_insert_unit_detail() {
    let source = "def main() -> int32:\n    mut values = [1, 2, 3]\n    values.\n    return 0\n";
    let line_index = source
        .lines()
        .position(|line| line.contains("values."))
        .expect("vec source should contain member access");
    let line_text = source.lines().nth(line_index).unwrap();
    let character = line_text.find('.').unwrap() + 1;

    let completions =
        complete_source(source, line_index, character, Some('.')).expect("completion should work");
    let insert = completions
        .into_iter()
        .find(|item| item.name == "insert")
        .expect("insert completion should exist");

    assert_eq!(insert.detail, "insert(index: int64, value: own T) -> None");
}

#[test]
fn compiler_member_completion_includes_trait_impl_methods() {
    let source = include_str!("../../../examples/traits/greeter.au");
    let line_index = source
        .lines()
        .position(|line| line.contains("value.greet()"))
        .expect("trait example should contain member access");
    let line_text = source.lines().nth(line_index).unwrap();
    let character = line_text.find('.').unwrap() + 1;

    let completions =
        complete_source(source, line_index, character, Some('.')).expect("completion should work");
    let names = completions
        .into_iter()
        .map(|item| item.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"greet".to_string()));
}

#[test]
fn compiler_top_level_completion_includes_keywords_and_builtins() {
    let source = include_str!("../../../examples/point.au");
    let completions = complete_source(source, 0, 0, None).expect("completion should work");
    let names = completions
        .iter()
        .map(|item| item.name.clone())
        .collect::<Vec<_>>();

    assert!(names.contains(&"class".to_string()));
    assert!(names.contains(&"trait".to_string()));
    assert!(!names.contains(&"borrow".to_string()));
    assert!(names.contains(&"Point".to_string()));
    assert!(names.contains(&"distance".to_string()));
    assert!(names.contains(&"print".to_string()));
    assert!(names.contains(&"abs".to_string()));
    assert!(names.contains(&"min".to_string()));
    assert!(names.contains(&"max".to_string()));
    assert!(names.contains(&"sqrt".to_string()));
    assert!(names.contains(&"round".to_string()));
    assert!(names.contains(&"divmod".to_string()));
    let yield_now = completions
        .iter()
        .find(|item| item.name == "yield_now")
        .expect("yield_now builtin should appear in completions");
    assert_eq!(yield_now.detail, "yield_now() -> None");
    let range = completions
        .iter()
        .find(|item| item.name == "range")
        .expect("range builtin should appear in completions");
    assert!(range.detail.contains("start: int64"));
}

#[test]
fn module_constants_are_symbols_hover_targets_and_completions() {
    let source = "answer: int64 = 42\n\ndef main():\n    print(answer)\n";
    let output = analyze_source(source);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(output
        .symbols
        .iter()
        .any(|symbol| symbol.name == "answer" && symbol.kind == "constant"));
    let occurrence = output
        .occurrences
        .iter()
        .find(|occurrence| occurrence.line == 3 && occurrence.hover.contains("answer"))
        .expect("constant use occurrence");
    assert!(occurrence.hover.contains("module constant"));
    assert_eq!(
        occurrence.definition.as_ref().map(|range| range.line),
        Some(0)
    );

    let completions = complete_source(source, 3, 4, None).expect("constant completion");
    assert!(completions.iter().any(|completion| {
        completion.name == "answer" && completion.kind == "constant" && completion.detail == "int64"
    }));
}

#[test]
fn top_level_completion_scope_respects_constant_initialization_phase() {
    let source = [
        "first: int64 = 1",
        "second: int64 = first + 1",
        "print(second)",
        "third: int64 = 3",
    ]
    .join("\n");
    let program = checked_program(&source);
    let builder = AnalysisBuilder::new(&source, &program, Vec::new());

    let second_initializer = builder.scope_for_line(1);
    assert!(second_initializer.contains_key("first"));
    assert!(!second_initializer.contains_key("second"));
    assert!(!second_initializer.contains_key("third"));

    let initializer_completions =
        complete_source(&source, 1, 25, None).expect("completion in second initializer");
    assert!(initializer_completions
        .iter()
        .any(|completion| completion.name == "first" && completion.kind == "constant"));
    assert!(!initializer_completions
        .iter()
        .any(|completion| completion.name == "second"));
    assert!(!initializer_completions
        .iter()
        .any(|completion| completion.name == "third"));

    // Executable top-level statements run after the complete module constant
    // initialization phase, even when statement and declaration text is
    // interleaved.
    let executable_scope = builder.scope_for_line(2);
    assert!(executable_scope.contains_key("first"));
    assert!(executable_scope.contains_key("second"));
    assert!(executable_scope.contains_key("third"));
}

#[test]
fn compiler_analysis_infers_round_and_divmod_results_and_builtin_hover() {
    let analysis = analyze_source(
        r#"
def main():
    rounded: int64 = round(2.5)
    pair: (int64, int64) = divmod(-7, 3)
    print(rounded)
    print(pair)
"#,
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("round(value:")));
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("divmod(left:")));
}

fn completion_names_after_marker(source: &str, marker: &str) -> Vec<String> {
    let line_index = source
        .lines()
        .position(|line| line.contains(marker))
        .expect("source should contain completion marker");
    let line_text = source.lines().nth(line_index).unwrap();
    let character = line_text.find(marker).unwrap() + marker.len();

    complete_source(source, line_index, character, Some('.'))
        .expect("completion should work")
        .into_iter()
        .map(|item| item.name)
        .collect()
}

#[test]
fn compiler_completion_uses_nested_scopes_for_methods_match_for_and_trait_bounds() {
    let source = [
        "trait Show:",
        "    def show(self) -> str",
        "",
        "class Label:",
        "    value: int32",
        "    def collect(self) -> int32:",
        "        mut items: list[str] = [\"ready\"]",
        "        for item in items:",
        "            item.len()",
        "        self.value",
        "        return 0",
        "",
        "def unwrap(value: own Option[str]) -> str:",
        "    match own value:",
        "        case Option.Some(text):",
        "            text.len()",
        "            return text",
        "        case Option.None:",
        "            return \"\"",
        "",
        "def noop():",
        "    pass",
        "",
        "def use_group():",
        "    with TaskGroup() as group:",
        "        group.start_soon(noop)",
        "",
        "def render[T: Show](value: T) -> str:",
        "    value.show()",
        "    return value.show()",
        "",
        "def after_branch(flag: bool) -> int32:",
        "    label = \"ready\"",
        "    if flag:",
        "        branch = \"yes\"",
        "    else:",
        "        branch = \"no\"",
        "    label.len()",
        "    return 0",
    ]
    .join("\n");

    let for_scope = completion_names_after_marker(&source, "item.");
    assert!(for_scope.contains(&"len".to_string()));

    let method_scope = completion_names_after_marker(&source, "self.");
    assert!(method_scope.contains(&"value".to_string()));

    let match_scope = completion_names_after_marker(&source, "text.");
    assert!(match_scope.contains(&"len".to_string()));

    let with_scope = completion_names_after_marker(&source, "group.");
    assert!(with_scope.contains(&"start".to_string()));

    let trait_bound_scope = completion_names_after_marker(&source, "value.");
    assert!(trait_bound_scope.contains(&"show".to_string()));

    let after_branch_scope = completion_names_after_marker(&source, "label.");
    assert!(after_branch_scope.contains(&"len".to_string()));
}

#[test]
fn comprehension_analysis_uses_execution_scope_and_exact_target_spans() {
    let source = [
        "def collect_lengths(groups: list[list[str]]) -> list[int64]:",
        "    lengths = [",
        "        entry.len()",
        "        for group in groups",
        "        if group.len() > 0",
        "        for entry in group",
        "        if entry.contains(\"a\")",
        "    ]",
        "    print(lengths)",
        "    return lengths",
        "",
    ]
    .join("\n");

    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let group_target = analysis
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.line == 3
                && occurrence.start_character == 12
                && occurrence.end_character == 17
        })
        .expect("the outer comprehension target should have an exact definition occurrence");
    assert_eq!(group_target.hover, "```aura\nlocal group: list[str]\n```");
    assert_eq!(
        group_target.definition.as_ref(),
        Some(&super::AnalysisRange {
            file_path: None,
            line: 3,
            start_character: 12,
            end_character: 17,
        })
    );

    let entry_target = analysis
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.line == 5
                && occurrence.start_character == 12
                && occurrence.end_character == 17
        })
        .expect("the inner comprehension target should have an exact definition occurrence");
    assert_eq!(entry_target.hover, "```aura\nlocal entry: str\n```");
    assert_eq!(
        entry_target.definition.as_ref(),
        Some(&super::AnalysisRange {
            file_path: None,
            line: 5,
            start_character: 12,
            end_character: 17,
        })
    );

    let output_entry = analysis
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.line == 2
                && occurrence.start_character == 8
                && occurrence.end_character == 13
        })
        .expect("the output expression should see the inner target");
    assert_eq!(output_entry.definition, entry_target.definition);

    let occurrence_index = |line, start_character| {
        analysis
            .occurrences
            .iter()
            .position(|occurrence| {
                occurrence.line == line && occurrence.start_character == start_character
            })
            .unwrap_or_else(|| panic!("missing occurrence at {line}:{start_character}"))
    };
    let execution_order = [
        occurrence_index(3, 21), // outer iterable: groups
        occurrence_index(3, 12), // outer target: group
        occurrence_index(4, 11), // outer filter: group
        occurrence_index(5, 21), // inner iterable: group
        occurrence_index(5, 12), // inner target: entry
        occurrence_index(6, 11), // inner filter: entry
        occurrence_index(2, 8),  // output: entry
    ];
    assert!(
        execution_order.windows(2).all(|pair| pair[0] < pair[1]),
        "analysis occurrences must follow comprehension execution order: {execution_order:?}"
    );

    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover == "```aura\nbinding lengths: list[int64]\n```"));

    let completion_names = |line: usize, character: usize, trigger| {
        complete_source(&source, line, character, trigger)
            .expect("comprehension completion should succeed")
            .into_iter()
            .map(|completion| completion.name)
            .collect::<BTreeSet<_>>()
    };
    let output_members = completion_names(2, 14, Some('.'));
    assert!(output_members.contains("len"));
    assert!(output_members.contains("contains"));

    let outer_filter = completion_names(4, 16, None);
    assert!(outer_filter.contains("group"));
    assert!(!outer_filter.contains("entry"));

    let inner_iterable = completion_names(5, 26, None);
    assert!(inner_iterable.contains("group"));
    assert!(!inner_iterable.contains("entry"));

    let inner_filter = completion_names(6, 16, None);
    assert!(inner_filter.contains("group"));
    assert!(inner_filter.contains("entry"));

    let after_comprehension = completion_names(8, 17, None);
    assert!(!after_comprehension.contains("group"));
    assert!(!after_comprehension.contains("entry"));
}

#[test]
fn phase72_slice_analysis_visits_bounds_and_preserves_owned_result_types() {
    let source = concat!(
        "def take_slice(values: list[str], text: str, start: int32, end: int32) -> list[str]:\n",
        "    selected = values[start:end]\n",
        "    label = text[:end]\n",
        "    print(label)\n",
        "    return selected\n",
    );
    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    for expected_hover in [
        "param values: list[str]",
        "param start: int32",
        "param end: int32",
        "binding selected: list[str]",
        "param text: str",
        "binding label: str",
    ] {
        assert!(
            analysis
                .occurrences
                .iter()
                .any(|occurrence| occurrence.hover.contains(expected_hover)),
            "missing slice occurrence `{expected_hover}` in {:?}",
            analysis.occurrences
        );
    }
}

#[test]
fn phase72_slice_result_type_drives_member_completion_during_an_incomplete_edit() {
    let source = concat!(
        "def take_slice(values: list[str], start: int32, end: int32):\n",
        "    selected = values[start:end]\n",
        "    selected.\n",
    );
    let completions = complete_source(source, 2, 13, Some('.'))
        .expect("slice result completion should recover from a dangling member");
    assert!(completions.iter().any(|item| item.name == "append"));
    assert!(completions.iter().any(|item| item.name == "len"));
}

#[test]
fn phase72_slice_completion_recovers_call_bases_and_delimiters_inside_strings() {
    for (receiver, line, character) in [
        (
            "make_values()[1:]",
            "    make_values()[1:].\n",
            "    make_values()[1:].".len(),
        ),
        (
            "values[endpoint(\"]\"):]",
            "    values[endpoint(\"]\"):].\n",
            "    values[endpoint(\"]\"):].".len(),
        ),
    ] {
        let source = format!(
            "{}{}",
            concat!(
                "def make_values() -> list[str]:\n",
                "    return [\"Ada\", \"Grace\"]\n",
                "\n",
                "def endpoint(text: str) -> int32:\n",
                "    return 0\n",
                "\n",
                "def inspect(values: list[str]):\n",
            ),
            line,
        );
        let completions = complete_source(&source, 7, character, Some('.'))
            .unwrap_or_else(|error| panic!("completion should recover `{receiver}`: {error:?}"));
        assert!(
            completions.iter().any(|item| item.name == "append"),
            "missing list completion for `{receiver}`: {completions:?}"
        );
        assert!(
            completions.iter().any(|item| item.name == "len"),
            "missing list completion for `{receiver}`: {completions:?}"
        );
    }
}

#[test]
fn comprehension_analysis_infers_every_builtin_iterable_target_and_result_shape() {
    let source = r#"def inspect(
    names: list[str],
    tags: set[str],
    left: list[int32],
    right: list[int32],
    jobs: Queue[str]
):
    indexed = [name.len() + index for index, name in enumerate(names)]
    paired = [number + delta for number, delta in zip(left, right)]
    ranged = [number for number in range(0, 3)]
    tagged = {tag.len() for tag in tags}
    received = {item.clone(): item.len() for item in jobs}
"#;

    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    for expected_hover in [
        "local index: int64",
        "local name: str",
        "local number: int32",
        "local delta: int32",
        "local tag: str",
        "local item: str",
        "binding indexed: list[int64]",
        "binding paired: list[int32]",
        "binding ranged: list[int64]",
        "binding tagged: set[int64]",
        "binding received: dict[str, int64]",
    ] {
        assert!(
            analysis
                .occurrences
                .iter()
                .any(|occurrence| occurrence.hover.contains(expected_hover)),
            "missing comprehension hover `{expected_hover}` in {:?}",
            analysis.occurrences
        );
    }
}

#[test]
fn comprehension_scope_composes_with_contextual_lambda_scope() {
    let source = [
        "def apply(values: list[int32]) -> list[int32]:",
        "    results = [values.map(lambda delta: item + delta).len() as int32 for item in values]",
        "    return results",
        "",
    ]
    .join("\n");
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let item_use = analysis
        .occurrences
        .iter()
        .find(|occurrence| occurrence.line == 1 && occurrence.start_character == 40)
        .expect("the lambda body should resolve its comprehension-target capture");
    assert!(item_use.hover.contains("local item: int32"));
    assert_eq!(
        item_use.definition.as_ref().map(|range| (
            range.line,
            range.start_character,
            range.end_character
        )),
        Some((1, 73, 77))
    );
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover == "```aura\nbinding results: list[int32]\n```"));

    let body_character = source.lines().nth(1).unwrap().find("item +").unwrap() + 2;
    let completions = complete_source(&source, 1, body_character, None)
        .expect("completion inside a comprehension lambda should succeed");
    let names = completions
        .into_iter()
        .map(|completion| completion.name)
        .collect::<BTreeSet<_>>();
    assert!(names.contains("item"));
    assert!(names.contains("delta"));
}

#[test]
fn comprehension_completion_respects_token_boundaries_and_map_output_scope() {
    let source = [
        "def inspect(groups: list[list[str]], gift: list[str]):",
        "    plain = [name.len() for name in gift]",
        "    projected = {",
        "        outer.len():",
        "        inner.len()",
        "        for outer in groups",
        "        for inner in outer",
        "        if inner.len() > 0",
        "    }",
        "    selected = [",
        "        item.len()",
        "        for item in gift",
        "        if # gift_if iffy",
        "            item.len() > 0",
        "        if item.contains(\"a\")",
        "    ]",
        "    print(plain)",
        "    print(projected)",
        "    print(selected)",
        "",
    ]
    .join("\n");
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let completion_names = |line: usize, character: usize, trigger| {
        complete_source(&source, line, character, trigger)
            .expect("comprehension completion should succeed")
            .into_iter()
            .map(|completion| completion.name)
            .collect::<BTreeSet<_>>()
    };

    let plain_line = source.lines().nth(1).unwrap();
    let plain_iterable_start = plain_line.rfind("gift").unwrap();
    let inside_identifier_if = completion_names(1, plain_iterable_start + 3, None);
    assert!(inside_identifier_if.contains("gift"));
    assert!(
        !inside_identifier_if.contains("name"),
        "the `if` bytes inside `gift` must not be mistaken for a filter keyword"
    );

    let at_final_iterable_end = completion_names(1, plain_line.len(), None);
    assert!(at_final_iterable_end.contains("gift"));
    assert!(
        !at_final_iterable_end.contains("name"),
        "the target must remain unavailable throughout the source iterable"
    );

    let output_members = completion_names(10, 13, Some('.'));
    assert!(output_members.contains("len"));
    assert!(output_members.contains("contains"));

    let selected_iterable_line = source.lines().nth(11).unwrap();
    let selected_iterable_start = selected_iterable_line.rfind("gift").unwrap();
    let inside_filtered_identifier_if = completion_names(11, selected_iterable_start + 3, None);
    assert!(inside_filtered_identifier_if.contains("gift"));
    assert!(
        !inside_filtered_identifier_if.contains("item"),
        "the filtered target must stay unavailable inside an iterable whose name contains `if`"
    );

    let multiline_if_line = source.lines().nth(12).unwrap();
    let immediately_after_multiline_if = completion_names(12, multiline_if_line.len(), None);
    assert!(immediately_after_multiline_if.contains("item"));

    let first_filter_line = source.lines().nth(13).unwrap();
    let first_filter_members =
        completion_names(13, first_filter_line.find('.').unwrap() + 1, Some('.'));
    assert!(first_filter_members.contains("len"));

    let second_if_line = source.lines().nth(14).unwrap();
    let second_keyword_end = second_if_line.find("if").unwrap() + "if".len();
    let immediately_after_second_if = completion_names(14, second_keyword_end, None);
    assert!(immediately_after_second_if.contains("item"));

    let second_filter_members =
        completion_names(14, second_if_line.find('.').unwrap() + 1, Some('.'));
    assert!(second_filter_members.contains("contains"));

    let after_comprehension = completion_names(18, source.lines().nth(18).unwrap().len(), None);
    assert!(
        !after_comprehension.contains("item"),
        "a target must not leak after its comprehension"
    );

    let outer_line = source.lines().nth(5).unwrap();
    let outer_iterable_start = outer_line.rfind("groups").unwrap();
    let before_outer_iterable = completion_names(5, outer_iterable_start - 1, None);
    assert!(before_outer_iterable.contains("groups"));
    assert!(!before_outer_iterable.contains("outer"));
    assert!(!before_outer_iterable.contains("inner"));

    let inner_line = source.lines().nth(6).unwrap();
    let inner_iterable_start = inner_line.rfind("outer").unwrap();
    let before_inner_iterable = completion_names(6, inner_iterable_start - 1, None);
    assert!(before_inner_iterable.contains("outer"));
    assert!(!before_inner_iterable.contains("inner"));

    let filter_line = source.lines().nth(7).unwrap();
    let filter_expression_start = filter_line.rfind("inner").unwrap();
    let before_filter_expression = completion_names(7, filter_expression_start - 1, None);
    assert!(
        before_filter_expression.contains("outer"),
        "{before_filter_expression:?}"
    );
    assert!(
        before_filter_expression.contains("inner"),
        "{before_filter_expression:?}"
    );

    let map_key_line = source.lines().nth(3).unwrap();
    let map_key_members = completion_names(3, map_key_line.find('.').unwrap() + 1, Some('.'));
    assert!(map_key_members.contains("len"));
    let map_value_line = source.lines().nth(4).unwrap();
    let map_value_members = completion_names(4, map_value_line.find('.').unwrap() + 1, Some('.'));
    assert!(map_value_members.contains("len"));
}

#[test]
fn comprehension_completion_finds_the_filter_keyword_by_source_token_position() {
    let commented_source = [
        "def inspect(values: list[str]):",
        "    selected = [",
        "        item.len()",
        "        for item in values",
        "        if # a standalone if inside this comment is not syntax",
        "            item.len() > 0",
        "    ]",
        "    print(selected)",
        "",
    ]
    .join("\n");
    let keyword_line = commented_source.lines().nth(4).unwrap();
    let keyword_end = keyword_line.find("if").unwrap() + "if".len();
    let commented_names = complete_source(&commented_source, 4, keyword_end, None)
        .expect("completion immediately after a commented filter keyword should succeed")
        .into_iter()
        .map(|completion| completion.name)
        .collect::<BTreeSet<_>>();
    assert!(
        commented_names.contains("item"),
        "comment text must not displace the actual comprehension-filter keyword"
    );

    let unicode_source = [
        "def inspect():",
        "    selected = [item.len() for item in [\"é🙂\"] if item.len() > 0]",
        "",
    ]
    .join("\n");
    let unicode_line = unicode_source.lines().nth(1).unwrap();
    let keyword_start = unicode_line.rfind(" if ").unwrap() + 1;
    let keyword_end = unicode_line[..keyword_start + "if".len()]
        .encode_utf16()
        .count();
    let unicode_names = complete_source(&unicode_source, 1, keyword_end, None)
        .expect("completion after a filter following non-ASCII source should succeed")
        .into_iter()
        .map(|completion| completion.name)
        .collect::<BTreeSet<_>>();
    assert!(
        unicode_names.contains("item"),
        "source byte offsets must be translated to LSP character positions"
    );

    let fstring_source = [
        "def inspect(values: list[int64]):",
        "    rendered = f\"{[item for item in values if item > 0]}\"",
        "",
    ]
    .join("\n");
    let fstring_line = fstring_source.lines().nth(1).unwrap();
    let keyword_end = fstring_line.rfind(" if ").unwrap() + 1 + "if".len();
    let fstring_names = complete_source(&fstring_source, 1, keyword_end, None)
        .expect("completion inside an f-string comprehension should succeed")
        .into_iter()
        .map(|completion| completion.name)
        .collect::<BTreeSet<_>>();
    assert!(
        fstring_names.contains("item"),
        "embedded expressions must retain comprehension-filter scope"
    );
}

#[test]
fn completion_keeps_function_scope_through_a_multiline_final_statement() {
    let source = [
        "def inspect(values: list[str]):",
        "    selected = [",
        "        item.len()",
        "        for item in values",
        "        if",
        "            item.len() > 0",
        "    ]",
        "",
    ]
    .join("\n");

    let keyword_line = source.lines().nth(4).unwrap();
    let after_keyword = complete_source(&source, 4, keyword_line.len(), None)
        .expect("completion after the final statement's filter keyword should succeed")
        .into_iter()
        .map(|completion| completion.name)
        .collect::<BTreeSet<_>>();
    assert!(after_keyword.contains("values"));
    assert!(
        after_keyword.contains("item"),
        "the final multiline statement must retain its comprehension scope"
    );

    let filter_line = source.lines().nth(5).unwrap();
    let filter_members = complete_source(&source, 5, filter_line.find('.').unwrap() + 1, Some('.'))
        .expect("member completion in the final multiline statement should succeed")
        .into_iter()
        .map(|completion| completion.name)
        .collect::<BTreeSet<_>>();
    assert!(filter_members.contains("len"));
    assert!(filter_members.contains("contains"));
}

#[test]
fn completion_uses_expression_and_nested_block_extents_for_final_statements() {
    for (source, line, character, expected) in [
        (
            "def retain(value: int64) -> int64:\n    return (\n        value\n    )\n",
            2,
            10,
            "value",
        ),
        (
            "def retain(flag: bool):\n    assert (\n        flag\n    )\n",
            2,
            10,
            "flag",
        ),
        (
            "def retain(value: int64):\n    print(\n        value\n    )\n",
            2,
            10,
            "value",
        ),
    ] {
        let names = complete_source(source, line, character, None)
            .expect("completion in a multiline final value statement should succeed")
            .into_iter()
            .map(|completion| completion.name)
            .collect::<BTreeSet<_>>();
        assert!(
            names.contains(expected),
            "the function scope must extend through the final expression: {source}"
        );
    }

    let nested_source = [
        "def inspect(values: list[str], enabled: bool):",
        "    if enabled:",
        "        selected = [",
        "            item.len()",
        "            for item in values",
        "            if",
        "                item.len() > 0",
        "        ]",
        "",
    ]
    .join("\n");
    let keyword_line = nested_source.lines().nth(5).unwrap();
    let names = complete_source(&nested_source, 5, keyword_line.len(), None)
        .expect("completion in a nested final multiline statement should succeed")
        .into_iter()
        .map(|completion| completion.name)
        .collect::<BTreeSet<_>>();
    assert!(names.contains("enabled"));
    assert!(names.contains("values"));
    assert!(names.contains("item"));

    let indexed_assignment_source = [
        "def replace(values: mut list[str], index: int32, replacement: own str):",
        "    values[",
        "        index",
        "    ] = (",
        "        replacement",
        "    )",
        "",
    ]
    .join("\n");
    let index_line = indexed_assignment_source.lines().nth(2).unwrap();
    let names = complete_source(&indexed_assignment_source, 2, index_line.len(), None)
        .expect("completion in a multiline final indexed-assignment target should succeed")
        .into_iter()
        .map(|completion| completion.name)
        .collect::<BTreeSet<_>>();
    assert!(names.contains("values"));
    assert!(names.contains("index"));
    assert!(names.contains("replacement"));
}

#[test]
fn analysis_recovery_helpers_cover_member_error_paths() {
    let source = [
        "class Counter:",
        "    value: int32",
        "",
        "def main() -> int32:",
        "    counter = Counter(value=1)",
        "    counter.",
        "    counter.",
        "    return 0",
    ]
    .join("\n");
    let mut check_program = crate::check_source;

    let error = crate::parser::parse(&source).expect_err("dangling member should not parse");
    let recovered =
        recover_checked_program_after_parse_error_with(&source, &error, &mut check_program);
    assert!(recovered.is_some());

    let recovered_after_position =
        recover_checked_program_after_position(&source, 5, 12, &mut check_program);
    assert!(recovered_after_position.is_some());

    let recovered_after_members =
        recover_checked_program_after_member_errors(&source, &mut check_program);
    assert!(recovered_after_members.is_some());

    let mid_line_incomplete_member = [
        "class Counter:",
        "    value: int32",
        "",
        "def main() -> int32:",
        "    counter = Counter(value=1)",
        "    counter. + 1",
        "    return 0",
    ]
    .join("\n");
    assert!(
        recover_checked_program_after_member_errors(
            &mid_line_incomplete_member,
            &mut check_program
        )
        .is_some(),
        "editor recovery should replace an incomplete member statement even when the dot is not at end of line"
    );

    let too_many_dangling_members = [
        "def main() -> int32:",
        "    counter.",
        "    counter.",
        "    counter.",
        "    counter.",
        "    counter.",
        "    counter.",
        "    counter.",
        "    counter.",
        "    counter.",
        "    return 0",
    ]
    .join("\n");
    assert!(
        recover_checked_program_after_member_errors(&too_many_dangling_members, &mut check_program)
            .is_none(),
        "recovery should stop after the bounded retry budget"
    );

    let non_member = Diagnostic::at(Span::new(1, 1), "expected Colon, found Newline");
    assert!(recover_checked_program_after_parse_error_with(
        &source,
        &non_member,
        &mut check_program
    )
    .is_none());
    assert!(
        recover_checked_program_after_member_errors("def main(\n", &mut check_program).is_none()
    );
    assert!(
        complete_source("def main(\n", 0, 1, None).is_err(),
        "non-member completion requests should surface parse errors instead of recovering"
    );
}

#[test]
fn analysis_recovery_helpers_stop_when_replacement_makes_no_progress() {
    fn no_progress(source: &str, _line: usize) -> String {
        source.to_string()
    }

    let source = ["def main() -> None:", "    value."].join("\n");
    let mut check_program = crate::check_source;

    assert!(
        recover_checked_program_after_member_errors_with(&source, &mut check_program, no_progress,)
            .is_none(),
        "member recovery should stop if the replacement leaves the candidate unchanged"
    );
}

#[test]
fn analysis_recovery_only_classifies_code_dots_as_dangling_members() {
    assert_eq!(
        first_dangling_member_line("def main():\n    print(value.\n    return 0"),
        Some(1)
    );
    assert_eq!(
        first_dangling_member_line("def main():\n    text = \"value.\"\n    # value."),
        None
    );
    assert_eq!(
        first_dangling_member_line("def main():\n    text = 'value.' # comment."),
        None
    );
    assert_eq!(
        first_dangling_member_line("def main():\n    text = \"escaped quote: \\\"."),
        None,
        "a trailing dot inside an escaped-quote string is not a member access"
    );
}

#[test]
fn analysis_recovery_replaces_the_multiline_statement_owning_a_dangling_member() {
    let source = [
        "def main() -> int32:",
        "    text = \"hello\"",
        "    print(",
        "        \"escaped \\\" quote\",",
        "        text.",
    ]
    .join("\n");

    let analysis = analyze_source(&source);
    assert!(
        !analysis.symbols.is_empty(),
        "analysis should retain declaration structure while the call is incomplete"
    );
    assert!(
        !analysis.occurrences.is_empty(),
        "analysis should retain checked occurrences while the call is incomplete"
    );

    let completions =
        complete_source(&source, 4, 13, Some('.')).expect("member completion should recover");
    assert!(
        completions.iter().any(|item| item.name == "len"),
        "the recovered receiver should retain its str type"
    );
}

#[test]
fn analysis_trait_impl_helpers_cover_generic_bound_resolution() {
    let source = [
        "trait Show:",
        "    def show(self) -> str",
        "",
        "trait Named:",
        "    def label(self) -> str",
        "",
        "trait Mapper[T]:",
        "    def map(self) -> T",
        "",
        "class Box[T]:",
        "    value: T",
        "",
        "impl Show for int32:",
        "    def show(self) -> str:",
        "        return f\"{self}\"",
        "",
        "impl[T: Show] Named for Box[T]:",
        "    def label(self) -> str:",
        "        return self.value.show()",
        "",
        "impl Mapper[int32] for Box[int32]:",
        "    def map(self) -> int32:",
        "        return self.value",
    ]
    .join("\n");
    let program = checked_program(&source);
    let builder = AnalysisBuilder::new(&source, &program, Vec::new());
    let trait_impl = program
        .trait_impls
        .iter()
        .find(|info| info.trait_name == "Named")
        .expect("Named impl should exist");
    let bound = TraitBound {
        trait_name: "Named".to_string(),
        trait_args: Vec::new(),
    };

    let substitutions = builder
        .trait_impl_substitutions(
            trait_impl,
            &Type::Named("Box".to_string(), vec![Type::named("int32")]),
        )
        .expect("Box[str] should satisfy Named impl");
    assert_eq!(substitutions.get("T"), Some(&Type::named("int32")));

    let bound_substitutions = builder
        .trait_impl_substitutions_for_bound(
            trait_impl,
            &Type::Named("Box".to_string(), vec![Type::named("int32")]),
            &bound,
        )
        .expect("bound substitution should resolve");
    assert_eq!(bound_substitutions.get("T"), Some(&Type::named("int32")));

    assert!(builder.type_implements_trait_bound(
        &Type::Named("Box".to_string(), vec![Type::named("int32")]),
        &bound,
    ));
    assert!(!builder.type_implements_trait_bound(
        &Type::Named("Box".to_string(), vec![Type::named("str")]),
        &bound,
    ));
    let mapper_impl = program
        .trait_impls
        .iter()
        .find(|info| info.trait_name == "Mapper")
        .expect("Mapper impl should exist");
    let mismatched_mapper_bound = TraitBound {
        trait_name: "Mapper".to_string(),
        trait_args: vec![Type::named("str")],
    };
    assert!(
        builder
            .trait_impl_substitutions_for_bound(
                mapper_impl,
                &Type::Named("Box".to_string(), vec![Type::named("int32")]),
                &mismatched_mapper_bound,
            )
            .is_none(),
        "trait argument mismatch should reject otherwise matching impls"
    );
    let matching_mapper_bound = TraitBound {
        trait_name: "Mapper".to_string(),
        trait_args: vec![Type::named("int32")],
    };
    let mapper_substitutions = builder
        .trait_impl_substitutions_for_bound(
            mapper_impl,
            &Type::Named("Box".to_string(), vec![Type::named("int32")]),
            &matching_mapper_bound,
        )
        .expect("matching trait arguments should keep the impl in scope");
    assert!(mapper_substitutions.is_empty());

    let (_impl_info, method, resolved) = builder
        .trait_method_for_receiver(
            &Type::Named("Box".to_string(), vec![Type::named("int32")]),
            "label",
        )
        .expect("trait method should resolve for Box[int32]");
    assert_eq!(method.signature.return_type, Type::named("str"));
    assert_eq!(resolved.get("T"), Some(&Type::named("int32")));
}

#[test]
fn analysis_scope_and_call_inference_helpers_cover_methods_assignments_and_builtins() {
    let source = [
        "class Counter:",
        "    value: int32",
        "    def bump(mut self, step: int32) -> int32:",
        "        start = self.value",
        "        mut total = start",
        "        total = total + step",
        "        self.value = total",
        "        return total",
        "",
        "def helper() -> int32:",
        "    return 1",
        "",
        "def main() -> int32:",
        "    return helper()",
    ]
    .join("\n");
    let program = checked_program(&source);
    let mut builder = AnalysisBuilder::new(&source, &program, Vec::new());
    let class_decl = program
        .module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Class(class_decl) if class_decl.name == "Counter" => Some(class_decl),
            _ => None,
        })
        .expect("Counter class should exist");
    let method_decl = class_decl
        .methods
        .iter()
        .find(|method| method.name == "bump")
        .expect("bump method should exist");
    let method_info = program
        .classes
        .get("Counter")
        .and_then(|class| class.methods.get("bump"))
        .expect("method info should exist");

    let mut scope = builder.method_scope("Counter", method_decl, method_info);
    assert_eq!(
        scope.get("self").map(|binding| binding.ty.clone()),
        Some(Type::named("Counter"))
    );
    assert_eq!(
        scope.get("step").map(|binding| binding.ty.clone()),
        Some(Type::named("int32"))
    );

    builder.visit_stmts(&method_decl.body, &mut scope);
    assert_eq!(
        scope.get("start").map(|binding| binding.ty.clone()),
        Some(Type::named("int32"))
    );
    assert_eq!(
        scope.get("total").map(|binding| binding.ty.clone()),
        Some(Type::named("int32"))
    );
    assert!(!builder.output.occurrences.is_empty());

    let scope_for_return = builder.scope_for_line(7);
    assert!(scope_for_return.contains_key("self"));
    assert!(scope_for_return.contains_key("total"));

    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Name("abs".to_string())),
            &[arg(expr(ExprKind::Int(4)))],
            &BTreeMap::new(),
        ),
        Some(Type::named("int64"))
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Name("sqrt".to_string())),
            &[arg(expr(ExprKind::Float(4.0)))],
            &BTreeMap::new(),
        ),
        Some(Type::named("float64"))
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("list".to_string()))),
                type_args: vec![type_ref("int32")],
            }),
            &[],
            &BTreeMap::new(),
        ),
        Some(Type::Named("list".to_string(), vec![Type::named("int32")]))
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Name("helper".to_string())),
            &[],
            &BTreeMap::new(),
        ),
        Some(Type::named("int32"))
    );
}

#[test]
fn analysis_infers_canonical_concrete_type_for_user_generic_specialization() {
    let source = "class Parcel[T]:\n    value: T\n";
    let mut program = checked_program(source);
    program
        .canonical_type_names
        .insert("Parcel".to_string(), "inventory.Parcel".to_string());
    let builder = AnalysisBuilder::new(source, &program, Vec::new());
    let specialized = expr(ExprKind::Specialize {
        expr: Box::new(expr(ExprKind::Name("Parcel".to_string()))),
        type_args: vec![type_ref("str")],
    });

    assert_eq!(
        builder.infer_expr_type(&specialized, &BTreeMap::new()),
        Some(Type::Named(
            "inventory.Parcel".to_string(),
            vec![Type::named("str")],
        )),
        "analysis clients must see both the canonical class identity and its concrete type argument"
    );
}

#[test]
fn completion_scope_walks_past_if_else_and_while_blocks() {
    let source = [
        "def scoped(flag: bool) -> int32:",
        "    mut total: int32 = 0",
        "    if flag:",
        "        in_if = total",
        "    else:",
        "        in_else = total",
        "    after_if = total",
        "    while flag:",
        "        in_while = total",
        "        break",
        "    after_while = total",
        "    return after_while",
        "",
        "def main() -> int32:",
        "    return scoped(false)",
    ]
    .join("\n");
    let program = checked_program(&source);
    let builder = AnalysisBuilder::new(&source, &program, Vec::new());

    let scope_inside_else = builder.scope_for_line(5);
    assert!(scope_inside_else.contains_key("total"));
    assert!(scope_inside_else.contains_key("in_else"));
    assert!(!scope_inside_else.contains_key("after_if"));

    let scope_after_if = builder.scope_for_line(7);
    assert!(scope_after_if.contains_key("total"));
    assert!(scope_after_if.contains_key("after_if"));
    assert!(!scope_after_if.contains_key("in_else"));

    let scope_after_while = builder.scope_for_line(11);
    assert!(scope_after_while.contains_key("after_while"));
    assert!(!scope_after_while.contains_key("in_while"));
}

#[test]
fn analysis_completion_and_inference_helpers_cover_builtin_collection_and_enum_surfaces() {
    let source = [
        "trait Show:",
        "    def show(self) -> str",
        "",
        "trait Greeter:",
        "    def greet(self) -> str",
        "",
        "class User:",
        "    label: str",
        "",
        "    def greet(self) -> str:",
        "        return self.label.clone()",
        "",
        "impl Show for User:",
        "    def show(self) -> str:",
        "        return self.label.clone()",
        "",
        "impl Greeter for User:",
        "    def greet(self) -> str:",
        "        return self.label.clone()",
        "",
        "enum Status:",
        "    Ready",
        "    Failed(code: int32, reason: str)",
        "",
        "def helper() -> int32:",
        "    return 1",
        "",
        "def resultify(value: int32) -> Result[int32, str]:",
        "    return Result.Ok(value)",
    ]
    .join("\n");
    let mut program = checked_program(&source);
    let remote_source = [
        "trait RemoteTrait:",
        "    def render(self) -> str",
        "",
        "enum RemoteStatus:",
        "    Ready",
        "    Failed(code: int32, reason: str)",
        "",
        "class Remote:",
        "    value: int32",
        "",
        "def remote_fn() -> int32:",
        "    return 9",
    ]
    .join("\n");
    let remote_program = checked_program(&remote_source);
    let mut tools_namespace = crate::sema::ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "tools".to_string(),
        path: "pkg.tools".to_string(),
        source_path: None,
        closures: Default::default(),
        comprehensions: Default::default(),
        modules: Default::default(),
        functions: remote_program.functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: remote_program.classes.clone(),
        enums: remote_program.enums.clone(),
        traits: remote_program.traits.clone(),
        trait_impls: remote_program.trait_impls.clone(),
        all_functions: remote_program.functions.clone(),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: remote_program.classes.clone(),
        all_enums: remote_program.enums.clone(),
        all_traits: remote_program.traits.clone(),
        imported_modules: Default::default(),
    };
    tools_namespace.modules.insert(
        "inner".to_string(),
        crate::sema::ModuleNamespace {
            constants: BTreeMap::new(),
            all_constants: BTreeMap::new(),
            name: "inner".to_string(),
            path: "pkg.tools.inner".to_string(),
            source_path: None,
            closures: Default::default(),
            comprehensions: Default::default(),
            modules: Default::default(),
            functions: Default::default(),
            extern_functions: Default::default(),
            opaque_handles: Default::default(),
            classes: Default::default(),
            enums: Default::default(),
            traits: Default::default(),
            trait_impls: Vec::new(),
            all_functions: Default::default(),
            all_extern_functions: Default::default(),
            all_opaque_handles: Default::default(),
            all_classes: Default::default(),
            all_enums: Default::default(),
            all_traits: Default::default(),
            imported_modules: Default::default(),
        },
    );
    program.imported_modules.insert(
        "pkg".to_string(),
        crate::sema::ModuleNamespace {
            constants: BTreeMap::new(),
            all_constants: BTreeMap::new(),
            name: "pkg".to_string(),
            path: "pkg".to_string(),
            source_path: None,
            closures: Default::default(),
            comprehensions: Default::default(),
            modules: std::collections::BTreeMap::from([(
                "tools".to_string(),
                tools_namespace.clone(),
            )]),
            functions: Default::default(),
            extern_functions: Default::default(),
            opaque_handles: Default::default(),
            classes: Default::default(),
            enums: Default::default(),
            traits: Default::default(),
            trait_impls: Vec::new(),
            all_functions: Default::default(),
            all_extern_functions: Default::default(),
            all_opaque_handles: Default::default(),
            all_classes: Default::default(),
            all_enums: Default::default(),
            all_traits: Default::default(),
            imported_modules: Default::default(),
        },
    );
    let builder = AnalysisBuilder::new(&source, &program, Vec::new());
    assert!(builder.complete(100, 0, Some('.')).unwrap().is_empty());
    let unresolved_completion_program = checked_program("def main():\n    pass\n");
    let unresolved_completion_builder =
        AnalysisBuilder::new("missing.", &unresolved_completion_program, Vec::new());
    assert!(unresolved_completion_builder
        .complete(0, "missing.".len(), Some('.'))
        .unwrap()
        .is_empty());

    let top_level_names = builder
        .top_level_completions()
        .into_iter()
        .map(|completion| completion.name)
        .collect::<Vec<_>>();
    assert!(top_level_names.contains(&"User".to_string()));
    assert!(top_level_names.contains(&"Status".to_string()));
    assert!(top_level_names.contains(&"Show".to_string()));
    assert!(top_level_names.contains(&"Result".to_string()));
    assert!(top_level_names.contains(&"SendError".to_string()));
    assert!(top_level_names.contains(&"pkg".to_string()));

    let send_error_symbol = builder
        .resolve_name("SendError", &BTreeMap::new())
        .expect("builtin SendError should resolve");
    assert!(send_error_symbol.hover.contains("SendError[T]"));
    assert!(send_error_symbol.definition.is_none());

    let trait_bound_names = builder
        .trait_bound_member_completions(&[
            TraitBound {
                trait_name: "Missing".to_string(),
                trait_args: Vec::new(),
            },
            TraitBound {
                trait_name: "Show".to_string(),
                trait_args: Vec::new(),
            },
            TraitBound {
                trait_name: "Show".to_string(),
                trait_args: Vec::new(),
            },
        ])
        .into_iter()
        .map(|completion| completion.name)
        .collect::<Vec<_>>();
    assert_eq!(
        trait_bound_names
            .iter()
            .filter(|name| name.as_str() == "show")
            .count(),
        1
    );

    let module_names = builder
        .member_completions(&Type::Module("pkg.tools".to_string()))
        .into_iter()
        .map(|completion| completion.name)
        .collect::<Vec<_>>();
    assert!(module_names.contains(&"inner".to_string()));
    assert!(module_names.contains(&"remote_fn".to_string()));
    assert!(module_names.contains(&"Remote".to_string()));
    assert!(module_names.contains(&"RemoteStatus".to_string()));
    assert!(module_names.contains(&"RemoteTrait".to_string()));
    assert!(
        builder
            .resolve_member_type(&Type::Module("pkg.tools".to_string()), "missing")
            .is_none(),
        "unknown module members should not resolve"
    );

    let remote_trait_member = builder
        .resolve_member_type(&Type::Module("pkg.tools".to_string()), "RemoteTrait")
        .expect("qualified imported traits should resolve as module members");
    assert!(remote_trait_member.hover.contains("trait RemoteTrait"));
    assert!(remote_trait_member.definition.is_some());
    assert_eq!(remote_trait_member.ty, None);

    let user_member_names = builder
        .member_completions(&Type::named("User"))
        .into_iter()
        .map(|completion| completion.name)
        .collect::<Vec<_>>();
    assert!(user_member_names.contains(&"label".to_string()));
    assert!(user_member_names.contains(&"greet".to_string()));
    assert!(user_member_names.contains(&"show".to_string()));

    let status_member_names = builder
        .member_completions(&Type::named("Status"))
        .into_iter()
        .map(|completion| completion.name)
        .collect::<Vec<_>>();
    assert!(status_member_names.contains(&"Ready".to_string()));
    assert!(status_member_names.contains(&"Failed".to_string()));
    assert_eq!(
        builder
            .member_completions(&Type::named("Status"))
            .into_iter()
            .find(|completion| completion.name == "Failed")
            .map(|completion| completion.detail),
        Some("Failed(code: own int32, reason: own str) -> Status".to_string())
    );
    let ready_member = builder
        .resolve_member_type(&Type::named("Status"), "Ready")
        .expect("enum variants should resolve as static members");
    assert!(ready_member.hover.contains("Status"));
    assert!(ready_member.hover.contains("Ready"));
    assert!(ready_member.definition.is_some());
    assert!(
        builder
            .resolve_member_type(&Type::named("Status"), "Missing")
            .is_none(),
        "unknown enum variants should not resolve"
    );
    let option_member_names = builder
        .member_completions(&Type::named("Option"))
        .into_iter()
        .map(|completion| completion.name)
        .collect::<Vec<_>>();
    assert!(option_member_names.contains(&"Some".to_string()));
    assert!(option_member_names.contains(&"None".to_string()));
    let local_status = builder
        .resolve_match_variant_enum("Status")
        .expect("local enum should resolve as a match variant enum");
    assert!(local_status.hover.contains("enum Status"));
    assert!(local_status.definition.is_some());
    let inferred_status_variant = builder
        .resolve_match_variant(
            Some(&Type::named("Status")),
            &VariantPattern {
                enum_name: None,
                variant_name: "Ready".to_string(),
                subpatterns: Vec::new(),
                span: Span::new(1, 1),
            },
        )
        .expect("inferred user enum variants should resolve in match patterns");
    assert!(inferred_status_variant
        .hover
        .contains("variant Ready -> Status"));
    assert!(inferred_status_variant.definition.is_some());
    let imported_variant = builder
        .resolve_match_variant(
            Some(&Type::named("pkg.tools.RemoteStatus")),
            &VariantPattern {
                enum_name: Some("pkg.tools.RemoteStatus".to_string()),
                variant_name: "Failed".to_string(),
                subpatterns: Vec::new(),
                span: Span::new(1, 1),
            },
        )
        .expect("qualified imported enum variants should resolve in match patterns");
    assert!(imported_variant
        .hover
        .contains("variant Failed(code: own int32, reason: own str) -> pkg.tools.RemoteStatus"));
    assert!(imported_variant.definition.is_some());
    assert_eq!(
        builder
            .member_completions(&Type::named("pkg.tools.RemoteStatus"))
            .into_iter()
            .find(|completion| completion.name == "Failed")
            .map(|completion| completion.detail),
        Some("Failed(code: own int32, reason: own str) -> pkg.tools.RemoteStatus".to_string())
    );
    assert!(builder
        .resolve_member_type(&Type::named("pkg.tools.RemoteStatus"), "Failed")
        .expect("qualified imported enum variants should resolve as static members")
        .hover
        .contains("variant Failed(code: own int32, reason: own str) -> pkg.tools.RemoteStatus"));
    let remote_status = builder
        .resolve_match_variant_enum("pkg.tools.RemoteStatus")
        .expect("qualified imported enum should resolve as a match variant enum");
    assert!(remote_status.hover.contains("enum pkg.tools.RemoteStatus"));
    assert!(remote_status.definition.is_some());
    assert!(builder
        .resolve_match_variant_enum("SendError")
        .expect("builtin SendError should resolve as a match variant enum")
        .hover
        .contains("SendError[T]"));
    assert!(builder
        .resolve_member_type(&Type::named("WaitAny"), "Ready")
        .expect("WaitAny.Ready should resolve")
        .hover
        .contains("variant Ready(own int64, own T) -> WaitAny"));
    assert!(builder
        .resolve_member_type(&Type::named("WaitAny"), "Error")
        .expect("WaitAny.Error should resolve")
        .hover
        .contains("variant Error(own int64, own str) -> WaitAny"));
    assert!(builder
        .resolve_member_type(&Type::named("WaitAll"), "Ready")
        .expect("WaitAll.Ready should resolve")
        .hover
        .contains("variant Ready(own list[T]) -> WaitAll"));
    assert!(builder
        .resolve_member_type(&Type::named("WaitAll"), "Error")
        .expect("WaitAll.Error should resolve")
        .hover
        .contains("variant Error(own int64, own str) -> WaitAll"));
    assert_eq!(
        builtin_enum_variant_completions("WaitAny")
            .into_iter()
            .find(|completion| completion.name == "Ready")
            .map(|completion| completion.detail),
        Some("Ready(own int64, own T) -> WaitAny".to_string())
    );
    assert_eq!(
        builtin_enum_variant_completions("WaitAll")
            .into_iter()
            .find(|completion| completion.name == "Error")
            .map(|completion| completion.detail),
        Some("Error(own int64, own str) -> WaitAll".to_string())
    );

    let string_member_names = builder
        .member_completions(&Type::named("str"))
        .into_iter()
        .map(|completion| completion.name)
        .collect::<Vec<_>>();
    assert!(string_member_names.contains(&"split".to_string()));
    assert!(string_member_names.contains(&"trim".to_string()));
    assert!(string_member_names.contains(&"strip_prefix".to_string()));

    let task_group_member_names = builder
        .member_completions(&Type::named("TaskGroup"))
        .into_iter()
        .map(|completion| completion.name)
        .collect::<Vec<_>>();
    assert!(task_group_member_names.contains(&"start".to_string()));
    assert!(task_group_member_names.contains(&"start_soon".to_string()));
    assert!(task_group_member_names.contains(&"start_with_stack".to_string()));
    assert!(task_group_member_names.contains(&"start_soon_with_stack".to_string()));

    let assert_resolved_member =
        |receiver: Type, field: &str, hover_fragment: &str, expected_ty: Type| {
            let member = builder
                .resolve_member_type(&receiver, field)
                .unwrap_or_else(|| panic!("expected {receiver}.{field} to resolve"));
            assert!(
                member.hover.contains(hover_fragment),
                "hover for {receiver}.{field} should mention {hover_fragment}: {}",
                member.hover
            );
            assert_eq!(member.definition, None);
            assert_eq!(member.ty, Some(expected_ty), "{receiver}.{field}");
        };
    assert_resolved_member(
        Type::named("TaskGroup"),
        "start",
        "Task[T]",
        Type::Named("Task".to_string(), vec![Type::Unit]),
    );
    assert_resolved_member(
        Type::named("TaskGroup"),
        "start_soon",
        "start_soon",
        Type::Unit,
    );
    assert_resolved_member(
        Type::named("TaskGroup"),
        "start_with_stack",
        "bytes: int64",
        Type::Named("Task".to_string(), vec![Type::Unit]),
    );
    assert_resolved_member(
        Type::named("TaskGroup"),
        "start_soon_with_stack",
        "bytes: int64",
        Type::Unit,
    );
    assert_resolved_member(Type::named("Option"), "Some", "Some", Type::named("Option"));
    assert_resolved_member(Type::named("Option"), "None", "None", Type::named("Option"));
    assert_resolved_member(Type::named("Result"), "Ok", "Ok", Type::named("Result"));
    assert_resolved_member(Type::named("Result"), "Err", "Err", Type::named("Result"));
    assert_resolved_member(
        Type::named("SendError"),
        "Closed",
        "Closed",
        Type::named("SendError"),
    );
    assert_resolved_member(
        Type::named("QueueReceive"),
        "Item",
        "Item",
        Type::named("QueueReceive"),
    );
    assert_resolved_member(
        Type::named("QueueReceive"),
        "TimedOut",
        "TimedOut",
        Type::named("QueueReceive"),
    );
    assert_resolved_member(
        Type::named("TaskResult"),
        "Ready",
        "Ready",
        Type::named("TaskResult"),
    );
    assert_resolved_member(
        Type::named("TaskResult"),
        "Error",
        "Error",
        Type::named("TaskResult"),
    );
    assert_resolved_member(
        Type::named("TaskResult"),
        "Cancelled",
        "Cancelled",
        Type::named("TaskResult"),
    );
    assert_resolved_member(
        Type::named("WaitAny"),
        "Ready",
        "Ready",
        Type::named("WaitAny"),
    );
    assert_resolved_member(
        Type::named("WaitAny"),
        "Error",
        "Error",
        Type::named("WaitAny"),
    );
    assert_resolved_member(
        Type::named("WaitAny"),
        "TimedOut",
        "TimedOut",
        Type::named("WaitAny"),
    );
    assert_resolved_member(
        Type::named("WaitAll"),
        "Ready",
        "Ready",
        Type::named("WaitAll"),
    );
    assert_resolved_member(
        Type::named("WaitAll"),
        "Error",
        "Error",
        Type::named("WaitAll"),
    );
    assert_resolved_member(
        Type::named("WaitAll"),
        "Cancelled",
        "Cancelled",
        Type::named("WaitAll"),
    );

    let scope = BTreeMap::from([
        (
            "numbers".to_string(),
            super::BindingInfo {
                ty: Type::Named("list".to_string(), vec![Type::named("int32")]),
                trait_bounds: Vec::new(),
                definition: super::AnalysisRange {
                    file_path: None,
                    line: 0,
                    start_character: 0,
                    end_character: 7,
                },
                hover: "binding numbers: list[int32]".to_string(),
            },
        ),
        (
            "mapping".to_string(),
            super::BindingInfo {
                ty: Type::Named(
                    "dict".to_string(),
                    vec![Type::named("str"), Type::named("int32")],
                ),
                trait_bounds: Vec::new(),
                definition: super::AnalysisRange {
                    file_path: None,
                    line: 0,
                    start_character: 0,
                    end_character: 7,
                },
                hover: "binding mapping: dict[str, int32]".to_string(),
            },
        ),
        (
            "task".to_string(),
            super::BindingInfo {
                ty: Type::Named("Task".to_string(), vec![Type::named("int32")]),
                trait_bounds: Vec::new(),
                definition: super::AnalysisRange {
                    file_path: None,
                    line: 0,
                    start_character: 0,
                    end_character: 4,
                },
                hover: "binding task: Task[int32]".to_string(),
            },
        ),
        (
            "tasks".to_string(),
            super::BindingInfo {
                ty: Type::Named(
                    "list".to_string(),
                    vec![Type::Named("Task".to_string(), vec![Type::named("int32")])],
                ),
                trait_bounds: Vec::new(),
                definition: super::AnalysisRange {
                    file_path: None,
                    line: 0,
                    start_character: 0,
                    end_character: 5,
                },
                hover: "binding tasks: list[Task[int32]]".to_string(),
            },
        ),
    ]);

    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::List(vec![
                expr(ExprKind::Int(1)),
                expr(ExprKind::Int(2))
            ])),
            &scope,
        ),
        Some(Type::Named("list".to_string(), vec![Type::named("int64")]))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Set(vec![
                expr(ExprKind::String("a".to_string())),
                expr(ExprKind::String("b".to_string())),
            ])),
            &scope,
        ),
        Some(Type::Named("set".to_string(), vec![Type::named("str")]))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Map(vec![crate::ast::MapEntryExpr {
                key: expr(ExprKind::String("a".to_string())),
                value: expr(ExprKind::Int(1)),
            }])),
            &scope,
        ),
        Some(Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int64")],
        ))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Call {
                callee: Box::new(expr(ExprKind::Name("wait_any".to_string()))),
                args: vec![arg(expr(ExprKind::Name("tasks".to_string())))],
            }),
            &scope,
        ),
        Some(Type::Named(
            "WaitAny".to_string(),
            vec![Type::named("int32")],
        ))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Call {
                callee: Box::new(expr(ExprKind::Name("wait_all".to_string()))),
                args: vec![arg(expr(ExprKind::Name("tasks".to_string())))],
            }),
            &scope,
        ),
        Some(Type::Named(
            "WaitAll".to_string(),
            vec![Type::named("int32")],
        ))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Call {
                callee: Box::new(expr(ExprKind::Member {
                    object: Box::new(expr(ExprKind::Name("task".to_string()))),
                    field: "result".to_string(),
                })),
                args: Vec::new(),
            }),
            &scope,
        ),
        Some(Type::Named(
            "TaskResult".to_string(),
            vec![Type::named("int32")],
        ))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Call {
                callee: Box::new(expr(ExprKind::Member {
                    object: Box::new(expr(ExprKind::Name("task".to_string()))),
                    field: "result_or_none".to_string(),
                })),
                args: Vec::new(),
            }),
            &scope,
        ),
        Some(Type::Named(
            "Option".to_string(),
            vec![Type::named("int32")],
        ))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Try(Box::new(expr(ExprKind::Call {
                callee: Box::new(expr(ExprKind::Name("resultify".to_string()))),
                args: vec![arg(expr(ExprKind::Int(7)))],
            })))),
            &scope,
        ),
        Some(Type::named("int32"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Index {
                object: Box::new(expr(ExprKind::Name("numbers".to_string()))),
                index: Box::new(expr(ExprKind::Int(0))),
            }),
            &scope,
        ),
        Some(Type::named("int32"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Index {
                object: Box::new(expr(ExprKind::Name("mapping".to_string()))),
                index: Box::new(expr(ExprKind::String("a".to_string()))),
            }),
            &scope,
        ),
        Some(Type::named("int32"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Cast {
                expr: Box::new(expr(ExprKind::Int(1))),
                ty: type_ref("str"),
            }),
            &scope,
        ),
        Some(Type::named("str"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Unary {
                op: crate::ast::UnaryOp::Not,
                expr: Box::new(expr(ExprKind::Bool(false))),
            }),
            &scope,
        ),
        Some(Type::named("bool"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Unary {
                op: crate::ast::UnaryOp::Neg,
                expr: Box::new(expr(ExprKind::Float(1.5))),
            }),
            &scope,
        ),
        Some(Type::named("float64"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Group(Box::new(expr(ExprKind::Int(3))))),
            &scope
        ),
        Some(Type::named("int64"))
    );
    assert_eq!(
        builder.infer_expr_type(&expr(ExprKind::Name("pkg".to_string())), &scope),
        Some(Type::Module("pkg".to_string()))
    );
    assert_eq!(
        builder.infer_expr_type(&expr(ExprKind::Name("Status".to_string())), &scope),
        Some(Type::named("Status"))
    );
    assert_eq!(
        builder.infer_expr_type(&expr(ExprKind::Name("helper".to_string())), &scope),
        Some(Type::Function {
            params: Vec::new(),
            return_type: Box::new(Type::named("int32")),
        })
    );
    for builtin_name in [
        "SendError",
        "QueueReceive",
        "TaskResult",
        "WaitAny",
        "WaitAll",
        "Queue",
        "TaskGroup",
    ] {
        assert_eq!(
            builder.infer_expr_type(&expr(ExprKind::Name(builtin_name.to_string())), &scope),
            Some(Type::named(builtin_name)),
            "{builtin_name} should infer as a builtin type constructor"
        );
    }
    for (builtin_name, args) in [
        ("SendError", vec![type_ref("int32")]),
        ("Queue", vec![type_ref("str")]),
        ("list", vec![type_ref("int32")]),
        ("set", vec![type_ref("str")]),
        ("dict", vec![type_ref("str"), type_ref("int32")]),
        ("Task", vec![type_ref("int32")]),
    ] {
        assert_eq!(
            builder.infer_expr_type(
                &expr(ExprKind::Specialize {
                    expr: Box::new(expr(ExprKind::Name(builtin_name.to_string()))),
                    type_args: args.clone(),
                }),
                &scope,
            ),
            Some(Type::Named(
                builtin_name.to_string(),
                args.into_iter().map(|arg| lower_type_ref(&arg)).collect(),
            )),
            "{builtin_name} specialization should infer its generic type"
        );
    }
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("helper".to_string()))),
                type_args: vec![type_ref("int32")],
            }),
            &scope,
        ),
        Some(Type::Function {
            params: Vec::new(),
            return_type: Box::new(Type::named("int32")),
        })
    );
    assert_eq!(Type::Unit.type_arguments(), &[]);
    assert_eq!(Type::Module("pkg".to_string()).type_arguments(), &[]);
    assert_eq!(Type::TypeParam("T".to_string()).type_arguments(), &[]);
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("numbers".to_string()))),
                field: "len".to_string(),
            }),
            &scope,
        ),
        Some(Type::named("int64"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Match {
                scrutinee: Box::new(expr(ExprKind::Name("numbers".to_string()))),
                capability: ReceiverKind::Borrow,
                arms: vec![crate::ast::MatchExprArm {
                    guard: None,
                    pattern: crate::ast::Pattern::Wildcard(Span::new(1, 1)),
                    value: expr(ExprKind::Int(4)),
                    span: Span::new(1, 1),
                }],
            }),
            &scope,
        ),
        Some(Type::named("int64"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Try(Box::new(expr(ExprKind::Int(1))))),
            &scope,
        ),
        None
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Index {
                object: Box::new(expr(ExprKind::String("abc".to_string()))),
                index: Box::new(expr(ExprKind::Int(0))),
            }),
            &scope,
        ),
        None
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Binary {
                op: BinaryOp::And,
                left: Box::new(expr(ExprKind::Bool(true))),
                right: Box::new(expr(ExprKind::Bool(false))),
            }),
            &scope,
        ),
        Some(Type::named("bool"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Binary {
                op: BinaryOp::Eq,
                left: Box::new(expr(ExprKind::Int(1))),
                right: Box::new(expr(ExprKind::Int(1))),
            }),
            &scope,
        ),
        Some(Type::named("bool"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Binary {
                op: BinaryOp::Add,
                left: Box::new(expr(ExprKind::Int(1))),
                right: Box::new(expr(ExprKind::Float(2.0))),
            }),
            &scope,
        ),
        Some(Type::named("float64"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Binary {
                op: BinaryOp::Add,
                left: Box::new(expr(ExprKind::String("left".to_string()))),
                right: Box::new(expr(ExprKind::Int(2))),
            }),
            &scope,
        ),
        None
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Name("wait_any".to_string())),
            &[arg(expr(ExprKind::Name("numbers".to_string())))],
            &scope,
        ),
        None
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Name("wait_any".to_string())),
            &[arg(expr(ExprKind::Int(1)))],
            &scope,
        ),
        None
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Name("wait_all".to_string())),
            &[arg(expr(ExprKind::Name("numbers".to_string())))],
            &scope,
        ),
        None
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Name("wait_all".to_string())),
            &[arg(expr(ExprKind::Int(1)))],
            &scope,
        ),
        None
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("Option".to_string()))),
                field: "Some".to_string(),
            }),
            &[arg(expr(ExprKind::Int(1)))],
            &scope,
        ),
        Some(Type::Named(
            "Option".to_string(),
            vec![Type::named("int64")]
        ))
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Member {
                object: Box::new(expr(ExprKind::Name("Result".to_string()))),
                field: "Err".to_string(),
            }),
            &[arg(expr(ExprKind::String("no".to_string())))],
            &scope,
        ),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::Unit, Type::named("str")],
        ))
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("Task".to_string()))),
                type_args: vec![type_ref("int32")],
            }),
            &[],
            &scope,
        ),
        Some(Type::Named("Task".to_string(), vec![Type::named("int32")]))
    );
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Specialize {
                expr: Box::new(expr(ExprKind::Name("helper".to_string()))),
                type_args: vec![type_ref("int32")],
            }),
            &[],
            &scope,
        ),
        Some(Type::named("int32"))
    );
    assert_eq!(
        builder.infer_call_type(&expr(ExprKind::Int(1)), &[], &scope),
        None
    );
    assert_eq!(
        builder.infer_iterable_binding_type(
            &expr(ExprKind::Set(vec![expr(ExprKind::String("a".to_string()))])),
            &scope,
        ),
        Some(Type::named("str"))
    );
    assert_eq!(
        builder.match_binding_type(
            Some(&Type::Named(
                "Result".to_string(),
                vec![Type::named("int32"), Type::named("str")],
            )),
            None,
            "Err",
        ),
        Some(Type::named("str"))
    );
    assert_eq!(
        builder.match_binding_type(
            Some(&Type::Named(
                "Option".to_string(),
                vec![Type::named("int32")],
            )),
            None,
            "Some",
        ),
        Some(Type::named("int32"))
    );
    assert_eq!(
        builder.match_binding_type(
            Some(&Type::Named(
                "Result".to_string(),
                vec![Type::named("int32"), Type::named("str")],
            )),
            None,
            "Ok",
        ),
        Some(Type::named("int32"))
    );
    assert_eq!(
        builder.match_binding_type(
            Some(&Type::Named(
                "SendError".to_string(),
                vec![Type::named("str")],
            )),
            None,
            "Closed",
        ),
        Some(Type::named("str"))
    );
}

#[test]
fn analysis_import_and_match_resolution_helpers_cover_fallbacks() {
    let source = [
        "import pkg.types",
        "",
        "enum Status:",
        "    Ready",
        "    Failed(str)",
        "",
        "def inspect(status: own Status, value: Option[int32]) -> int32:",
        "    match status:",
        "        case Status.Ready:",
        "            return 1",
        "        case Status.Failed(reason):",
        "            return 2",
        "    match value:",
        "        case Some(found):",
        "            return found",
        "        case None:",
        "            return 0",
    ]
    .join("\n");
    let mut program = checked_program(&source);
    program.source_path = Some("/tmp/main.au".to_string());
    program.imported_modules.insert(
        "pkg".to_string(),
        crate::sema::ModuleNamespace {
            constants: BTreeMap::new(),
            all_constants: BTreeMap::new(),
            name: "pkg".to_string(),
            path: "pkg".to_string(),
            source_path: None,
            closures: Default::default(),
            comprehensions: Default::default(),
            modules: std::collections::BTreeMap::from([(
                "types".to_string(),
                crate::sema::ModuleNamespace {
                    constants: BTreeMap::new(),
                    all_constants: BTreeMap::new(),
                    name: "types".to_string(),
                    path: "pkg.types".to_string(),
                    source_path: None,
                    closures: Default::default(),
                    comprehensions: Default::default(),
                    modules: Default::default(),
                    functions: Default::default(),
                    extern_functions: Default::default(),
                    opaque_handles: Default::default(),
                    classes: Default::default(),
                    enums: Default::default(),
                    traits: Default::default(),
                    trait_impls: Vec::new(),
                    all_functions: Default::default(),
                    all_extern_functions: Default::default(),
                    all_opaque_handles: Default::default(),
                    all_classes: Default::default(),
                    all_enums: Default::default(),
                    all_traits: Default::default(),
                    imported_modules: Default::default(),
                },
            )]),
            functions: Default::default(),
            extern_functions: Default::default(),
            opaque_handles: Default::default(),
            classes: Default::default(),
            enums: Default::default(),
            traits: Default::default(),
            trait_impls: Vec::new(),
            all_functions: Default::default(),
            all_extern_functions: Default::default(),
            all_opaque_handles: Default::default(),
            all_classes: Default::default(),
            all_enums: Default::default(),
            all_traits: Default::default(),
            imported_modules: Default::default(),
        },
    );

    let builder = AnalysisBuilder::new(&source, &program, Vec::new());
    assert_eq!(
        builder.current_source_path().as_deref(),
        Some("/tmp/main.au")
    );
    let import_range = builder
        .find_imported_module_range("pkg.types")
        .expect("import range should fall back to current file");
    assert_eq!(import_range.file_path.as_deref(), Some("/tmp/main.au"));
    assert_eq!(import_range.line, 0);
    assert!(
        builder
            .find_imported_module_range("pkg.types.inner")
            .is_none(),
        "longer target paths should not match shorter imports"
    );
    let mismatched_source_builder =
        AnalysisBuilder::new("def other():\n    pass\n", &program, vec![]);
    assert!(
        mismatched_source_builder
            .find_imported_module_range("pkg.types")
            .is_none(),
        "fallback import ranges require the token to be present on the source line"
    );

    let option_symbol = builder
        .resolve_match_variant_enum("Option")
        .expect("builtin Option enum should resolve");
    assert!(option_symbol.definition.is_none());
    assert!(option_symbol.hover.contains("Option[T]"));

    let status_symbol = builder
        .resolve_match_variant_enum("Status")
        .expect("named enum should resolve");
    assert!(status_symbol.definition.is_some());
    assert!(status_symbol.hover.contains("enum Status"));

    let builtin_variant = builder
        .resolve_match_variant(
            Some(&Type::Named(
                "Option".to_string(),
                vec![Type::named("int32")],
            )),
            &VariantPattern {
                enum_name: None,
                variant_name: "Some".to_string(),
                subpatterns: vec![crate::ast::Pattern::Binding(crate::ast::BindingPattern {
                    name: "found".to_string(),
                    span: Span::new(14, 14),
                })],
                span: Span::new(14, 14),
            },
        )
        .expect("builtin variant should resolve");
    assert!(builtin_variant.definition.is_none());
    assert!(builtin_variant.hover.contains("Some"));
    assert!(builtin_variant.hover.contains("int32"));

    let named_variant = builder
        .resolve_match_variant(
            Some(&Type::named("Status")),
            &VariantPattern {
                enum_name: Some("Status".to_string()),
                variant_name: "Failed".to_string(),
                subpatterns: vec![crate::ast::Pattern::Binding(crate::ast::BindingPattern {
                    name: "reason".to_string(),
                    span: Span::new(10, 14),
                })],
                span: Span::new(10, 14),
            },
        )
        .expect("named enum variant should resolve");
    assert!(named_variant.definition.is_some());
    assert!(named_variant.hover.contains("Failed"));
    assert!(named_variant.hover.contains("str"));

    let inferred_named_variant = builder
        .resolve_match_variant(
            Some(&Type::named("Status")),
            &VariantPattern {
                enum_name: None,
                variant_name: "Failed".to_string(),
                subpatterns: Vec::new(),
                span: Span::new(10, 14),
            },
        )
        .expect("scrutinee-inferred enum variants should resolve");
    assert!(inferred_named_variant.definition.is_some());

    let result_err_variant = builder
        .resolve_match_variant(
            Some(&Type::Named(
                "Result".to_string(),
                vec![Type::named("int32"), Type::named("str")],
            )),
            &VariantPattern {
                enum_name: None,
                variant_name: "Err".to_string(),
                subpatterns: Vec::new(),
                span: Span::new(12, 14),
            },
        )
        .expect("builtin Result.Err should resolve");
    assert!(result_err_variant.hover.contains("str"));

    let send_cancelled_variant = builder
        .resolve_match_variant(
            Some(&Type::Named(
                "SendError".to_string(),
                vec![Type::named("int32")],
            )),
            &VariantPattern {
                enum_name: None,
                variant_name: "Cancelled".to_string(),
                subpatterns: Vec::new(),
                span: Span::new(13, 14),
            },
        )
        .expect("builtin SendError.Cancelled should resolve");
    assert!(send_cancelled_variant.hover.contains("int32"));

    assert!(
        builder
            .resolve_match_variant(
                Some(&Type::Named(
                    "Option".to_string(),
                    vec![Type::named("int32")],
                )),
                &VariantPattern {
                    enum_name: None,
                    variant_name: "Missing".to_string(),
                    subpatterns: Vec::new(),
                    span: Span::new(14, 14),
                },
            )
            .is_none(),
        "unknown builtin enum variants should fall through to named enum resolution"
    );
}

#[test]
fn analysis_completion_helpers_cover_top_level_module_and_enum_surfaces() {
    let source = [
        "import pkg",
        "",
        "trait Show:",
        "    def show(self) -> str",
        "",
        "enum Status:",
        "    Ready",
        "    Failed(str)",
        "",
        "class Local:",
        "    value: int32",
        "",
        "def helper() -> int32:",
        "    return 1",
    ]
    .join("\n");
    let mut program = checked_program(&source);
    let remote_source = [
        "trait RemoteTrait:",
        "    def show(self) -> str",
        "",
        "enum RemoteStatus:",
        "    Ready",
        "",
        "class Remote:",
        "    value: int32",
        "",
        "def remote_fn() -> int32:",
        "    return 7",
    ]
    .join("\n");
    let remote_program = checked_program(&remote_source);
    let tools_namespace = crate::sema::ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "tools".to_string(),
        path: "pkg.tools".to_string(),
        source_path: None,
        closures: Default::default(),
        comprehensions: Default::default(),
        modules: Default::default(),
        functions: remote_program.functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: remote_program.classes.clone(),
        enums: remote_program.enums.clone(),
        traits: remote_program.traits.clone(),
        trait_impls: remote_program.trait_impls.clone(),
        all_functions: remote_program.functions.clone(),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: remote_program.classes.clone(),
        all_enums: remote_program.enums.clone(),
        all_traits: remote_program.traits.clone(),
        imported_modules: Default::default(),
    };
    program.imported_modules.insert(
        "pkg".to_string(),
        crate::sema::ModuleNamespace {
            constants: BTreeMap::new(),
            all_constants: BTreeMap::new(),
            name: "pkg".to_string(),
            path: "pkg".to_string(),
            source_path: None,
            closures: Default::default(),
            comprehensions: Default::default(),
            modules: std::collections::BTreeMap::from([(
                "tools".to_string(),
                tools_namespace.clone(),
            )]),
            functions: Default::default(),
            extern_functions: Default::default(),
            opaque_handles: Default::default(),
            classes: Default::default(),
            enums: Default::default(),
            traits: Default::default(),
            trait_impls: Vec::new(),
            all_functions: Default::default(),
            all_extern_functions: Default::default(),
            all_opaque_handles: Default::default(),
            all_classes: Default::default(),
            all_enums: Default::default(),
            all_traits: Default::default(),
            imported_modules: Default::default(),
        },
    );

    let builder = AnalysisBuilder::new(&source, &program, Vec::new());
    let top_level_names = builder
        .top_level_completions()
        .into_iter()
        .map(|completion| completion.name)
        .collect::<Vec<_>>();
    assert!(top_level_names.contains(&"Local".to_string()));
    assert!(top_level_names.contains(&"Status".to_string()));
    assert!(top_level_names.contains(&"Show".to_string()));
    assert!(top_level_names.contains(&"helper".to_string()));
    assert!(top_level_names.contains(&"print".to_string()));
    assert!(top_level_names.contains(&"Option".to_string()));
    assert!(top_level_names.contains(&"pkg".to_string()));

    let module_names = builder
        .member_completions(&Type::Module("pkg.tools".to_string()))
        .into_iter()
        .map(|completion| completion.name)
        .collect::<Vec<_>>();
    assert!(module_names.contains(&"remote_fn".to_string()));
    assert!(module_names.contains(&"Remote".to_string()));
    assert!(module_names.contains(&"RemoteStatus".to_string()));
    assert!(module_names.contains(&"RemoteTrait".to_string()));
    assert!(
        builder
            .member_completions(&Type::Module("pkg.missing".to_string()))
            .is_empty(),
        "unknown module namespaces should complete to an empty member list"
    );

    let enum_names = builder
        .member_completions(&Type::named("Status"))
        .into_iter()
        .map(|completion| completion.name)
        .collect::<Vec<_>>();
    assert!(enum_names.contains(&"Ready".to_string()));
    assert!(enum_names.contains(&"Failed".to_string()));

    assert_eq!(
        builder.match_binding_type(None, Some("Status"), "Failed"),
        Some(Type::named("str"))
    );
    assert_eq!(
        builder.match_binding_type(
            Some(&Type::Named(
                "SendError".to_string(),
                vec![Type::named("int32")]
            )),
            None,
            "Cancelled"
        ),
        Some(Type::named("int32"))
    );
}

#[test]
fn complete_path_source_recovers_imported_module_member_completion() {
    let temp = TempDir::new("analysis-complete-path");
    fs::create_dir_all(temp.path().join("helpers")).expect("should create helper module dir");
    fs::write(
        temp.path().join("helpers").join("math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\n",
    )
    .expect("should write helper module");

    let source = "import helpers.math\n\ndef main() -> int32:\n    helpers.math.\n    return 0\n";
    let path = temp.path().join("main.au");
    fs::write(&path, source).expect("should write main module");
    let line_index = source
        .lines()
        .position(|line| line.contains("helpers.math."))
        .expect("source should contain member access");
    let line_text = source.lines().nth(line_index).unwrap();
    let character = line_text.rfind('.').unwrap() + 1;

    let completions = complete_path_source(&path, source, line_index, character, Some('.'))
        .expect("path-aware completion should recover");
    let names = completions
        .into_iter()
        .map(|item| item.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"double".to_string()));
}

#[test]
fn completion_scope_tracks_nested_statement_bindings() {
    let source = [
        "class FileHandle:",
        "    name: str",
        "",
        "    def close(mut self):",
        "        pass",
        "",
        "class Counter:",
        "    value: int32",
        "",
        "    def inspect(self) -> int32:",
        "        print(self.value)",
        "        return self.value",
        "",
        "def scoped(value: int32) -> int32:",
        "    jobs = Queue[int32]()",
        "    if value > 0:",
        "        positive = value",
        "        print(positive)",
        "    else:",
        "        negative = value",
        "        print(negative)",
        "    match value:",
        "        case 0:",
        "            zero = value",
        "            print(zero)",
        "        case _:",
        "            wildcard = value",
        "            print(wildcard)",
        "    for item in [1, 2, 3]:",
        "        print(item)",
        "    with TaskGroup() as group:",
        "        print(group.cancel())",
        "    match jobs.get(timeout=1ms):",
        "        case QueueReceive.Item(received):",
        "            print(received)",
        "        case _:",
        "            pass",
        "    while value > 0:",
        "        loop_value = value",
        "        print(loop_value)",
        "        break",
        "    return value",
        "",
        "top = 1",
        "print(top)",
    ]
    .join("\n");

    let program = checked_program(&source);
    let builder = AnalysisBuilder::new(&source, &program, Vec::new());
    let checks = [
        ("print(self.value)", "self"),
        ("print(positive)", "positive"),
        ("print(negative)", "negative"),
        ("print(zero)", "zero"),
        ("print(wildcard)", "wildcard"),
        ("print(item)", "item"),
        ("print(group.cancel())", "group"),
        ("print(received)", "received"),
        ("print(loop_value)", "loop_value"),
        ("print(top)", "top"),
    ];

    for (needle, expected) in checks {
        let line_index = source
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("source should contain `{needle}`"));
        let completions = builder.scope_for_line(line_index);
        assert!(
            completions.contains_key(expected),
            "completion scope for `{needle}` should include `{expected}`"
        );
    }
}

#[test]
fn compiler_analysis_accepts_builtin_named_arguments() {
    let source = include_str!("../../../examples/basics/named_builtin_arguments.au");
    let analysis = analyze_source(source);

    assert!(analysis.diagnostics.is_empty());
}

#[test]
fn compiler_analysis_handles_named_wait_any_timeout() {
    let source = "def worker(value: int32) -> int32:\n    return value\n\ndef main() -> int32:\n    with TaskGroup() as group:\n        mut tasks = list[Task[int32]]()\n        tasks.append(group.start(worker, 1))\n        print(wait_any(tasks, timeout=5ms))\n    return 0\n";
    let analysis = analyze_source(source);

    assert!(analysis.diagnostics.is_empty());
    assert!(analysis.occurrences.iter().any(|occurrence| occurrence
        .hover
        .contains("wait_any(tasks: list[Task[T]], timeout: Duration = ...) -> WaitAny[T]")));
}

#[test]
fn compiler_analysis_preserves_real_ownership_diagnostic_metadata() {
    let source = "def take(value: str) -> str:\n    return value\n";
    let analysis = analyze_source(source);

    assert_eq!(analysis.diagnostics.len(), 1);
    let diagnostic = &analysis.diagnostics[0];
    assert_eq!(diagnostic.code, "AU3002");
    assert_eq!((diagnostic.line, diagnostic.start_character), (1, 11));
    assert_eq!(diagnostic.secondary_spans.len(), 1);
    assert_eq!(
        (
            diagnostic.secondary_spans[0].line,
            diagnostic.secondary_spans[0].start_character,
            diagnostic.secondary_spans[0].label.as_str(),
        ),
        (0, 9, "parameter `value` is borrowed here")
    );
    assert_eq!(
        diagnostic.help,
        ["declare the parameter as `own str` when the function should consume it, or call `.clone()` to consume an independent copy"]
    );
    assert_eq!(diagnostic.edits.len(), 1);
    assert_eq!(
        (
            diagnostic.edits[0].line,
            diagnostic.edits[0].start_character,
            diagnostic.edits[0].end_character,
            diagnostic.edits[0].replacement.as_str(),
            diagnostic.edits[0].applicability.as_str(),
        ),
        (1, 16, 16, ".clone()", "machine-applicable")
    );
    assert!(
        crate::check_source("def take(value: str) -> str:\n    return value.clone()\n").is_ok()
    );
}

#[test]
fn compiler_analysis_reports_provenance_for_representative_ownership_paths() {
    let cases = [
        (
            "def consume(value: own str):\n    pass\n\ndef main() -> int32:\n    value = \"x\"\n    consume(value)\n    print(value)\n    return 0\n",
            "AU3001",
            "use of moved value",
            true,
        ),
        (
            "def main() -> int32:\n    mut values = [1]\n    for value in values:\n        values.clear()\n    return 0\n",
            "AU3002",
            "borrowed for iteration",
            false,
        ),
        (
            "class Counter:\n    value: int32\n\n    def bump(self):\n        self.value += 1\n",
            "AU3003",
            "shared receiver `self`",
            false,
        ),
        (
            "class Data:\n    value: int32\n\ndef use(r: Data, w: mut Data):\n    pass\n\ndef main() -> int32:\n    mut data = Data(value=1)\n    use(data, data)\n    return 0\n",
            "AU3002",
            "overlaps borrow",
            false,
        ),
    ];

    for (source, code, message_fragment, has_safe_edit) in cases {
        let analysis = analyze_source(source);
        assert_eq!(analysis.diagnostics.len(), 1, "{message_fragment}");
        let diagnostic = &analysis.diagnostics[0];
        assert_eq!(diagnostic.code, code, "{message_fragment}");
        assert!(
            diagnostic.message.contains(message_fragment),
            "{}",
            diagnostic.message
        );
        assert_eq!(diagnostic.secondary_spans.len(), 1, "{message_fragment}");
        assert!(!diagnostic.secondary_spans[0].label.is_empty());
        assert!(!diagnostic.help.is_empty(), "{message_fragment}");
        assert_eq!(!diagnostic.edits.is_empty(), has_safe_edit);
    }
}

#[test]
fn compiler_member_completion_tolerates_dangling_dot_buffers() {
    let source = [
        "class Counter:",
        "    value: int32",
        "",
        "def main() -> int32:",
        "    counter = Counter(value=1)",
        "    counter.",
        "    return 0",
    ]
    .join("\n");

    let completions = complete_source(&source, 5, 12, Some('.')).expect("completion should work");
    let names = completions
        .into_iter()
        .map(|item| item.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"value".to_string()));
}

#[test]
fn compiler_member_completion_tolerates_dangling_dot_at_eof_buffers() {
    let source = [
        "class Counter:",
        "    value: int32",
        "",
        "def main() -> int32:",
        "    counter = Counter(value=1)",
        "    counter.",
    ]
    .join("\n");

    let completions = complete_source(&source, 5, 12, Some('.')).expect("completion should work");
    let names = completions
        .into_iter()
        .map(|item| item.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"value".to_string()));
}

#[test]
fn compiler_member_completion_tolerates_multiple_dangling_dot_buffers() {
    let source = [
        "class Counter:",
        "    value: int32",
        "",
        "def main() -> int32:",
        "    counter = Counter(value=1)",
        "    print(counter.",
        "    print(counter.",
        "    return 0",
    ]
    .join("\n");

    let completions = complete_source(&source, 5, 18, Some('.')).expect("completion should work");
    let names = completions
        .into_iter()
        .map(|item| item.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"value".to_string()));
}

#[test]
fn compiler_member_completion_survives_a_closing_delimiter_and_unrelated_diagnostic() {
    let source = [
        "def inspect(tags: list[str]):",
        "    first = tags[0].clone()",
        "    print(range(tags.))",
    ]
    .join("\n");
    let member_line = source.lines().nth(2).unwrap();
    let character = member_line.find("tags.").unwrap() + "tags.".len();

    let completions = complete_source(&source, 2, character, Some('.'))
        .expect("member completion should ignore unrelated diagnostics");
    let names = completions
        .into_iter()
        .map(|item| item.name)
        .collect::<BTreeSet<_>>();

    assert!(names.contains("append"));
    assert!(names.contains("get"));
    assert!(names.contains("len"));
}

#[test]
fn machine_readable_analysis_recovers_symbols_for_dangling_dot_buffers() {
    let source = [
        "class Counter:",
        "    value: int32",
        "",
        "def main() -> int32:",
        "    counter = Counter(value=1)",
        "    counter.",
        "    return 0",
    ]
    .join("\n");

    let analysis = analyze_source(&source);

    assert!(!analysis.symbols.is_empty());
    assert!(analysis
        .symbols
        .iter()
        .any(|symbol| symbol.kind == "class" && symbol.name == "Counter"));
    assert!(!analysis.occurrences.is_empty());
}

#[test]
fn machine_readable_analysis_recovers_symbols_for_dangling_dot_at_eof_buffers() {
    let source = [
        "class Counter:",
        "    value: int32",
        "",
        "def main() -> int32:",
        "    counter = Counter(value=1)",
        "    counter.",
    ]
    .join("\n");

    let analysis = analyze_source(&source);

    assert!(!analysis.symbols.is_empty());
    assert!(analysis
        .symbols
        .iter()
        .any(|symbol| symbol.kind == "class" && symbol.name == "Counter"));
    assert!(!analysis.occurrences.is_empty());
}

#[test]
fn machine_readable_analysis_recovers_symbols_for_multiple_dangling_dot_buffers() {
    let source = [
        "class Counter:",
        "    value: int32",
        "",
        "def main() -> int32:",
        "    counter = Counter(value=1)",
        "    print(counter.",
        "    print(counter.",
        "    return 0",
    ]
    .join("\n");

    let analysis = analyze_source(&source);

    assert!(!analysis.symbols.is_empty());
    assert!(analysis
        .symbols
        .iter()
        .any(|symbol| symbol.kind == "class" && symbol.name == "Counter"));
    assert!(!analysis.occurrences.is_empty());
}

#[test]
fn path_aware_analysis_tracks_definitions_for_namespace_imported_symbols() {
    let path = repo_root().join("examples/modules/namespace_import_types.au");
    let source = std::fs::read_to_string(&path).expect("example should exist");
    let analysis = analyze_path_source(&path, &source);
    let types_path = fs::canonicalize(repo_root().join("examples/modules/pkg/types.au"))
        .expect("types path should canonicalize")
        .display()
        .to_string();

    assert!(analysis.diagnostics.is_empty());
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.hover.contains("module pkg.types")
            && occurrence
                .definition
                .as_ref()
                .and_then(|definition| definition.file_path.as_deref())
                == Some(types_path.as_str())
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.hover.contains("class Counter")
            && occurrence
                .definition
                .as_ref()
                .and_then(|definition| definition.file_path.as_deref())
                == Some(types_path.as_str())
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.hover.contains("enum pkg.types.Status")
            && occurrence
                .definition
                .as_ref()
                .and_then(|definition| definition.file_path.as_deref())
                == Some(types_path.as_str())
    }));
}

#[test]
fn path_aware_analysis_tracks_imported_function_field_and_trait_method_definitions() {
    let temp_dir = TempDir::new("aura-analysis-cross-file");
    fs::create_dir_all(temp_dir.path().join("pkg")).expect("failed to create pkg dir");
    let math_path = temp_dir.path().join("pkg/math.au");
    let named_path = temp_dir.path().join("pkg/named.au");
    let user_path = temp_dir.path().join("pkg/user.au");
    let main_path = temp_dir.path().join("main.au");

    fs::write(
        &math_path,
        "public def add(left: int32, right: int32) -> int32:\n    return left + right\n",
    )
    .expect("failed to write math module");
    fs::write(
        &named_path,
        "public trait Named:\n    def name(self) -> str\n",
    )
    .expect("failed to write named module");
    fs::write(
        &user_path,
        [
            "from pkg.named import Named",
            "",
            "public class User:",
            "    public label: str",
            "",
            "impl Named for User:",
            "    def name(self) -> str:",
            "        return self.label.clone()",
        ]
        .join("\n"),
    )
    .expect("failed to write user module");
    let source = [
        "from pkg.math import add",
        "from pkg.user import User",
        "",
        "def main() -> int32:",
        "    total = add(left=1, right=2)",
        "    user = User(label=\"Ada\")",
        "    print(user.label)",
        "    print(user.name())",
        "    return total",
    ]
    .join("\n");
    fs::write(&main_path, &source).expect("failed to write main module");

    let analysis = analyze_path_source(&main_path, &source);
    let math_path = fs::canonicalize(&math_path)
        .expect("math path should canonicalize")
        .display()
        .to_string();
    let user_path = fs::canonicalize(&user_path)
        .expect("user path should canonicalize")
        .display()
        .to_string();

    assert!(analysis.diagnostics.is_empty());
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.hover.contains("function add")
            && occurrence
                .definition
                .as_ref()
                .and_then(|definition| definition.file_path.as_deref())
                == Some(math_path.as_str())
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.hover.contains("class User")
            && occurrence
                .definition
                .as_ref()
                .and_then(|definition| definition.file_path.as_deref())
                == Some(user_path.as_str())
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.hover.contains("field label: str")
            && occurrence
                .definition
                .as_ref()
                .and_then(|definition| definition.file_path.as_deref())
                == Some(user_path.as_str())
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.hover.contains("method name(self) -> str")
            && occurrence
                .definition
                .as_ref()
                .and_then(|definition| definition.file_path.as_deref())
                == Some(user_path.as_str())
    }));
}

#[test]
fn path_aware_analysis_preserves_same_leaf_imported_generic_class_identity() {
    let temp_dir = TempDir::new("aura-analysis-imported-generic-identity");
    let remote_path = temp_dir.path().join("remote.au");
    let main_path = temp_dir.path().join("main.au");
    fs::write(
        &remote_path,
        r#"public class Holder[T]:
    public value: T
"#,
    )
    .expect("failed to write imported generic class");
    let source = r#"import remote

class Holder[T]:
    value: T

def main() -> int32:
    local = Holder[int64](1)
    imported: remote.Holder[str] = remote.Holder[str]("remote")
    print(local.value)
    print(imported.value)
    return 0
"#;
    fs::write(&main_path, source).expect("failed to write same-leaf analysis source");

    let analysis = analyze_path_source(&main_path, source);
    let canonical_remote_path = fs::canonicalize(&remote_path)
        .expect("remote path should canonicalize")
        .display()
        .to_string();
    assert!(
        analysis.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        analysis.diagnostics
    );
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence
            .hover
            .contains("binding imported: remote.Holder[str]")
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.hover.contains("field value: T")
            && occurrence
                .definition
                .as_ref()
                .and_then(|definition| definition.file_path.as_deref())
                == Some(canonical_remote_path.as_str())
    }));

    let completion_source = source.replace("    print(imported.value)\n", "    imported.\n");
    let completion_line = completion_source
        .lines()
        .position(|line| line.contains("imported."))
        .expect("completion source should contain imported receiver");
    let completion_character = completion_source
        .lines()
        .nth(completion_line)
        .expect("completion line should exist")
        .len();
    let completions = complete_path_source(
        &main_path,
        &completion_source,
        completion_line,
        completion_character,
        Some('.'),
    )
    .expect("qualified imported generic member completion should recover");
    assert_eq!(
        completions
            .iter()
            .filter(|completion| completion.name == "value")
            .count(),
        1,
        "the imported Holder field should complete exactly once"
    );
}

#[test]
fn analysis_records_variant_occurrences_inside_match_patterns() {
    let source = [
        "enum Status:",
        "    Ready",
        "    Busy",
        "",
        "def render(status: Status) -> int32:",
        "    match status:",
        "        case Status.Ready:",
        "            return 1",
        "        case Status.Busy:",
        "            return 0",
        "",
        "def render_unqualified(status: Status) -> int32:",
        "    match status:",
        "        case Ready:",
        "            return 1",
        "        case Busy:",
        "            return 0",
    ]
    .join("\n");

    let analysis = analyze_source(&source);

    assert!(analysis.diagnostics.is_empty());
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 6
            && occurrence.hover.contains("variant Ready")
            && occurrence.definition.is_some()
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 6
            && occurrence.hover.contains("enum Status")
            && occurrence.definition.is_some()
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 8
            && occurrence.hover.contains("variant Busy")
            && occurrence.definition.is_some()
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 13
            && occurrence.hover.contains("variant Ready")
            && occurrence.definition.is_some()
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 15
            && occurrence.hover.contains("variant Busy")
            && occurrence.definition.is_some()
    }));
}

#[test]
fn analysis_records_recursive_tuple_match_bindings_and_body_uses() {
    let source = [
        "def inspect(pair: (int32, (bool, str))):",
        "    match pair:",
        "        case (left, (ready, text)):",
        "            print(left)",
        "            print(ready)",
        "            print(text)",
    ]
    .join("\n");

    let analysis = analyze_source(&source);

    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    for (name, ty, use_line) in [
        ("left", "int32", 3),
        ("ready", "bool", 4),
        ("text", "str", 5),
    ] {
        let definition = analysis
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.line == 2 && occurrence.hover.contains(&format!("local {name}: {ty}"))
            })
            .unwrap_or_else(|| panic!("missing tuple-pattern definition occurrence for `{name}`"));
        assert_eq!(
            definition.definition.as_ref().map(|range| range.line),
            Some(2)
        );

        let use_occurrence = analysis
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.line == use_line
                    && occurrence.hover.contains(&format!("local {name}: {ty}"))
            })
            .unwrap_or_else(|| panic!("missing tuple-pattern use occurrence for `{name}`"));
        assert_eq!(
            use_occurrence.definition.as_ref().map(|range| range.line),
            Some(2)
        );
    }
}

#[test]
fn analysis_records_enum_occurrences_nested_inside_tuple_patterns() {
    let source = [
        "enum Status:",
        "    Ready(int32)",
        "    Waiting",
        "",
        "def inspect(entry: (Status, bool)):",
        "    match entry:",
        "        case (Status.Ready(code), true):",
        "            print(code)",
        "        case _:",
        "            pass",
    ]
    .join("\n");

    let analysis = analyze_source(&source);

    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 6
            && occurrence.hover.contains("variant Ready")
            && occurrence.definition.as_ref().map(|range| range.line) == Some(1)
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 6
            && occurrence.hover.contains("enum Status")
            && occurrence.definition.as_ref().map(|range| range.line) == Some(0)
    }));
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 7
            && occurrence.hover.contains("local code: int32")
            && occurrence.definition.as_ref().map(|range| range.line) == Some(6)
    }));
}

#[test]
fn analysis_tracks_annotated_tuple_destructuring_index_types_and_completion_scope() {
    let source = [
        "def make() -> (int32, str):",
        "    return (1, \"one\")",
        "",
        "def inspect():",
        "    pair: (int32, str) = make()",
        "    left, text = pair",
        "    coords: (int32, int32) = (2, 3)",
        "    chosen = coords[1]",
        "    inferred = (4, \"four\")",
        "    print(left)",
        "    print(text.len())",
        "    print(chosen)",
        "    print(inferred)",
    ]
    .join("\n");

    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    for (name, ty, use_line) in [
        ("left", "int32", 9),
        ("text", "str", 10),
        ("chosen", "int32", 11),
        ("inferred", "(int64, str)", 12),
    ] {
        let occurrence = analysis
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.line == use_line
                    && occurrence.hover.contains(&format!("binding {name}: {ty}"))
            })
            .unwrap_or_else(|| panic!("missing typed occurrence for `{name}`"));
        assert_eq!(
            occurrence.definition.as_ref().map(|range| range.line),
            Some(match name {
                "chosen" => 7,
                "inferred" => 8,
                _ => 5,
            })
        );
    }

    let member_line = source
        .lines()
        .position(|line| line.contains("text.len"))
        .expect("source should contain str member access");
    let character = source
        .lines()
        .nth(member_line)
        .and_then(|line| line.find('.'))
        .map(|index| index + 1)
        .expect("source should contain a member dot");
    let completions = complete_source(&source, member_line, character, Some('.'))
        .expect("tuple-destructured str completion should succeed");
    assert!(
        completions
            .iter()
            .any(|completion| completion.name == "len"),
        "tuple destructuring must place element types in the completion scope"
    );

    let tuple_type = Type::Tuple(vec![Type::named("int32"), Type::named("str")]);
    assert_eq!(base_type_name(&tuple_type), "tuple");
    assert_eq!(
        tuple_type.type_arguments(),
        &[Type::named("int32"), Type::named("str")]
    );
}

#[test]
fn analysis_exposes_structural_tuple_equality_without_consuming_operands() {
    let source = [
        "def inspect():",
        "    left = (\"left\", 1)",
        "    right = (\"right\", 2)",
        "    equal = left == right",
        "    not_equal = left != right",
        "    typed: (Option[int32], float32) = (Option.Some(1), 1.5)",
        "    literal_on_right = typed == (None, 2.5)",
        "    literal_on_left = (None, 3.5) != typed",
        "    print(left[1])",
        "    print(right[1])",
    ]
    .join("\n");

    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    for (name, line) in [
        ("equal", 3),
        ("not_equal", 4),
        ("literal_on_right", 6),
        ("literal_on_left", 7),
    ] {
        assert!(
            analysis.occurrences.iter().any(|occurrence| {
                occurrence.line == line
                    && occurrence.hover.contains(&format!("binding {name}: bool"))
            }),
            "missing bool result hover for `{name}` in {:?}",
            analysis.occurrences
        );
    }

    for (name, definition_line, definition_end, use_lines) in
        [("left", 1, 8, [3, 4, 8]), ("right", 2, 9, [3, 4, 9])]
    {
        for use_line in use_lines {
            let occurrence = analysis
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.line == use_line
                        && occurrence
                            .hover
                            .contains(&format!("binding {name}: (str, int64)"))
                })
                .unwrap_or_else(|| {
                    panic!("missing reusable tuple occurrence for `{name}` on line {use_line}")
                });
            assert_eq!(
                occurrence.definition.as_ref().map(|range| (
                    range.line,
                    range.start_character,
                    range.end_character
                )),
                Some((definition_line, 4, definition_end))
            );
        }
    }
}

#[test]
fn analysis_maps_tuple_ordering_diagnostic() {
    let source = [
        "def compare(left: (str, int64), right: (str, int64)):",
        "    ordered = left < right",
    ]
    .join("\n");

    let analysis = analyze_source(&source);
    assert_eq!(analysis.diagnostics.len(), 1, "{:?}", analysis.diagnostics);
    let diagnostic = &analysis.diagnostics[0];
    assert_eq!(diagnostic.code, "AU2003");
    assert_eq!(
        diagnostic.message,
        "tuple ordering is not supported; use `==` or `!=`, or compare tuple elements explicitly"
    );
    assert_eq!(
        (
            diagnostic.line,
            diagnostic.start_character,
            diagnostic.end_character,
        ),
        (1, 14, 15)
    );
}

#[test]
fn analysis_preserves_tuple_recovery_for_invalid_patterns_and_grouped_indices() {
    let pattern_source = [
        "def inspect(flag: bool):",
        "    match flag:",
        "        case (_, true):",
        "            pass",
    ]
    .join("\n");
    let pattern_analysis = analyze_source(&pattern_source);
    assert!(
        pattern_analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("tuple pattern requires a tuple scrutinee")
        }),
        "{:?}",
        pattern_analysis.diagnostics
    );

    let index_source = [
        "def index():",
        "    pair = (1, 2)",
        "    offset = 0",
        "    print(pair[(offset)])",
    ]
    .join("\n");
    let index_analysis = analyze_source(&index_source);
    assert!(
        index_analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("tuple indices must be non-negative integer literals")
        }),
        "{:?}",
        index_analysis.diagnostics
    );
}

#[test]
fn analysis_member_assignment_without_source_field_range_does_not_emit_occurrence() {
    let assignment_source = [
        "class Counter:",
        "    value: int32",
        "",
        "def update():",
        "    mut counter = Counter(value=0)",
        "    counter.value = 1",
    ]
    .join("\n");
    let assignment_analysis = analyze_source(&assignment_source);
    assert!(assignment_analysis.diagnostics.is_empty());
    assert!(assignment_analysis.occurrences.iter().any(|occurrence| {
        occurrence.line == 5
            && occurrence.hover.contains("field value")
            && occurrence.definition.is_some()
    }));

    let source = [
        "class Counter:",
        "    value: int32",
        "",
        "def update(counter: Counter):",
        "    pass",
    ]
    .join("\n");
    let program = checked_program(&source);
    let mut builder = AnalysisBuilder::new(&source, &program, Vec::new());
    let function_decl = program
        .module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function_decl) if function_decl.name == "update" => Some(function_decl),
            _ => None,
        })
        .expect("update function should exist");
    let function_info = program
        .functions
        .get("update")
        .expect("update function info should exist");
    let mut scope = builder.function_scope(function_decl, function_info);
    let assignment = AssignStmt {
        mutable: false,
        target: AssignTarget::Member {
            object: Box::new(expr(ExprKind::Name("counter".to_string()))),
            field: "value".to_string(),
        },
        annotation: None,
        op: None,
        value: expr(ExprKind::Int(1)),
        span: Span::new(5, 5),
    };

    builder.visit_assign(&assignment, &mut scope);

    assert!(builder
        .output
        .occurrences
        .iter()
        .all(|occurrence| !occurrence.hover.contains("field value")));

    let unresolved_receiver_assignment = AssignStmt {
        mutable: false,
        target: AssignTarget::Member {
            object: Box::new(expr(ExprKind::Name("missing".to_string()))),
            field: "value".to_string(),
        },
        annotation: None,
        op: None,
        value: expr(ExprKind::Int(2)),
        span: Span::new(5, 5),
    };
    builder.visit_assign(&unresolved_receiver_assignment, &mut scope);

    assert!(builder
        .output
        .occurrences
        .iter()
        .all(|occurrence| !occurrence.hover.contains("missing")));
}

#[test]
fn analysis_helper_functions_cover_formatting_ranges_and_builtin_surface() {
    let diagnostic = analysis_diagnostic(&Diagnostic::at(Span::new(3, 5), "problem"));
    assert_eq!(diagnostic.line, 2);
    assert_eq!(diagnostic.start_character, 4);
    assert_eq!(diagnostic.end_character, 5);

    assert_eq!(
        range_from_span(Span::new(4, 2), 3),
        super::AnalysisRange {
            file_path: None,
            line: 3,
            start_character: 1,
            end_character: 4,
        }
    );
    assert_eq!(
        range_from_span_with_path(Span::new(2, 4), 2, Some("/tmp/example.au".to_string())),
        super::AnalysisRange {
            file_path: Some("/tmp/example.au".to_string()),
            line: 1,
            start_character: 3,
            end_character: 5,
        }
    );
    assert_eq!(
        find_identifier_in_line("value total value2", "value"),
        Some((0, 5))
    );
    assert_eq!(find_identifier_in_line("value2", "value"), None);
    assert_eq!(find_identifier_in_line("prefixvalue suffix", "value"), None);
    assert_eq!(
        find_identifier_in_line("prefix_value value", "value"),
        Some((13, 18))
    );

    assert_eq!(lower_type_ref(&type_ref("None")), Type::Unit);
    assert_eq!(lower_type_ref(&type_ref("str")), Type::named("str"));
    assert_eq!(
        base_type_name(&Type::Module("pkg.types".to_string())),
        "pkg.types"
    );
    assert!(format_value_hover("let", "count", &Type::named("int32")).contains("count: int32"));
    assert!(format_function_hover(&function_decl("total", "int32")).contains("function total"));
    assert!(format_method_hover(&function_decl("name", "str")).contains("method name"));

    let class_info = ClassInfo {
        module_name: "<test>".to_string(),
        is_builtin: false,
        decl: ClassDecl {
            public: true,
            copy: false,
            name: "Counter".to_string(),
            type_params: Vec::new(),
            type_param_bounds: Default::default(),
            fields: Vec::new(),
            methods: Vec::new(),
            span: Span::new(1, 1),
        },
        type_param_bounds: Default::default(),
        fields: std::collections::BTreeMap::from([(
            "value".to_string(),
            FieldInfo {
                public: true,
                ty: Type::named("int32"),
                span: Span::new(1, 1),
            },
        )]),
        methods: Default::default(),
    };
    let enum_info = EnumInfo {
        module_name: "<test>".to_string(),
        decl: crate::ast::EnumDecl {
            public: true,
            name: "Status".to_string(),
            type_params: Vec::new(),
            type_param_bounds: Default::default(),
            variants: Vec::new(),
            span: Span::new(1, 1),
        },
        type_param_bounds: Default::default(),
        variants: std::collections::BTreeMap::from([(
            "Ready".to_string(),
            EnumVariantInfo {
                payloads: Vec::new(),
                named_payloads: false,
                span: Span::new(1, 1),
            },
        )]),
    };
    assert!(format_class_hover(&class_info).contains("value: int32"));
    assert!(format_enum_hover_named(&enum_info.decl.name).contains("enum Status"));
    assert!(builtin_enum_hover("Option[T]", "docs").contains("docs"));
    assert!(builtin_function_hover("print(value)", "docs").contains("print(value)"));
    assert!(
        format_variant_hover("Option", "Some", Some(&Type::named("str")))
            .contains("variant Some(own str) -> Option")
    );

    let option_variants = builtin_enum_variant_completions("Option");
    assert!(option_variants.iter().any(|item| item.name == "Some"));
    assert!(builtin_enum_variant_completions("Result")
        .iter()
        .any(|item| item.name == "Err"));
    assert!(builtin_enum_variant_completions("SendError")
        .iter()
        .any(|item| item.name == "Full"));
    assert!(builtin_enum_variant_completions("QueueReceive")
        .iter()
        .any(|item| item.name == "Item"));
    assert!(builtin_enum_variant_completions("TaskResult")
        .iter()
        .any(|item| item.name == "Ready"));
    assert!(builtin_enum_variant_completions("WaitAny")
        .iter()
        .any(|item| item.name == "Error"));
    assert!(builtin_enum_variant_completions("WaitAll")
        .iter()
        .any(|item| item.name == "Cancelled"));
    assert!(builtin_enum_variant_completions("Unknown").is_empty());
    assert!(builtin_member_completions(&Type::Named(
        "set".to_string(),
        vec![Type::named("int32")],
    ))
    .iter()
    .any(|item| item.name == "add"));
    assert!(builtin_member_completions(&Type::Named(
        "Queue".to_string(),
        vec![Type::named("int32")],
    ))
    .iter()
    .any(|item| item.name == "put"));
    assert!(builtin_member_completions(&Type::Named(
        "Queue".to_string(),
        vec![Type::named("int32")],
    ))
    .iter()
    .any(|item| item.name == "get"));
    assert!(builtin_member_completions(&Type::Named(
        "Task".to_string(),
        vec![Type::named("int32")],
    ))
    .iter()
    .any(|item| item.name == "result"));
    assert!(
        builtin_member_completions(&Type::Named("TaskGroup".to_string(), Vec::new(),))
            .iter()
            .any(|item| item.name == "start")
    );
    assert_eq!(
        builtin_function_return_type("parse_float64"),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::named("float64"), Type::named("str")],
        ))
    );
    assert_eq!(builtin_function_return_type("min"), None);
    assert_eq!(builtin_function_return_type("queue"), None);
    assert_eq!(builtin_function_return_type("TaskGroup"), None);
    assert_eq!(
        format_function_detail(&function_decl("render", "bool")),
        "render(self, value: int32) -> bool"
    );
}

#[test]
fn builtin_variant_inference_helpers_cover_builtin_constructors_and_unknowns() {
    let int_arg = [arg(Expr {
        kind: ExprKind::Int(7),
        span: Span::new(1, 1),
    })];
    let string_arg = [arg(Expr {
        kind: ExprKind::String("oops".to_string()),
        span: Span::new(1, 1),
    })];

    assert_eq!(
        infer_builtin_variant_call("Option", "Some", &int_arg, |_| Some(Type::named("int32"))),
        Some(Type::Named(
            "Option".to_string(),
            vec![Type::named("int32")]
        ))
    );
    assert_eq!(
        infer_builtin_variant_call("Option", "None", &[], |_| None),
        Some(Type::Named("Option".to_string(), vec![Type::Unit]))
    );
    assert_eq!(
        infer_builtin_variant_call("Result", "Ok", &int_arg, |_| Some(Type::named("int32"))),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::named("int32"), Type::Unit],
        ))
    );
    assert_eq!(
        infer_builtin_variant_call("Result", "Err", &string_arg, |_| Some(Type::named("str"))),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::Unit, Type::named("str")],
        ))
    );
    assert_eq!(
        infer_builtin_variant_call("SendError", "Closed", &int_arg, |_| Some(Type::named(
            "int32"
        ))),
        Some(Type::Named(
            "SendError".to_string(),
            vec![Type::named("int32")],
        ))
    );
    assert_eq!(
        infer_builtin_variant_call("SendError", "Cancelled", &int_arg, |_| Some(Type::named(
            "int32"
        ))),
        Some(Type::Named(
            "SendError".to_string(),
            vec![Type::named("int32")],
        ))
    );
    assert_eq!(
        infer_builtin_variant_call("Option", "Missing", &[], |_| None),
        None
    );
}

#[test]
fn analysis_recovery_helpers_cover_placeholders_and_receiver_extraction() {
    let source = [
        "def main() -> int32:",
        "    counter = Counter(value=1)",
        "    counter.",
        "    return 0",
    ]
    .join("\n");
    assert_eq!(
        sanitize_member_completion_source(&source, 2, 12),
        [
            "def main() -> int32:",
            "    counter = Counter(value=1)",
            "    counter",
            "    return 0",
        ]
        .join("\n")
    );
    assert_eq!(
        replace_dangling_member_stmt_with_recovery_stmt(&source, 2),
        [
            "def main() -> int32:",
            "    counter = Counter(value=1)",
            "    return 0",
            "    return 0",
        ]
        .join("\n")
    );
    assert_eq!(
        replace_dangling_member_stmt_with_recovery_stmt("value.", 0),
        "pass"
    );
    assert_eq!(
        enclosing_function_return_placeholder(&source, 2),
        Some("return 0".to_string())
    );
    assert_eq!(
        enclosing_function_return_placeholder(
            "def main() -> bool:\n    if true:\n        value.",
            2
        ),
        Some("return false".to_string())
    );
    assert_eq!(
        placeholder_stmt_for_return_type("Option[str]"),
        Some("return Option.None".to_string())
    );
    assert_eq!(
        placeholder_stmt_for_return_type("str"),
        Some("return \"\"".to_string())
    );
    assert_eq!(placeholder_stmt_for_return_type("Counter"), None);

    let line = "    values[idx].clone().";
    assert_eq!(
        extract_receiver_before_dot(line, line.len()),
        Some("values[idx].clone()".to_string())
    );
    assert_eq!(
        extract_receiver_ending_before(line, line.len()),
        Some("values[idx].clone()")
    );
    let field_line = "    value.";
    assert_eq!(
        extract_receiver_before_dot(field_line, field_line.len()),
        Some("value".to_string())
    );
    let spaced_field_line = "    value   .   ";
    assert_eq!(
        extract_receiver_before_dot(spaced_field_line, spaced_field_line.len()),
        Some("value".to_string())
    );
    assert_eq!(extract_receiver_before_dot("      .   ", 10), None);
    assert_eq!(find_receiver_start("value.clone()", 10), Some(0));
    assert_eq!(find_receiver_start("(value.clone())", 13), Some(1));
    assert_eq!(find_receiver_start("(value.clone())", 14), Some(0));
    assert_eq!(
        extract_receiver_before_dot("    values[start:end].", 23),
        Some("values[start:end]".to_string())
    );
    assert_eq!(
        extract_receiver_before_dot("    values[:][index].", 22),
        Some("values[:][index]".to_string())
    );

    let stmts = vec![
        crate::ast::Stmt::Pass(PassStmt {
            span: Span::new(2, 5),
        }),
        crate::ast::Stmt::Return(ReturnStmt {
            value: Some(Expr {
                kind: ExprKind::Int(1),
                span: Span::new(4, 5),
            }),
            view: None,
            span: Span::new(4, 5),
        }),
    ];
    assert!(callable_contains_line(&stmts, 3));
    assert!(block_contains_line(&stmts, 4));
    assert_eq!(stmt_start_line(&stmts[0]), 2);
    assert_eq!(stmt_end_line(&stmts[1]), 4);
}

#[test]
fn analysis_builtin_completion_and_statement_helpers_cover_remaining_branches() {
    let vec_completions =
        builtin_member_completions(&Type::Named("list".to_string(), vec![Type::named("int32")]));
    assert!(vec_completions.iter().any(|item| item.name == "append"));
    assert!(vec_completions.iter().any(|item| item.name == "reverse"));
    for (name, detail) in [
        (
            "sort",
            "sort(key: def(T) -> K = ..., reverse: bool = false) -> None",
        ),
        ("map", "map(f: def(T) -> U) -> list[U]"),
        ("filter", "filter(f: def(T) -> bool) -> list[T]"),
    ] {
        let completion = vec_completions
            .iter()
            .find(|item| item.name == name)
            .unwrap_or_else(|| panic!("list.{name} completion should exist"));
        assert_eq!(completion.detail, detail);
    }

    let queue_completions = builtin_member_completions(&Type::Named(
        "Queue".to_string(),
        vec![Type::named("int32")],
    ));
    assert!(queue_completions.iter().any(|item| item.name == "put"));
    assert!(queue_completions.iter().any(|item| item.name == "get"));
    let task_completions =
        builtin_member_completions(&Type::Named("Task".to_string(), vec![Type::named("int32")]));
    assert!(task_completions.iter().any(|item| item.name == "result"));

    let program = checked_program("def main():\n    pass\n");
    let builder = AnalysisBuilder::new("", &program, Vec::new());
    let method_decl = function_decl("tick", "None");
    let method_info = MethodInfo {
        decl: method_decl.clone(),
        signature: FunctionSignature {
            params: vec![Type::named("int32")],
            param_passings: vec![ReceiverKind::Value],
            return_type: Type::Unit,
            rng_clone_safe_type_params: Default::default(),
            array_equality_safe_type_params: Default::default(),
        },
        type_param_bounds: Default::default(),
    };
    let method_scope = builder.method_scope("Counter", &method_decl, &method_info);
    assert_eq!(
        method_scope
            .get("self")
            .expect("method scope should include self")
            .definition,
        range_from_span(method_decl.span, method_decl.name.len())
    );
    let vec_receiver = Type::Named("list".to_string(), vec![Type::named("int32")]);
    assert_eq!(
        builder
            .resolve_member_type(&vec_receiver, "copy")
            .expect("list.copy should resolve")
            .ty,
        Some(vec_receiver.clone())
    );
    for (name, signature) in [
        (
            "sort",
            "sort(key: def(T) -> K = ..., reverse: bool = false) -> None",
        ),
        ("map", "map(f: def(T) -> U) -> list[U]"),
        ("filter", "filter(f: def(T) -> bool) -> list[T]"),
    ] {
        let member = builder
            .resolve_member_type(&vec_receiver, name)
            .unwrap_or_else(|| panic!("list.{name} should resolve for hover"));
        assert!(
            member.hover.contains(signature),
            "list.{name} hover should contain `{signature}`, got `{}`",
            member.hover
        );
    }
    let map_receiver = Type::Named(
        "dict".to_string(),
        vec![Type::named("str"), Type::named("int32")],
    );
    assert_eq!(
        builder
            .resolve_member_type(&map_receiver, "copy")
            .expect("dict.copy should resolve")
            .ty,
        Some(map_receiver.clone())
    );
    let set_receiver = Type::Named("set".to_string(), vec![Type::named("str")]);
    assert_eq!(
        builder
            .resolve_member_type(&set_receiver, "copy")
            .expect("set.copy should resolve")
            .ty,
        Some(set_receiver.clone())
    );

    assert_eq!(
        builtin_function_return_type("range"),
        Some(Type::named("Range"))
    );
    assert_eq!(builtin_function_return_type("print"), Some(Type::Unit));
    assert_eq!(builtin_function_return_type("TaskGroup"), None);
    assert_eq!(
        builtin_function_return_type("cancelled"),
        Some(Type::named("bool"))
    );
    assert_eq!(builtin_function_return_type("after"), None);
    assert_eq!(builtin_function_return_type("wait_any"), None);
    assert_eq!(builtin_function_return_type("wait_all"), None);
    assert_eq!(builtin_function_return_type("abs"), None);
    assert_eq!(builtin_function_return_type("min"), None);
    assert_eq!(builtin_function_return_type("max"), None);
    assert_eq!(builtin_function_return_type("sqrt"), None);
    assert_eq!(builtin_function_return_type("sleep"), Some(Type::Unit));
    assert_eq!(builtin_function_return_type("yield_now"), Some(Type::Unit));
    assert_eq!(
        builtin_function_return_type("parse_int32"),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::named("int32"), Type::named("str")],
        ))
    );
    assert_eq!(
        builtin_function_return_type("parse_int64"),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::named("int64"), Type::named("str")],
        ))
    );
    assert_eq!(
        builtin_function_return_type("parse_float64"),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::named("float64"), Type::named("str")],
        ))
    );

    let if_stmt = crate::ast::Stmt::If(crate::ast::IfStmt {
        branches: vec![crate::ast::IfBranch {
            condition: Expr {
                kind: ExprKind::Bool(true),
                span: Span::new(2, 8),
            },
            body: vec![crate::ast::Stmt::Pass(PassStmt {
                span: Span::new(3, 9),
            })],
            span: Span::new(2, 5),
        }],
        else_body: Some(vec![crate::ast::Stmt::Return(ReturnStmt {
            value: None,
            view: None,
            span: Span::new(5, 5),
        })]),
        span: Span::new(2, 5),
    });
    let match_stmt = crate::ast::Stmt::Match(crate::ast::MatchStmt {
        scrutinee: Expr {
            kind: ExprKind::Name("status".to_string()),
            span: Span::new(6, 11),
        },
        capability: ReceiverKind::Borrow,
        arms: vec![crate::ast::MatchArm {
            guard: None,
            pattern: crate::ast::Pattern::Wildcard(Span::new(7, 9)),
            body: vec![crate::ast::Stmt::Pass(PassStmt {
                span: Span::new(8, 9),
            })],
            span: Span::new(7, 9),
        }],
        span: Span::new(6, 5),
    });
    let for_stmt = crate::ast::Stmt::For(crate::ast::ForStmt {
        target: crate::ast::BindingTarget::Name {
            name: "value".to_string(),
            span: Span::new(9, 5),
        },
        iterable: Expr {
            kind: ExprKind::Name("values".to_string()),
            span: Span::new(9, 14),
        },
        borrow_mode: None,
        body: vec![crate::ast::Stmt::Pass(PassStmt {
            span: Span::new(10, 9),
        })],
        span: Span::new(9, 5),
    });
    let with_stmt = crate::ast::Stmt::With(crate::ast::WithStmt {
        binding: "resource".to_string(),
        value: Expr {
            kind: ExprKind::Name("resource".to_string()),
            span: Span::new(11, 10),
        },
        body: vec![crate::ast::Stmt::Pass(PassStmt {
            span: Span::new(12, 9),
        })],
        span: Span::new(11, 5),
    });
    let helper_with_stmt = crate::ast::Stmt::With(crate::ast::WithStmt {
        binding: "group".to_string(),
        value: Expr {
            kind: ExprKind::Name("group".to_string()),
            span: Span::new(13, 10),
        },
        body: vec![crate::ast::Stmt::Pass(PassStmt {
            span: Span::new(14, 9),
        })],
        span: Span::new(13, 5),
    });
    let while_stmt = crate::ast::Stmt::While(crate::ast::WhileStmt {
        condition: Expr {
            kind: ExprKind::Bool(true),
            span: Span::new(15, 11),
        },
        body: vec![crate::ast::Stmt::Pass(PassStmt {
            span: Span::new(16, 9),
        })],
        span: Span::new(15, 5),
    });
    let stmts = vec![
        if_stmt,
        match_stmt,
        for_stmt,
        with_stmt,
        helper_with_stmt,
        while_stmt,
    ];
    assert_eq!(stmt_end_line(&stmts[0]), 5);
    let empty_else_stmt = crate::ast::Stmt::If(crate::ast::IfStmt {
        branches: vec![crate::ast::IfBranch {
            condition: Expr {
                kind: ExprKind::Bool(true),
                span: Span::new(20, 8),
            },
            body: vec![crate::ast::Stmt::Pass(PassStmt {
                span: Span::new(21, 9),
            })],
            span: Span::new(20, 5),
        }],
        else_body: Some(Vec::new()),
        span: Span::new(20, 5),
    });
    assert_eq!(stmt_end_line(&empty_else_stmt), 21);
    let no_else_stmt = crate::ast::Stmt::If(crate::ast::IfStmt {
        branches: vec![crate::ast::IfBranch {
            condition: Expr {
                kind: ExprKind::Bool(false),
                span: Span::new(22, 8),
            },
            body: vec![crate::ast::Stmt::Pass(PassStmt {
                span: Span::new(23, 9),
            })],
            span: Span::new(22, 5),
        }],
        else_body: None,
        span: Span::new(22, 5),
    });
    assert_eq!(stmt_end_line(&no_else_stmt), 23);
    assert_eq!(stmt_end_line(&stmts[1]), 8);
    assert_eq!(stmt_end_line(&stmts[2]), 10);
    assert_eq!(stmt_end_line(&stmts[3]), 12);
    assert_eq!(stmt_end_line(&stmts[4]), 14);
    assert_eq!(stmt_end_line(&stmts[5]), 16);
    assert!(!block_contains_line(&[], 1));
    let break_stmt = crate::ast::Stmt::Break(crate::ast::BreakStmt {
        span: Span::new(17, 9),
    });
    let continue_stmt = crate::ast::Stmt::Continue(crate::ast::ContinueStmt {
        span: Span::new(18, 9),
    });
    let expr_stmt = crate::ast::Stmt::Expr(crate::ast::ExprStmt {
        expr: expr(ExprKind::Int(1)),
        span: Span::new(19, 9),
    });
    assert_eq!(stmt_start_line(&break_stmt), 17);
    assert_eq!(stmt_start_line(&continue_stmt), 18);
    assert_eq!(stmt_start_line(&expr_stmt), 19);
    assert_eq!(stmt_end_line(&break_stmt), 17);
    assert_eq!(stmt_end_line(&continue_stmt), 18);
    assert_eq!(stmt_end_line(&expr_stmt), 19);
    assert!(callable_contains_line(&stmts, 14));
    assert!(!block_contains_line(&stmts, 20));

    let scope_builder = AnalysisBuilder::new("", &program, Vec::new());
    let mut accumulated_scope = BTreeMap::new();
    scope_builder.accumulate_scope_from_stmts(&stmts[..1], 4, &mut accumulated_scope);
    scope_builder.accumulate_scope_from_stmts(
        std::slice::from_ref(&no_else_stmt),
        24,
        &mut accumulated_scope,
    );
    scope_builder.accumulate_scope_from_stmts(&stmts, 5, &mut accumulated_scope);
    scope_builder.accumulate_scope_from_stmts(&stmts, 6, &mut accumulated_scope);
    scope_builder.accumulate_scope_from_stmts(&stmts, 16, &mut accumulated_scope);

    let mut fallback_builder = AnalysisBuilder::new("", &program, Vec::new());
    let mut fallback_scope = BTreeMap::new();
    let fallback_assignment = AssignStmt {
        mutable: false,
        target: AssignTarget::Name("fresh".to_string()),
        annotation: Some(type_ref("int32")),
        op: None,
        value: expr(ExprKind::Int(1)),
        span: Span::new(30, 1),
    };
    fallback_builder.bind_assignment(&fallback_assignment, &mut fallback_scope);
    assert_eq!(
        fallback_scope
            .get("fresh")
            .expect("fresh binding should be inserted")
            .definition,
        range_from_span(Span::new(30, 1), "fresh".len())
    );
    let fallback_view = ViewStmt {
        name: "alias".to_string(),
        mutable: false,
        source: expr(ExprKind::Name("fresh".to_string())),
        span: Span::new(32, 1),
    };
    fallback_builder.bind_view_value(&fallback_view, Type::named("int32"), &mut fallback_scope);
    assert_eq!(
        fallback_scope
            .get("alias")
            .expect("fallback view binding should be inserted")
            .definition,
        fallback_scope
            .get("fresh")
            .expect("view source should remain in scope")
            .definition
    );
    let reassignment = AssignStmt {
        mutable: false,
        target: AssignTarget::Name("fresh".to_string()),
        annotation: None,
        op: None,
        value: expr(ExprKind::Int(2)),
        span: Span::new(31, 1),
    };
    fallback_builder.visit_assign(&reassignment, &mut fallback_scope);
    let reassignment_range = range_from_span(Span::new(31, 1), "fresh".len());
    assert!(fallback_builder
        .output
        .occurrences
        .iter()
        .any(|occurrence| {
            occurrence.hover.contains("fresh: int32")
                && occurrence.line == reassignment_range.line
                && occurrence.start_character == reassignment_range.start_character
                && occurrence.end_character == reassignment_range.end_character
        }));

    let mut scope = BTreeMap::new();
    let no_payload_arm = crate::ast::MatchArm {
        guard: None,
        pattern: crate::ast::Pattern::Variant(VariantPattern {
            enum_name: Some("Option".to_string()),
            variant_name: "None".to_string(),
            subpatterns: Vec::new(),
            span: Span::new(21, 9),
        }),
        body: Vec::new(),
        span: Span::new(21, 9),
    };
    scope_builder.bind_match_arm_scope(
        &no_payload_arm,
        Some(&Type::Named(
            "Option".to_string(),
            vec![Type::named("int32")],
        )),
        &mut scope,
    );
    assert!(scope.is_empty());
    let non_binding_payload_arm = crate::ast::MatchArm {
        guard: None,
        pattern: crate::ast::Pattern::Variant(VariantPattern {
            enum_name: Some("Option".to_string()),
            variant_name: "Some".to_string(),
            subpatterns: vec![crate::ast::Pattern::Wildcard(Span::new(22, 19))],
            span: Span::new(22, 9),
        }),
        body: Vec::new(),
        span: Span::new(22, 9),
    };
    scope_builder.bind_match_arm_scope(
        &non_binding_payload_arm,
        Some(&Type::Named(
            "Option".to_string(),
            vec![Type::named("int32")],
        )),
        &mut scope,
    );
    assert!(scope.is_empty());

    assert_eq!(extract_receiver_ending_before("", 0), None);
    assert_eq!(extract_receiver_ending_before("value", 5), None);
    assert_eq!(extract_receiver_ending_before(".field", 1), None);
    assert_eq!(
        extract_receiver_ending_before("(value + other).field", 16),
        Some("(value + other)")
    );
    assert_eq!(
        extract_receiver_ending_before("value.   ", "value.   ".len()),
        Some("value")
    );
    assert_eq!(
        extract_receiver_ending_before("((value)).field", 10),
        Some("((value))")
    );
    assert_eq!(find_receiver_start("value).field", 5), None);
    assert_eq!(
        sanitize_member_completion_source("def main():\n    value", 20, 0),
        "def main():\n    value"
    );
    assert_eq!(
        sanitize_member_completion_source("def main():\n    value", 1, 0),
        "def main():\n    value"
    );
    assert_eq!(
        sanitize_member_completion_source("def main():\n    value", 1, 10),
        "def main():\n    value"
    );
    assert_eq!(
        replace_dangling_member_stmt_with_recovery_stmt("def main():\n    value.", 20),
        "def main():\n    value."
    );
    assert_eq!(enclosing_function_return_placeholder("value.", 0), None);
    assert_eq!(
        enclosing_function_return_placeholder("def main() -> int32:\n    value.", 10),
        None
    );
    assert_eq!(placeholder_stmt_for_return_type("Custom"), None);
}

#[test]
fn adr0038_analysis_exposes_view_provenance_and_return_contracts() {
    let source = r#"
class User:
    name: str

def name(user: User) -> view str from user:
    return view user.name

def main():
    user = User(name="Ada")
    view direct = (user.name)
    print(direct)
    view display = name(user)
    print(display)
"#;
    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.occurrences.iter().any(|occurrence| {
        occurrence
            .hover
            .contains("function name(user: User) -> view str from user")
    }));
    let display = analysis
        .occurrences
        .iter()
        .find(|occurrence| occurrence.hover.contains("view display: str from <place>"))
        .expect("view hover should expose its kind, pointee type, and returned source");
    assert!(display.definition.is_some());
    assert!(analysis
        .symbols
        .iter()
        .any(|symbol| { symbol.name == "name" && symbol.detail == "view str from user" }));
}

#[test]
fn adr0038_analysis_view_source_helpers_cover_place_shapes_and_recovery() {
    let name = expr(ExprKind::Name("pair".to_string()));
    let member = expr(ExprKind::Member {
        object: Box::new(name.clone()),
        field: "right".to_string(),
    });
    let grouped = expr(ExprKind::Group(Box::new(member.clone())));
    let indexed = expr(ExprKind::Index {
        object: Box::new(grouped.clone()),
        index: Box::new(expr(ExprKind::Int(1))),
    });
    assert_eq!(view_source_root(&indexed), Some("pair"));
    assert_eq!(
        render_view_source(&indexed).as_deref(),
        Some("pair.right[1]")
    );

    let dynamic_index = expr(ExprKind::Index {
        object: Box::new(name),
        index: Box::new(expr(ExprKind::Name("position".to_string()))),
    });
    assert_eq!(render_view_source(&dynamic_index), None);
    let literal = expr(ExprKind::Int(7));
    assert_eq!(view_source_root(&literal), None);
    assert_eq!(render_view_source(&literal), None);
}

#[test]
fn adr0038_completion_scope_retains_view_bindings_and_sources() {
    let source = [
        "def main():",
        "    mut pair = (1, 2)",
        "    view mut selected = pair[0]",
        "    selected",
    ]
    .join("\n");
    let completions = complete_source(&source, 3, 12, None)
        .expect("completion after a view declaration should recover the function scope");
    for name in ["pair", "selected"] {
        assert!(
            completions.iter().any(|completion| completion.name == name),
            "view-aware completion should retain `{name}`: {completions:?}"
        );
    }

    let lambda_source = [
        "def main():",
        "    pair = (1, 2)",
        "    view selected = pair[0]",
        "    callback: def(int64) -> int64 = lambda [selected] value: selected + value",
    ]
    .join("\n");
    let _ = complete_source(
        &lambda_source,
        3,
        lambda_source.lines().nth(3).unwrap().len(),
        None,
    )
    .expect("lambda scope traversal should accept view statements during recovery");
}

#[test]
fn analysis_builtin_member_types_cover_io_network_and_process_surfaces() {
    let program = checked_program("def main():\n    pass\n");
    let builder = AnalysisBuilder::new("", &program, Vec::new());
    let named = |name: &str| Type::Named(name.to_string(), Vec::new());
    let option = |payload: Type| Type::Named("Option".to_string(), vec![payload]);
    let result = |ok: Type, err: Type| Type::Named("Result".to_string(), vec![ok, err]);
    let vec_of = |payload: Type| Type::Named("list".to_string(), vec![payload]);
    let string = Type::named("str");
    let uint8 = Type::named("uint8");
    let io_error = named("io.Error");
    let process_error = named("process.Error");

    let assert_member_type = |receiver: &str, field: &str, expected: Type| {
        let member = builder
            .resolve_member_type(&named(receiver), field)
            .unwrap_or_else(|| panic!("expected builtin member {receiver}.{field}"));
        assert!(
            member.hover.contains(field),
            "hover for {receiver}.{field} should mention the member name"
        );
        assert_eq!(member.ty, Some(expected), "{receiver}.{field}");
    };

    assert_member_type("str", "len", Type::named("int64"));
    assert_member_type("str", "byte_len", Type::named("int64"));
    assert_member_type("process.Child", "stdin", option(named("process.Pipe")));
    assert_member_type("process.Child", "stdout", option(named("process.Pipe")));
    assert_member_type("process.Child", "stderr", option(named("process.Pipe")));
    assert_member_type("process.Child", "wait", named("process.Wait"));
    assert_member_type(
        "process.Child",
        "wait_or_none",
        result(option(named("process.ExitStatus")), process_error.clone()),
    );
    assert_member_type(
        "process.Child",
        "wait_ok",
        result(named("process.ExitStatus"), process_error.clone()),
    );
    assert_member_type(
        "process.Child",
        "kill",
        result(Type::Unit, process_error.clone()),
    );
    assert_member_type(
        "process.Child",
        "terminate",
        result(Type::Unit, process_error.clone()),
    );
    assert_member_type("process.Child", "close", Type::Unit);

    assert_member_type(
        "process.Pipe",
        "read_all",
        result(string.clone(), process_error.clone()),
    );
    assert_member_type(
        "process.Pipe",
        "read_line",
        result(option(string.clone()), process_error.clone()),
    );
    assert_member_type(
        "process.Pipe",
        "read_bytes",
        result(option(vec_of(uint8.clone())), process_error.clone()),
    );
    for field in ["write_all", "write_bytes", "flush"] {
        assert_member_type(
            "process.Pipe",
            field,
            result(Type::Unit, process_error.clone()),
        );
    }
    assert_member_type("process.Pipe", "close", Type::Unit);

    assert_member_type("process.Completed", "status", named("process.ExitStatus"));
    assert_member_type("process.Completed", "success", Type::named("bool"));
    assert_member_type("process.Completed", "stdout", string.clone());
    assert_member_type("process.Completed", "stderr", string.clone());
    assert_member_type("process.Completed", "stdout_bytes", vec_of(uint8.clone()));
    assert_member_type("process.Completed", "stderr_bytes", vec_of(uint8.clone()));
    assert_member_type(
        "process.Completed",
        "check",
        result(Type::Unit, process_error.clone()),
    );

    for field in ["start", "stop"] {
        assert_member_type(
            "process.Supervisor",
            field,
            result(Type::Unit, process_error.clone()),
        );
    }
    assert_member_type(
        "process.Supervisor",
        "wait",
        named("process.SupervisorWait"),
    );
    assert_member_type(
        "process.Supervisor",
        "wait_or_none",
        result(
            option(named("process.SupervisorEvent")),
            process_error.clone(),
        ),
    );
    assert_member_type("process.Supervisor", "is_empty", Type::named("bool"));
    assert_member_type("process.Supervisor", "close", Type::Unit);

    assert_member_type(
        "fs.File",
        "read_all",
        result(string.clone(), io_error.clone()),
    );
    assert_member_type(
        "fs.File",
        "read_bytes",
        result(vec_of(uint8.clone()), io_error.clone()),
    );
    for field in ["write_all", "write_bytes", "flush"] {
        assert_member_type("fs.File", field, result(Type::Unit, io_error.clone()));
    }
    assert_member_type("fs.File", "close", Type::Unit);

    assert_member_type(
        "net.TcpListener",
        "accept",
        result(named("net.TcpStream"), io_error.clone()),
    );
    assert_member_type(
        "net.TcpListener",
        "local_addr",
        result(string.clone(), io_error.clone()),
    );
    assert_member_type("net.TcpListener", "close", Type::Unit);
    for field in ["read_all", "local_addr", "peer_addr"] {
        assert_member_type(
            "net.TcpStream",
            field,
            result(string.clone(), io_error.clone()),
        );
    }
    assert_member_type(
        "net.TcpStream",
        "read_line",
        result(option(string.clone()), io_error.clone()),
    );
    assert_member_type(
        "net.TcpStream",
        "read_bytes",
        result(option(vec_of(uint8.clone())), io_error.clone()),
    );
    assert_member_type(
        "net.TcpStream",
        "read_exact",
        result(vec_of(uint8.clone()), io_error.clone()),
    );
    for field in [
        "write_all",
        "write_bytes",
        "flush",
        "shutdown_read",
        "shutdown_write",
        "shutdown_both",
    ] {
        assert_member_type("net.TcpStream", field, result(Type::Unit, io_error.clone()));
    }
    assert_member_type("net.TcpStream", "close", Type::Unit);

    for field in ["send_text", "send_bytes"] {
        assert_member_type("net.UdpSocket", field, result(Type::Unit, io_error.clone()));
    }
    assert_member_type(
        "net.UdpSocket",
        "recv",
        result(option(vec_of(uint8.clone())), io_error.clone()),
    );
    assert_member_type(
        "net.UdpSocket",
        "recv_from",
        result(option(named("net.UdpDatagram")), io_error.clone()),
    );
    for field in ["local_addr", "peer_addr"] {
        assert_member_type(
            "net.UdpSocket",
            field,
            result(string.clone(), io_error.clone()),
        );
    }
    assert_member_type("net.UdpSocket", "close", Type::Unit);
    assert_member_type("net.UdpDatagram", "address", string.clone());
    assert_member_type("net.UdpDatagram", "bytes", vec_of(uint8.clone()));
    assert_member_type(
        "net.UdpDatagram",
        "text",
        result(string.clone(), io_error.clone()),
    );

    assert_member_type(
        "net.HttpListener",
        "accept",
        result(named("net.HttpExchange"), io_error.clone()),
    );
    assert_member_type(
        "net.HttpListener",
        "local_addr",
        result(string.clone(), io_error.clone()),
    );
    assert_member_type("net.HttpListener", "close", Type::Unit);
    assert_member_type("net.HttpExchange", "method", string.clone());
    assert_member_type("net.HttpExchange", "path", string.clone());
    assert_member_type(
        "net.HttpExchange",
        "headers",
        Type::Named("dict".to_string(), vec![string.clone(), string.clone()]),
    );
    assert_member_type(
        "net.HttpExchange",
        "body_text",
        result(string.clone(), io_error.clone()),
    );
    assert_member_type("net.HttpExchange", "body_bytes", vec_of(uint8.clone()));
    for field in ["respond_text", "respond_bytes"] {
        assert_member_type(
            "net.HttpExchange",
            field,
            result(Type::Unit, io_error.clone()),
        );
    }
    assert_member_type("net.HttpResponse", "status", Type::named("int32"));
    assert_member_type("net.HttpResponse", "reason", string.clone());
    assert_member_type(
        "net.HttpResponse",
        "headers",
        Type::Named("dict".to_string(), vec![string.clone(), string.clone()]),
    );
    assert_member_type(
        "net.HttpResponse",
        "text",
        result(string.clone(), io_error.clone()),
    );
    assert_member_type("net.HttpResponse", "bytes", vec_of(uint8.clone()));

    assert_member_type(
        "net.WebSocketListener",
        "accept",
        result(named("net.WebSocket"), io_error.clone()),
    );
    assert_member_type(
        "net.WebSocketListener",
        "local_addr",
        result(string.clone(), io_error.clone()),
    );
    for field in ["send_text", "send_bytes"] {
        assert_member_type("net.WebSocket", field, result(Type::Unit, io_error.clone()));
    }
    assert_member_type(
        "net.WebSocket",
        "recv_text",
        result(option(string.clone()), io_error.clone()),
    );
    assert_member_type(
        "net.WebSocket",
        "recv_bytes",
        result(option(vec_of(uint8.clone())), io_error.clone()),
    );
    assert_member_type("net.WebSocket", "close", Type::Unit);

    assert_member_type(
        "net.UnixListener",
        "accept",
        result(named("net.UnixStream"), io_error.clone()),
    );
    assert_member_type("net.UnixListener", "close", Type::Unit);
    assert_member_type(
        "net.UnixStream",
        "read_line",
        result(option(string.clone()), io_error.clone()),
    );
    assert_member_type(
        "net.UnixStream",
        "read_exact",
        result(vec_of(uint8.clone()), io_error.clone()),
    );
    assert_member_type(
        "net.UnixStream",
        "write_all",
        result(Type::Unit, io_error.clone()),
    );
    assert_member_type("net.UnixStream", "close", Type::Unit);

    assert_member_type(
        "net.TlsListener",
        "accept",
        result(named("net.TlsStream"), io_error.clone()),
    );
    assert_member_type(
        "net.TlsListener",
        "local_addr",
        result(string.clone(), io_error.clone()),
    );
    assert_member_type("net.TlsListener", "close", Type::Unit);
    assert_member_type(
        "net.TlsStream",
        "read_line",
        result(option(string.clone()), io_error.clone()),
    );
    assert_member_type(
        "net.TlsStream",
        "read_exact",
        result(vec_of(uint8.clone()), io_error.clone()),
    );
    assert_member_type(
        "net.TlsStream",
        "write_all",
        result(Type::Unit, io_error.clone()),
    );
    assert_member_type("net.TlsStream", "close", Type::Unit);
}

#[test]
fn function_value_analysis_preserves_symbol_contract_and_indirect_call_result() {
    let source = [
        "def decorate(prefix: str, value: str = \"world\") -> str:",
        "    return prefix + value",
        "",
        "def main() -> int32:",
        "    selected = decorate",
        "    outcome = selected(prefix=\"hello\")",
        "    print(outcome.len())",
        "    return 0",
    ]
    .join("\n");

    let analysis = analyze_source(&source);
    let serialized = serde_json::to_value(&analysis).expect("analysis should serialize");
    assert_eq!(serialized["diagnostics"], serde_json::json!([]));
    let hovers = serialized["occurrences"]
        .as_array()
        .expect("occurrences should serialize as an array")
        .iter()
        .filter_map(|occurrence| occurrence["hover"].as_str())
        .collect::<Vec<_>>();
    assert!(
        hovers.contains(&"```aura\nbinding selected: def(str, str) -> str\n```"),
        "the inferred function-value binding must expose its callable type: {hovers:?}"
    );
    assert!(
        hovers.contains(&"```aura\nbinding outcome: str\n```"),
        "an indirect function-value call must expose its declared return type: {hovers:?}"
    );

    let program = checked_program(&source);
    let builder = AnalysisBuilder::new(&source, &program, Vec::new());
    let function_type = builder.infer_expr_type(
        &expr(ExprKind::Name("decorate".to_string())),
        &BTreeMap::new(),
    );
    let Some(Type::Function {
        params,
        return_type,
    }) = function_type
    else {
        panic!("a function symbol should infer to a full callable type");
    };
    assert_eq!(*return_type, Type::named("str"));
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].name, "prefix");
    assert!(!params[0].has_default);
    assert_eq!(params[1].name, "value");
    assert!(params[1].has_default);
    assert!(
        params.iter().all(|param| !param.default_erased),
        "direct function symbols must retain their named/default call contract"
    );
}

#[test]
fn nested_written_function_types_drive_completion_scope_and_json_schema() {
    let source = [
        "def passthrough(callback: def(mut str, own str) -> (str, int32)) -> def(mut str, own str) -> (str, int32):",
        "    return callback",
        "",
        "def main() -> int32:",
        "    selected: def(def(mut str, own str) -> (str, int32)) -> def(mut str, own str) -> (str, int32) = passthrough",
        "    print(selected)",
        "    return 0",
    ]
    .join("\n");
    let expected_type =
        "def(def(mut str, own str) -> (str, int32)) -> def(mut str, own str) -> (str, int32)";

    let analysis = analyze_source(&source);
    let analysis_json = serde_json::to_value(&analysis).expect("analysis should serialize");
    assert_eq!(analysis_json["diagnostics"], serde_json::json!([]));
    assert!(
        analysis_json["occurrences"]
            .as_array()
            .expect("occurrences should serialize as an array")
            .iter()
            .any(|occurrence| {
                occurrence["hover"]
                    == serde_json::json!(format!("```aura\nbinding selected: {expected_type}\n```"))
            }),
        "the semantic JSON hover must preserve the nested function/tuple signature"
    );

    let program = checked_program(&source);
    let builder = AnalysisBuilder::new(&source, &program, Vec::new());
    let scope = builder.scope_for_line(5);
    let selected_type = &scope
        .get("selected")
        .expect("the written binding should be in completion scope")
        .ty;
    assert_eq!(selected_type.to_string(), expected_type);

    let type_json =
        serde_json::to_value(selected_type).expect("function type schema should serialize");
    let outer_param = &type_json["Function"]["params"][0];
    assert_eq!(outer_param["name"], serde_json::json!(""));
    assert_eq!(outer_param["passing"], serde_json::json!("Borrow"));
    assert_eq!(outer_param["has_default"], serde_json::json!(false));
    assert_eq!(outer_param["default_erased"], serde_json::json!(true));
    let nested_params = &outer_param["ty"]["Function"]["params"];
    assert_eq!(nested_params[0]["passing"], serde_json::json!("BorrowMut"));
    assert_eq!(nested_params[1]["passing"], serde_json::json!("Value"));
    assert_eq!(
        outer_param["ty"]["Function"]["return_type"]["Tuple"][0],
        serde_json::json!({"Named": ["str", []]})
    );
    assert_eq!(
        type_json["Function"]["return_type"]["Function"]["return_type"]["Tuple"][1],
        serde_json::json!({"Named": ["int32", []]})
    );
}

#[test]
fn written_function_type_aliases_are_canonical_in_completion_scope() {
    let source = [
        "def report(label: str, count: int) -> None:",
        "    pass",
        "",
        "def main() -> int32:",
        "    selected: def(str, int) -> None = report",
        "    print(selected)",
        "    return 0",
    ]
    .join("\n");

    let analysis = analyze_source(&source);
    let analysis_json = serde_json::to_value(&analysis).expect("analysis should serialize");
    assert_eq!(analysis_json["diagnostics"], serde_json::json!([]));
    assert!(
        analysis_json["occurrences"]
            .as_array()
            .expect("occurrences should serialize as an array")
            .iter()
            .any(|occurrence| {
                occurrence["hover"]
                    == serde_json::json!("```aura\nbinding selected: def(str, int64) -> None\n```")
            }),
        "written callable aliases must be canonicalized in semantic JSON"
    );

    let program = checked_program(&source);
    let builder = AnalysisBuilder::new(&source, &program, Vec::new());
    let scope = builder.scope_for_line(5);
    assert_eq!(
        scope
            .get("selected")
            .expect("the written function binding should enter completion scope")
            .ty
            .to_string(),
        "def(str, int64) -> None"
    );
}

#[test]
fn lambda_analysis_resolves_parameters_captures_and_closure_bindings() {
    let source = [
        "def main():",
        "    offset: int32 = 40",
        "    add: def(int32) -> int32 = lambda value: value + offset",
        "    print(add(2))",
    ]
    .join("\n");

    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "valid lambda analysis should not report diagnostics: {:?}",
        analysis.diagnostics
    );

    let parameter_use = source.lines().nth(2).unwrap().rfind("value").unwrap();
    let parameter = analysis
        .occurrences
        .iter()
        .find(|occurrence| occurrence.line == 2 && occurrence.start_character == parameter_use)
        .expect("the lambda parameter use should be analyzed");
    assert!(parameter.hover.contains("param value: int32"));
    let parameter_declaration = source.lines().nth(2).unwrap().find("value").unwrap();
    assert_eq!(
        parameter.definition.as_ref().map(|range| (
            range.line,
            range.start_character,
            range.end_character
        )),
        Some((
            2,
            parameter_declaration,
            parameter_declaration + "value".len()
        ))
    );

    let capture_use = source.lines().nth(2).unwrap().rfind("offset").unwrap();
    let capture = analysis
        .occurrences
        .iter()
        .find(|occurrence| occurrence.line == 2 && occurrence.start_character == capture_use)
        .expect("the captured outer binding should be analyzed");
    assert!(capture.hover.contains("binding offset: int32"));
    assert_eq!(
        capture.definition.as_ref().map(|range| (
            range.line,
            range.start_character,
            range.end_character
        )),
        Some((1, 4, 10))
    );

    assert!(
        analysis.occurrences.iter().any(|occurrence| {
            occurrence.line == 3
                && occurrence
                    .hover
                    .contains("binding add: closure def(int32) -> int32")
        }),
        "capturing lambda bindings should retain their closure type"
    );

    let completions =
        complete_source(&source, 2, source.lines().nth(2).unwrap().len(), None).unwrap();
    assert!(completions.iter().any(|item| item.name == "value"));
    assert!(completions.iter().any(|item| item.name == "offset"));
    assert!(completions.iter().any(|item| item.name == "lambda"));
}

#[test]
fn lambda_analysis_displays_consuming_closure_bindings() {
    let source = [
        "def main():",
        "    text = \"captured\"",
        "    take: def() -> str = lambda: text",
        "    print(take())",
    ]
    .join("\n");

    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "single-use closure analysis should succeed: {:?}",
        analysis.diagnostics
    );
    assert!(
        analysis.occurrences.iter().any(|occurrence| {
            occurrence.line == 3
                && occurrence
                    .hover
                    .contains("binding take: consuming closure def() -> str")
        }),
        "a moved non-Copy capture should display its consuming call contract"
    );
}

#[test]
fn lambda_scope_navigation_follows_every_expression_container_to_its_structural_end() {
    let source = [
        "def main():",
        "    callback: def(int64) -> int64 = lambda value: value",
    ]
    .join("\n");
    let program = checked_program(&source);
    let Item::Function(main) = &program.module.items[0] else {
        panic!("expected main function");
    };
    let crate::ast::Stmt::Assign(assign) = &main.body[0] else {
        panic!("expected callback assignment");
    };
    let mut lambda = assign.value.clone();
    let ExprKind::Lambda { body, .. } = &mut lambda.kind else {
        panic!("expected checked lambda expression");
    };
    body.span = Span::new(9, 1);

    let leaf = |line| Expr {
        kind: ExprKind::Name("outside".to_string()),
        span: Span::new(line, 1),
    };
    let wrapper_span = Span::new(1, 1);
    let wrappers = vec![
        ("lambda", lambda.clone()),
        (
            "membership",
            Expr {
                kind: ExprKind::Membership {
                    value: Box::new(leaf(4)),
                    container: Box::new(lambda.clone()),
                    negated: false,
                    operator_span: wrapper_span,
                },
                span: wrapper_span,
            },
        ),
        (
            "comparison chain",
            Expr {
                kind: ExprKind::CompareChain {
                    first: Box::new(leaf(4)),
                    links: vec![crate::ast::CompareLink {
                        op: crate::ast::CompareOp::Eq,
                        op_span: wrapper_span,
                        operand: lambda.clone(),
                    }],
                },
                span: wrapper_span,
            },
        ),
        (
            "member",
            Expr {
                kind: ExprKind::Member {
                    object: Box::new(lambda.clone()),
                    field: "field".to_string(),
                },
                span: wrapper_span,
            },
        ),
        (
            "specialization",
            Expr {
                kind: ExprKind::Specialize {
                    expr: Box::new(lambda.clone()),
                    type_args: vec![type_ref("int64")],
                },
                span: wrapper_span,
            },
        ),
        (
            "cast",
            Expr {
                kind: ExprKind::Cast {
                    expr: Box::new(lambda.clone()),
                    ty: type_ref("int64"),
                },
                span: wrapper_span,
            },
        ),
        (
            "unary",
            Expr {
                kind: ExprKind::Unary {
                    op: crate::ast::UnaryOp::Not,
                    expr: Box::new(lambda.clone()),
                },
                span: wrapper_span,
            },
        ),
        (
            "try",
            Expr {
                kind: ExprKind::Try(Box::new(lambda.clone())),
                span: wrapper_span,
            },
        ),
        (
            "group",
            Expr {
                kind: ExprKind::Group(Box::new(lambda.clone())),
                span: wrapper_span,
            },
        ),
        (
            "call argument",
            Expr {
                kind: ExprKind::Call {
                    callee: Box::new(leaf(3)),
                    args: vec![arg(lambda.clone())],
                },
                span: wrapper_span,
            },
        ),
        (
            "formatted expression",
            Expr {
                kind: ExprKind::FString(vec![
                    crate::ast::FormatPart::Literal("value=".to_string()),
                    crate::ast::FormatPart::Expr(lambda.clone()),
                ]),
                span: wrapper_span,
            },
        ),
        (
            "binary",
            Expr {
                kind: ExprKind::Binary {
                    op: BinaryOp::Add,
                    left: Box::new(leaf(4)),
                    right: Box::new(lambda.clone()),
                },
                span: wrapper_span,
            },
        ),
        (
            "conditional",
            Expr {
                kind: ExprKind::Conditional {
                    then_expr: Box::new(leaf(4)),
                    condition: Box::new(expr(ExprKind::Bool(true))),
                    else_expr: Box::new(lambda.clone()),
                },
                span: wrapper_span,
            },
        ),
        (
            "tuple",
            Expr {
                kind: ExprKind::Tuple(vec![leaf(4), lambda.clone()]),
                span: wrapper_span,
            },
        ),
        (
            "list",
            Expr {
                kind: ExprKind::List(vec![leaf(4), lambda.clone()]),
                span: wrapper_span,
            },
        ),
        (
            "set",
            Expr {
                kind: ExprKind::Set(vec![leaf(4), lambda.clone()]),
                span: wrapper_span,
            },
        ),
        (
            "map",
            Expr {
                kind: ExprKind::Map(vec![crate::ast::MapEntryExpr {
                    key: leaf(4),
                    value: lambda.clone(),
                }]),
                span: wrapper_span,
            },
        ),
        (
            "index",
            Expr {
                kind: ExprKind::Index {
                    object: Box::new(leaf(4)),
                    index: Box::new(lambda.clone()),
                },
                span: wrapper_span,
            },
        ),
        (
            "match arm",
            Expr {
                kind: ExprKind::Match {
                    scrutinee: Box::new(leaf(4)),
                    capability: ReceiverKind::Borrow,
                    arms: vec![crate::ast::MatchExprArm {
                        guard: None,
                        pattern: crate::ast::Pattern::Wildcard(wrapper_span),
                        value: lambda.clone(),
                        span: wrapper_span,
                    }],
                },
                span: wrapper_span,
            },
        ),
    ];
    let builder = AnalysisBuilder::new(&source, &program, Vec::new());

    for (shape, wrapped) in wrappers {
        assert_eq!(
            expression_end_line(&wrapped),
            9,
            "{shape} must retain the final line of a nested lambda body"
        );
        let mut scope = BTreeMap::new();
        builder.extend_lambda_scope_from_expr(&wrapped, 9, 100, &mut scope);
        let parameter = scope
            .get("value")
            .unwrap_or_else(|| panic!("{shape} must expose the nested lambda parameter"));
        assert_eq!(parameter.ty, Type::named("int64"));
        assert_eq!(parameter.definition.line, 1);
    }

    let plain_name = leaf(7);
    assert_eq!(expression_end_line(&plain_name), 7);
    let mut scope = BTreeMap::new();
    builder.extend_lambda_scope_from_expr(&plain_name, 7, 100, &mut scope);
    assert!(
        !scope.contains_key("value"),
        "lambda parameters must not leak into unrelated expressions"
    );
}

#[test]
fn lambda_scope_navigation_reaches_assignment_targets_and_assert_operands() {
    let source = [
        "def main():",
        "    callback: def(int64) -> int64 = lambda value: value",
    ]
    .join("\n");
    let program = checked_program(&source);
    let Item::Function(main) = &program.module.items[0] else {
        panic!("expected main function");
    };
    let crate::ast::Stmt::Assign(assign) = &main.body[0] else {
        panic!("expected callback assignment");
    };
    let mut lambda = assign.value.clone();
    let ExprKind::Lambda { body, .. } = &mut lambda.kind else {
        panic!("expected checked lambda expression");
    };
    body.span = Span::new(9, 1);
    let leaf = || expr(ExprKind::Name("outside".to_string()));
    let span = Span::new(1, 1);
    let statements = vec![
        (
            "member assignment object",
            crate::ast::Stmt::Assign(AssignStmt {
                mutable: false,
                target: AssignTarget::Member {
                    object: Box::new(lambda.clone()),
                    field: "field".to_string(),
                },
                annotation: None,
                op: None,
                value: leaf(),
                span,
            }),
        ),
        (
            "indexed assignment index",
            crate::ast::Stmt::Assign(AssignStmt {
                mutable: false,
                target: AssignTarget::Index {
                    object: Box::new(leaf()),
                    index: Box::new(lambda.clone()),
                },
                annotation: None,
                op: None,
                value: leaf(),
                span,
            }),
        ),
        (
            "assert condition",
            crate::ast::Stmt::Assert(crate::ast::AssertStmt {
                condition: lambda.clone(),
                message: Some(leaf()),
                span,
            }),
        ),
        (
            "assert message",
            crate::ast::Stmt::Assert(crate::ast::AssertStmt {
                condition: expr(ExprKind::Bool(true)),
                message: Some(lambda),
                span,
            }),
        ),
    ];
    let builder = AnalysisBuilder::new(&source, &program, Vec::new());

    for (shape, statement) in statements {
        let mut scope = BTreeMap::new();
        builder.extend_lambda_scope_from_stmts(&[statement], 9, 100, &mut scope);
        let parameter = scope
            .get("value")
            .unwrap_or_else(|| panic!("{shape} must expose the nested lambda parameter"));
        assert_eq!(parameter.ty, Type::named("int64"));
        assert_eq!(parameter.definition.line, 1);
    }
}

#[test]
fn closure_types_preserve_analysis_shape_unknown_detection_and_call_results() {
    let closure_type = |param_ty: Type, return_ty: Type, capture_ty: Type| Type::Closure {
        params: Box::new(vec![crate::sema::FunctionParamContract {
            name: "value".to_string(),
            ty: param_ty,
            passing: ReceiverKind::Borrow,
            has_default: false,
            default_erased: false,
        }]),
        return_type: Box::new(return_ty),
        captures: Box::new(vec![crate::sema::ClosureCapture {
            name: "offset".to_string(),
            ty: capture_ty,
            mode: crate::sema::ClosureCaptureMode::Copy,
            span: Span::new(4, 31),
        }]),
        call_kind: crate::sema::ClosureCallKind::Repeatable,
    };
    let concrete = closure_type(
        Type::named("int64"),
        Type::named("str"),
        Type::named("int64"),
    );
    let unknown_param = closure_type(
        Type::named("Unknown"),
        Type::named("str"),
        Type::named("int64"),
    );
    let unknown_return = closure_type(
        Type::named("int64"),
        Type::named("Unknown"),
        Type::named("int64"),
    );
    let unknown_capture = closure_type(
        Type::named("int64"),
        Type::named("str"),
        Type::named("Unknown"),
    );

    assert_eq!(base_type_name(&concrete), "closure");
    assert!(concrete.type_arguments().is_empty());
    assert_eq!(
        concrete.to_string(),
        "closure def(int64) -> str",
        "analysis hovers should expose the closure call contract without capture internals"
    );
    assert!(!analysis_type_contains_unknown(&concrete));
    for unknown in [&unknown_param, &unknown_return, &unknown_capture] {
        assert!(
            analysis_type_contains_unknown(unknown),
            "Unknown nested anywhere in a closure contract must remain observable to inference"
        );
    }

    let serialized = serde_json::to_value(&concrete).expect("closure type should serialize");
    assert_eq!(
        serialized["Closure"]["captures"][0]["name"],
        serde_json::json!("offset")
    );
    assert_eq!(
        serialized["Closure"]["call_kind"],
        serde_json::json!("Repeatable")
    );

    let program = checked_program("def main():\n    pass\n");
    let builder = AnalysisBuilder::new("", &program, Vec::new());
    let binding = |ty: Type| super::BindingInfo {
        ty,
        trait_bounds: Vec::new(),
        definition: super::AnalysisRange {
            file_path: None,
            line: 0,
            start_character: 0,
            end_character: 1,
        },
        hover: String::new(),
    };
    let scope = BTreeMap::from([
        ("concrete".to_string(), binding(concrete.clone())),
        ("unknown".to_string(), binding(unknown_capture)),
    ]);
    assert_eq!(
        builder.infer_call_type(
            &expr(ExprKind::Name("concrete".to_string())),
            &[arg(expr(ExprKind::Int(1)))],
            &scope,
        ),
        Some(Type::named("str"))
    );
    assert_eq!(
        builder.infer_expr_type(
            &expr(ExprKind::Conditional {
                then_expr: Box::new(expr(ExprKind::Name("unknown".to_string()))),
                condition: Box::new(expr(ExprKind::Bool(true))),
                else_expr: Box::new(expr(ExprKind::Name("concrete".to_string()))),
            }),
            &scope,
        ),
        Some(concrete.clone()),
        "a concrete closure contract must win over a structurally unknown alternative"
    );
    assert!(
        builder.member_completions(&concrete).is_empty(),
        "closure capture metadata must not masquerade as generic member surface"
    );
}

#[test]
fn lambda_analysis_preserves_parameter_modes_and_vec_map_result_types() {
    let source = [
        "def main():",
        "    offset: int64 = 2",
        "    mut values: list[int64] = [1, 2]",
        "    consume: def(own str) -> str = lambda own text: text",
        "    measure: def(mut list[int64]) -> int64 = lambda mut items: items.len()",
        "    mapped = values.map(lambda value: value + offset)",
        "    print(mapped)",
    ]
    .join("\n");

    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "valid contextual lambda modes should analyze cleanly: {:?}",
        analysis.diagnostics
    );
    for expected in [
        "param text: own str",
        "param items: mut list[int64]",
        "binding mapped: list[int64]",
    ] {
        assert!(
            analysis
                .occurrences
                .iter()
                .any(|occurrence| occurrence.hover.contains(expected)),
            "missing semantic hover `{expected}`"
        );
    }

    let mapped_line = source
        .lines()
        .position(|line| line.contains("values.map"))
        .expect("mapped assignment should exist");
    let completions = complete_source(
        &source,
        mapped_line,
        source.lines().nth(mapped_line).unwrap().len(),
        None,
    )
    .expect("completion inside list.map lambda should succeed");
    for name in ["value", "offset"] {
        assert!(
            completions.iter().any(|completion| completion.name == name),
            "`{name}` should be visible inside the list.map lambda"
        );
    }
}

#[test]
fn path_analysis_infers_imported_function_values_and_member_call_results() {
    let temp = TempDir::new("analysis-imported-function-values");
    let helper_path = temp.path().join("helpers.au");
    let main_path = temp.path().join("main.au");
    fs::write(
        &helper_path,
        [
            "public def decorate(prefix: str, value: str = \"world\") -> str:",
            "    return prefix + value",
        ]
        .join("\n"),
    )
    .expect("should write helper module");
    let source = [
        "import helpers",
        "",
        "def main() -> int32:",
        "    direct = helpers.decorate(prefix=\"hello\")",
        "    selected = helpers.decorate",
        "    outcome = selected(prefix=\"hello\")",
        "    print(direct.len())",
        "    print(outcome.len())",
        "    return 0",
    ]
    .join("\n");
    fs::write(&main_path, &source).expect("should write main module");

    let analysis = analyze_path_source(&main_path, &source);
    let serialized = serde_json::to_value(&analysis).expect("analysis should serialize");
    assert_eq!(serialized["diagnostics"], serde_json::json!([]));
    let hovers = serialized["occurrences"]
        .as_array()
        .expect("occurrences should serialize as an array")
        .iter()
        .filter_map(|occurrence| occurrence["hover"].as_str())
        .collect::<Vec<_>>();
    assert!(
        hovers.contains(&"```aura\nbinding selected: def(str, str) -> str\n```"),
        "an imported function member must retain its full callable type: {hovers:?}"
    );
    for expected in [
        "```aura\nbinding direct: str\n```",
        "```aura\nbinding outcome: str\n```",
    ] {
        assert!(
            hovers.contains(&expected),
            "direct and indirect imported calls must infer str: {hovers:?}"
        );
    }
}

#[test]
fn completion_preserves_array_types_through_group_call_index_and_slice_receivers() {
    let cases = [
        (
            "def main():\n    values = Array[int32].zeros(shape=[2, 2])\n    (values).\n",
            2,
            13,
            "shape",
        ),
        (
            "def main():\n    values = Array[int32].zeros(shape=[2, 2])\n    values.clone().\n",
            2,
            19,
            "mean",
        ),
        (
            "def main():\n    values = Array[int32].zeros(shape=[2, 2])\n    values[:1].\n",
            2,
            15,
            "sum",
        ),
        (
            "def main():\n    values = Array[int32].zeros(shape=[2, 2])\n    values[0, 1].\n",
            2,
            17,
            "wrapping_add",
        ),
    ];

    for (source, line, character, expected_member) in cases {
        let completions = complete_source(source, line, character, Some('.'))
            .unwrap_or_else(|error| panic!("completion failed for `{source}`: {error}"));
        assert!(
            completions
                .iter()
                .any(|completion| completion.name == expected_member),
            "`{expected_member}` missing for `{source}`: {completions:?}"
        );
    }
}

#[test]
fn fixed_width_integer_completion_exposes_all_shift_arithmetic_modes() {
    let completions = builtin_member_completions(&Type::named("int32"));
    for (name, detail) in [
        ("wrapping_shl", "wrapping_shl(count: Self) -> Self"),
        ("wrapping_shr", "wrapping_shr(count: Self) -> Self"),
        ("saturating_shl", "saturating_shl(count: Self) -> Self"),
        ("saturating_shr", "saturating_shr(count: Self) -> Self"),
    ] {
        assert!(
            completions
                .iter()
                .any(|completion| completion.name == name && completion.detail == detail),
            "missing `{name}` completion with `{detail}`: {completions:?}"
        );
    }
}

#[test]
fn conditional_function_type_inference_prefers_concrete_nested_contracts() {
    let program = checked_program("def main():\n    pass\n");
    let builder = AnalysisBuilder::new("", &program, Vec::new());
    let function_type = |param_ty: Type, return_type: Type| Type::Function {
        params: vec![crate::sema::FunctionParamContract {
            name: "value".to_string(),
            ty: param_ty,
            passing: ReceiverKind::Borrow,
            has_default: false,
            default_erased: false,
        }],
        return_type: Box::new(return_type),
    };
    let concrete = function_type(
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        Type::named("str"),
    );
    let unknown_param = function_type(
        Type::Named("list".to_string(), vec![Type::named("Unknown")]),
        Type::named("str"),
    );
    let unknown_return = function_type(Type::named("int32"), Type::named("Unknown"));
    let binding = |ty: Type| super::BindingInfo {
        ty,
        trait_bounds: Vec::new(),
        definition: super::AnalysisRange {
            file_path: None,
            line: 0,
            start_character: 0,
            end_character: 1,
        },
        hover: String::new(),
    };
    let scope = BTreeMap::from([
        ("concrete".to_string(), binding(concrete.clone())),
        ("unknown_param".to_string(), binding(unknown_param)),
        ("unknown_return".to_string(), binding(unknown_return)),
    ]);
    let conditional = |then_name: &str, else_name: &str| {
        expr(ExprKind::Conditional {
            then_expr: Box::new(expr(ExprKind::Name(then_name.to_string()))),
            condition: Box::new(expr(ExprKind::Bool(true))),
            else_expr: Box::new(expr(ExprKind::Name(else_name.to_string()))),
        })
    };

    assert_eq!(
        builder.infer_expr_type(&conditional("unknown_param", "concrete"), &scope),
        Some(concrete.clone()),
        "Unknown nested in a function parameter must not mask a concrete callable contract"
    );
    assert_eq!(
        builder.infer_expr_type(&conditional("concrete", "unknown_return"), &scope),
        Some(concrete.clone()),
        "Unknown nested in a function return must not mask a concrete callable contract"
    );
    assert!(
        concrete.type_arguments().is_empty(),
        "function parameters and returns are schema fields, not generic type arguments"
    );
    assert!(
        builder.member_completions(&concrete).is_empty(),
        "a function value must not inherit member completions from its parameter or return types"
    );
}

#[test]
fn path_aware_analysis_handles_large_repo_scratch_corpus_without_panicking() {
    run_with_large_stack(|| {
        let repo_root = repo_root();
        let corpus_dirs = [repo_root.join("test_edge"), repo_root.join("test_recheck")];
        let mut file_count = 0usize;
        let mut symbol_total = 0usize;

        for dir in corpus_dirs {
            for path in collect_aura_files(&dir) {
                file_count += 1;
                let source = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("failed to read {}: {}", path.display(), error));
                let output = analyze_path_source(&path, &source);
                symbol_total += output.symbols.len();
            }
        }

        assert!(file_count >= 800, "expected large scratch corpus");
        assert!(
            symbol_total > 0,
            "expected scratch corpus analysis to produce some symbols"
        );
    });
}
