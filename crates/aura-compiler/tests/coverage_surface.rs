use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use aura_compiler::call::BuiltinMember;
use aura_compiler::diag::Span;
use aura_compiler::mir::{
    BasicBlock, CallTarget, Instruction, MirArg, MirExternCall, MirFunction, MirLocalType,
    MirModule, Operand, Rvalue, Terminator,
};
use aura_compiler::sema::Type;
use aura_compiler::{
    analyze_path_source, analyze_source, check_path, check_source, complete_path_source,
    complete_source, emit_host_native_object, emit_host_native_object_with_metadata,
    lower_path_to_mir, lower_source_to_mir, run_mir, run_path,
    run_path_with_source_and_stdout_sink, run_path_with_stdout_sink, run_serialized_mir,
    run_source, run_source_with_stdout_sink, StdoutSink,
};

struct TempDir {
    path: PathBuf,
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

    fn write(&self, relative: &str, source: &str) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        fs::write(&path, source).expect("failed to write module source");
        path
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
        .expect("compiler crate should live under repo root")
        .parent()
        .expect("compiler crate should live under repo root")
        .to_path_buf()
}

fn line_and_character(source: &str, needle: &str) -> (usize, usize) {
    let offset = source
        .find(needle)
        .unwrap_or_else(|| panic!("expected to find `{needle}` in source"));
    let before = &source[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count();
    let character = before
        .rsplit_once('\n')
        .map(|(_, tail)| tail.chars().count())
        .unwrap_or_else(|| before.chars().count());
    (line, character + needle.chars().count())
}

fn capture_stdout_sink() -> (Arc<Mutex<String>>, StdoutSink) {
    let captured = Arc::new(Mutex::new(String::new()));
    let sink_capture = captured.clone();
    let sink: StdoutSink = Arc::new(move |chunk| {
        sink_capture
            .lock()
            .expect("capture sink lock should not be poisoned")
            .push_str(chunk);
    });
    (captured, sink)
}

#[test]
fn broad_surface_source_covers_public_compiler_entrypoints() {
    let source = r#"
trait Labelled:
    def label(self) -> str

class Counter:
    value: int32

    def bump(mut self, amount: int32 = 1) -> int32:
        self.value += amount
        return self.value

impl Labelled for Counter:
    def label(self) -> str:
        return f"Counter({self.value})"

class Badge:
    text: str

impl Labelled for Badge:
    def label(self) -> str:
        return self.text.clone()

class Resource:
    closed: bool = false

    def close(mut self):
        self.closed = true

def worker(value: int32) -> int32:
    return value + 1

def produce(queue: Queue[int32]) -> None:
    queue.put(11)
    queue.close()

def summarize[T: Labelled](value: T) -> str:
    return value.label()

def parse_value(text: str) -> Result[int32, str]:
    return parse_int32(text)

def parse_and_offset(text: str) -> Result[int32, str]:
    parsed = try parse_value(text)
    return Result.Ok(parsed + 5)

def print_int_option(value: Option[int32]) -> None:
    match value:
        case Some(inner):
            print(inner)
        case None:
            print(-1)

def main() -> int32:
    text = "  Aura Repo  "
    trimmed = text.trim()
    words = trimmed.split(" ")
    print(trimmed.len())
    print(trimmed.contains("Repo"))
    print(trimmed.starts_with("Aura"))
    print(trimmed.ends_with("Repo"))
    print(trimmed.replace("Repo", "Lang"))
    print(trimmed.to_lower())
    print(trimmed.to_upper())
    print("-".join(words))
    print(trimmed.strip_prefix("Aura "))
    print(trimmed.strip_suffix(" Repo"))

    print(parse_int32("12"))
    print(parse_int64("42"))
    print(parse_float64("2.5"))
    print(abs(-3))
    print(min(9, 4))
    print(max(9, 4))
    print(sqrt(9.0))
    mut total = 9.0
    total = total + 1.0
    total = total - 2.0
    total = total * 3.0
    total = total / 2.0
    total = total % 2.0
    print(total)
    if total > 0.0 and total >= 0.0 and total < 10.0 and total <= 10.0 and total != 4.5 and total == total:
        print("float-ok")
    rounded = total as int32
    print(rounded)
    print((rounded as float64))
    parsed_result = parse_and_offset("7")
    print(parsed_result)

    mut values: list[int32] = [1, 2]
    values.append(3)
    print(values.get(1))
    print(values[0])
    print(values.set(0, 9))
    values[1] = 8
    print(values.pop(0))
    print(values.swap(0, 0))
    print(values.contains(8))
    print(values.insert(1, 7))
    values.reverse()
    values.extend([5, 6])
    print(values == [3, 7, 8, 5, 6])
    print_int_option(values.get(0))
    mut range_total: int64 = 0
    for number in range(values.len()):
        range_total += number
    print(range_total)
    for value in values:
        print(value)
    for value in mut values:
        value += 1
    print(values[0])
    values.clear()
    print(values.is_empty())

    mut counts = {"a": 1}
    print(counts.get("a"))
    print(counts["a"])
    counts["a"] = 2
    print(counts["a"])
    counts["b"] = 3
    print(counts.remove("a"))
    print("b" in counts)
    print(counts.keys().len())
    print(counts.values().len())
    print(counts.items().len())
    print(counts.items().len())
    counts.update({"c": 4})
    counts.clear()
    print(counts.is_empty())

    mut seen = {"x"}
    print(seen.add("y"))
    print(seen.remove("x"))
    print("y" in seen)
    print(seen.len())

    mut counter = Counter(value=1)
    print(counter.bump())
    print(summarize(counter))
    print(summarize(Badge(text="badge")))

    jobs = Queue[int32]()
    print(jobs.put(7))
    print(jobs.get())
    jobs.close()

    with TaskGroup() as group:
        task = group.start(worker, 4)
        print(task.result())

    stream = Queue[int32]()
    with TaskGroup() as group:
        group.start_soon(produce, stream)
        for item in stream:
            print(item)

    empty_any = list[Task[int32]]()
    match wait_any(empty_any, timeout=1ms):
        case WaitAny.Ready(index, value):
            print(index)
            print(value)
        case WaitAny.Error(index, message):
            print(index)
            print(message)
        case WaitAny.TimedOut:
            print("timedout")
        case WaitAny.Cancelled:
            print("cancelled")

    empty_all = list[Task[int32]]()
    match wait_all(empty_all, timeout=1ms):
        case WaitAll.Ready(results):
            print(results.len())
        case WaitAll.Error(index, message):
            print(index)
            print(message)
        case WaitAll.TimedOut:
            print("timedout")
        case WaitAll.Cancelled:
            print("cancelled")

    with Resource() as resource:
        print(resource.closed)

    match 1:
        case 0:
            print("zero")
        case 1:
            print("one")
        case _:
            print("other")

    match "go":
        case "stop":
            print("stop")
        case "go":
            print("go")
        case _:
            print("other")

    match true:
        case false:
            print("no")
        case true:
            print("yes")

    return rounded
"#;

    let program = check_source(source).expect("broad source should type-check");
    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "analysis should stay clean: {:?}",
        analysis.diagnostics
    );

    let completion_source = r#"
def main() -> int32:
    mut values = [1, 2]
    values.
    return 0
"#;
    let (line, character) = line_and_character(completion_source, "values.");
    let completions = complete_source(completion_source, line, character, Some('.'))
        .expect("completion should work on collection receiver");
    let completion_names = completions
        .iter()
        .map(|item| item.name.as_str())
        .collect::<BTreeSet<_>>();
    assert!(completion_names.contains("append"));
    assert!(completion_names.contains("reverse"));

    let output = run_source(source).expect("broad source should run");
    let mir = lower_source_to_mir(source).expect("broad source should lower to MIR");
    let mir_output = run_mir(&mir).expect("broad source MIR should run");
    assert_eq!(mir_output.stdout, output.stdout);
    assert!(!program.functions.is_empty());

    let object = emit_host_native_object(&mir).expect("broad source should emit a native object");
    assert!(!object.is_empty());
    let metadata_object = emit_host_native_object_with_metadata(&mir, "/tmp/broad.au", source)
        .expect("broad source should emit a metadata-backed native object");
    assert!(!metadata_object.is_empty());
}

#[test]
fn public_mir_callable_values_cover_specialization_defaults_and_dynamic_task_targets() {
    let source = r#"
import process

class Pipeline:
    transform: def(int32) -> int32

def increment(value: int32) -> int32:
    return value + 1

def double(value: int32) -> int32:
    return value * 2

def show[T](value: T) -> None:
    print(value)

def take_first[A, B](first: own A, second: B) -> A:
    return first

def mark(label: str, value: int32) -> int32:
    print(label)
    return value

def first_default(value: int32 = mark("first-default", 11)) -> int32:
    return value

def second_default(value: int32 = mark("second-default", 22)) -> int32:
    return value

def choose_transform(use_increment: bool) -> def(int32) -> int32:
    return increment if use_increment else double

def main() -> int32:
    selected = choose_transform(false)
    specialized = show[int32]
    known_default = first_default
    selected_default = first_default if false else second_default
    pipeline = Pipeline(transform=selected)
    callbacks: list[def(int32) -> int32] = [double]
    stdio_factory: def() -> process.Stdio = process.pipe

    print(selected(4))
    specialized(9)
    print(known_default(value=30))
    print(selected_default())
    match own stdio_factory():
        case process.Stdio.Pipe:
            print("pipe")
        case process.Stdio.Null:
            print("null")
        case process.Stdio.Inherit:
            print("inherit")

    with group = TaskGroup():
        field_task = group.start(pipeline.transform, 5)
        index_task = group.start(callbacks[0], 6)
        generic_task = group.start(take_first[int32, int32], 7, 8)
        print(field_task.result_or(-1, timeout=1s))
        print(index_task.result_or(-1, timeout=1s))
        print(generic_task.result_or(-1, timeout=1s))
    return 0
"#;

    check_source(source).expect("callable-value integration source should type-check");
    let direct = run_source(source).expect("callable-value integration source should run");
    assert_eq!(
        direct.stdout,
        "8\n9\n30\nsecond-default\n22\npipe\n10\n12\n7\n"
    );

    let mir = lower_source_to_mir(source).expect("callable-value source should lower to MIR");
    let mir_output = run_mir(&mir).expect("callable-value MIR should run");
    assert_eq!(mir_output.stdout, direct.stdout);
}

#[test]
fn public_mir_lowering_preserves_contextual_floats_operators_and_consuming_match_updates() {
    let source = r#"
trait Add[Rhs, Out]:
    def add(self, rhs: Rhs) -> Out

trait Neg[Out]:
    def neg(self) -> Out

copy class Score:
    value: int32

impl Add[Score, Score] for Score:
    def add(self, rhs: Score) -> Score:
        return Score(value=self.value + rhs.value)

impl Neg[Score] for Score:
    def neg(self) -> Score:
        return Score(value=0 - self.value)

enum Bucket:
    Items(list[int32])
    Empty

def main() -> int32:
    base: float32 = 2.5
    first: float32 = (1.25) if true else base
    second: float32 = base if false else (3.5)
    print(first)
    print(second)

    total = Score(value=1) + Score(value=2)
    print(total.value)
    negative = -total
    print(negative.value)

    match own ("left", "right"):
        case (left, right):
            print(f"{left}:{right}")

    mut bucket = Bucket.Items([1])
    match mut bucket:
        case Items(items):
            items.append(2)
        case Empty:
            pass
    match own bucket:
        case Items(items):
            print(items.len())
        case Empty:
            print(0)
    return 0
"#;

    check_source(source).expect("combined MIR behavior source should type-check");
    let direct = run_source(source).expect("combined MIR behavior source should run");
    assert_eq!(direct.stdout, "1.25\n3.5\n3\n-3\nleft:right\n2\n");

    let mir = lower_source_to_mir(source).expect("combined behavior source should lower to MIR");
    let mir_output = run_mir(&mir).expect("combined behavior MIR should execute");
    assert_eq!(mir_output.stdout, direct.stdout);
    assert!(
        mir.functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::TupleTakeElement { .. },
                    ..
                }
            )),
        "consuming tuple bindings must take their owned elements"
    );
    assert!(
        mir.functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                Instruction::Assign {
                    value:
                        Rvalue::Call {
                            callee:
                                CallTarget::Member {
                                    field,
                                    receiver_place: Some(_),
                                    ..
                                },
                            ..
                        },
                    ..
                } if field == "append"
            )),
        "mutable match append must retain the mutating receiver place"
    );
    assert!(!emit_host_native_object(&mir)
        .expect("combined behavior source should lower through the direct backend")
        .is_empty());
}

#[cfg(unix)]
#[test]
fn manifest_authorized_ffi_lowering_exposes_exact_extern_mir_to_backends() {
    let path = repo_root().join("examples/packages/ffi_getpid/src/main.au");
    let mir = lower_path_to_mir(&path).expect("maintained FFI example should lower by path");
    let extern_call = mir
        .functions
        .iter()
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
            } if call.symbol == "getpid" => Some(call),
            _ => None,
        })
        .expect("maintained FFI MIR should contain the getpid extern call");
    assert_eq!(extern_call.abi, "C");
    assert!(extern_call.params.is_empty());
    assert_eq!(extern_call.return_type, Type::named("int32"));

    let output =
        run_path(&path).expect("manifest-authorized FFI path should use the trusted route");
    assert_eq!(output.stdout, "true\n");
    let public_error =
        run_mir(&mir).expect_err("arbitrary public MIR execution must reject extern metadata");
    assert_eq!(public_error.code, "AU4001");
    assert!(public_error.message.contains("getpid"));
    assert!(!emit_host_native_object(&mir)
        .expect("exact extern MIR should lower through the direct backend adapter")
        .is_empty());
}

#[test]
fn public_native_codegen_rejects_invalid_mir_surface() {
    let invalid_module = MirModule {
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
    let error = emit_host_native_object(&invalid_module)
        .expect_err("invalid MIR terminators should fail through the public native codegen API");
    assert!(
        error.contains("does not yet support MIR terminator"),
        "unexpected native codegen error: {error}"
    );

    let mut invalid_monotonic_module = lower_source_to_mir(
        r#"
import sys

def main() -> int32:
    observed: int64 = sys.monotonic_time_ms()
    return observed as int32
"#,
    )
    .expect("valid monotonic clock source should lower to MIR");
    let monotonic_args = invalid_monotonic_module
        .functions
        .iter_mut()
        .flat_map(|function| function.blocks.iter_mut())
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Name(name),
                        args,
                    },
                ..
            } if name == "sys::monotonic_time_ms" => Some(args),
            _ => None,
        })
        .expect("lowered source should contain the monotonic clock call");
    monotonic_args.push(MirArg {
        name: None,
        value: Operand::Int(0),
        writeback_place: None,
    });

    let error = emit_host_native_object(&invalid_monotonic_module)
        .expect_err("monotonic clock calls with arguments should fail direct codegen");
    assert_eq!(
        error,
        "direct backend expected `sys::monotonic_time_ms` to receive no arguments, found 1"
    );
}

#[test]
fn public_stdout_sink_wrappers_capture_source_path_and_path_override_output() {
    let source = r#"
def main() -> int32:
    print("source")
    return 0
"#;
    let (captured_source, source_sink) = capture_stdout_sink();
    let source_output =
        run_source_with_stdout_sink(source, source_sink).expect("source sink wrapper should run");
    assert_eq!(source_output.stdout, "source\n");
    assert_eq!(
        captured_source
            .lock()
            .expect("captured source output should be readable")
            .as_str(),
        "source\n"
    );

    let temp = TempDir::new("aura-stdout-sink");
    let main_path = temp.write(
        "main.au",
        r#"def main() -> int32:
    print("path")
    return 0
"#,
    );
    let (captured_path, path_sink) = capture_stdout_sink();
    let path_output =
        run_path_with_stdout_sink(&main_path, path_sink).expect("path sink wrapper should run");
    assert_eq!(path_output.stdout, "path\n");
    assert_eq!(
        captured_path
            .lock()
            .expect("captured path output should be readable")
            .as_str(),
        "path\n"
    );

    let override_source = r#"def main() -> int32:
    print("override")
    return 0
"#;
    let (captured_override, override_sink) = capture_stdout_sink();
    let override_output =
        run_path_with_source_and_stdout_sink(&main_path, override_source, override_sink)
            .expect("path-with-source sink wrapper should run");
    assert_eq!(override_output.stdout, "override\n");
    assert_eq!(
        captured_override
            .lock()
            .expect("captured override output should be readable")
            .as_str(),
        "override\n"
    );
}

#[test]
fn public_serialized_mir_api_runs_safe_payloads_and_rejects_forged_ffi() {
    let safe_source = "def main():\n    print(\"serialized-safe\")\n";
    let safe = lower_source_to_mir(safe_source).expect("safe source should lower to MIR");
    let safe_json = serde_json::to_vec(&safe).expect("safe MIR should serialize");
    let output = run_serialized_mir(&safe_json, "/virtual/safe.au", safe_source)
        .expect("public serialized-MIR execution should run safe payloads");
    assert_eq!(output.stdout, "serialized-safe\n");

    let malformed = run_serialized_mir(b"{not json", "/virtual/bad.au", "def main():\n    pass\n")
        .expect_err("malformed serialized MIR must be diagnosed");
    assert_eq!(malformed.code, "AU4001");
    assert!(
        malformed
            .message
            .contains("failed to deserialize embedded MIR"),
        "{}",
        malformed.message
    );

    let forged = MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<forged>".to_string(),
            source_path: Some("/virtual/forged.au".to_string()),
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![MirLocalType {
                name: "process_id".to_string(),
                ty: Type::named("int32"),
            }],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![Instruction::Assign {
                    target: "process_id".to_string(),
                    value: Rvalue::Call {
                        callee: CallTarget::Extern(MirExternCall {
                            symbol: "getpid".to_string(),
                            abi: "C".to_string(),
                            params: Vec::new(),
                            return_type: Type::named("int32"),
                        }),
                        args: Vec::new(),
                    },
                }],
                terminator: Terminator::Return(Operand::Place("process_id".to_string())),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    let forged_json = serde_json::to_vec(&forged).expect("forged MIR should serialize");
    let rejected = run_serialized_mir(
        &forged_json,
        "/virtual/forged.au",
        "def main():\n    pass\n",
    )
    .expect_err("public serialized-MIR execution must reject caller-supplied FFI metadata");
    assert_eq!(rejected.code, "AU4001");
    assert!(rejected.message.contains("getpid"));
    assert!(rejected.message.contains("manifest-rooted package"));
    assert!(rejected.message.contains("path-based API"));
}

#[test]
fn native_runtime_abi_accepts_valid_embedded_buffers_and_returns_main_code() {
    let source = "def main() -> int32:\n    return 7\n";
    let module = lower_source_to_mir(source).expect("native ABI source should lower");
    let mir_json = serde_json::to_vec(&module).expect("native ABI MIR should serialize");
    let source_path = b"/virtual/native-abi.au";

    // SAFETY: each pointer refers to the paired live byte slice for the
    // duration of the call, exactly as the exported ABI requires.
    let code = unsafe {
        aura_compiler::mir_runtime::aura_native_run(
            mir_json.as_ptr(),
            mir_json.len(),
            source_path.as_ptr(),
            source_path.len(),
            source.as_ptr(),
            source.len(),
        )
    };
    assert_eq!(
        code, 7,
        "the native runtime ABI must return main's int32 exit code"
    );
}

#[test]
fn public_from_imports_cover_builtin_module_export_resolution() {
    let import_source = r#"
from fs import exists, File
from io import Error
from process import Stdio

def main() -> None:
    pass
"#;
    check_source(import_source).expect("builtin function class and enum imports should resolve");

    let run_source_text = r#"
from fs import exists

def main() -> int32:
    print(exists(path="/path/that/should/not/exist"))
    return 0
"#;
    let output = run_source(run_source_text).expect("builtin from-imported function should run");
    assert_eq!(output.stdout, "false\n");

    let missing_export = check_source(
        r#"from fs import Missing

def main() -> None:
    pass
"#,
    )
    .expect_err("missing builtin export should fail through from-import resolution");
    assert!(
        missing_export
            .message
            .contains("module `fs` has no export named `Missing`"),
        "unexpected builtin import diagnostic: {}",
        missing_export.message
    );

    let duplicate_source = check_source(
        r#"from fs import exists
from fs import exists

def main() -> None:
    pass
"#,
    )
    .expect_err("duplicate builtin source imports should fail");
    assert!(
        duplicate_source
            .message
            .contains("duplicate import binding `exists`"),
        "unexpected duplicate builtin source import diagnostic: {}",
        duplicate_source.message
    );

    let temp = TempDir::new("aura-builtin-from-import-duplicate");
    let duplicate_path = temp.write(
        "main.au",
        r#"from fs import exists
from fs import exists

def main() -> None:
    pass
"#,
    );
    let duplicate_path_error =
        check_path(&duplicate_path).expect_err("duplicate builtin path imports should fail");
    assert!(
        duplicate_path_error
            .message
            .contains("duplicate import binding `exists`"),
        "unexpected duplicate builtin path import diagnostic: {}",
        duplicate_path_error.message
    );
}

#[test]
fn imported_main_function_is_not_treated_as_the_local_entrypoint() {
    let temp = TempDir::new("aura-imported-main-entrypoint");
    temp.write(
        "helpers/entry.au",
        r#"public def main(value: int32) -> int32:
    return value + 3
"#,
    );
    let main_path = temp.write(
        "main.au",
        r#"from helpers.entry import main

print(main(4))
"#,
    );

    let output = run_path(&main_path).expect("imported main should be callable from a script");
    assert_eq!(output.stdout, "7\n");

    let mir = lower_path_to_mir(&main_path).expect("imported main script should lower to MIR");
    let mir_output = run_mir(&mir).expect("imported main script MIR should run");
    assert_eq!(mir_output.stdout, output.stdout);

    let object = emit_host_native_object(&mir)
        .expect("direct backend should emit imported main script object");
    assert!(!object.is_empty());
}

#[test]
fn public_surface_covers_escape_diagnostics_argument_counts_and_builtin_member_metadata() {
    let source = r#"
def main() -> int32:
    text = "\0\x41\u{1F600}"
    label = f"\x42\u{43}"
    braces = f"{{literal}}"
    print(text.contains("A"))
    print(label)
    print(braces)
    return 0
"#;

    let output = run_source(source).expect("escape and writeback source should run");
    assert_eq!(output.stdout, "true\nBC\n{literal}\n");

    let mir = lower_source_to_mir(source).expect("escape and writeback source should lower");
    let mir_output = run_mir(&mir).expect("escape and writeback MIR should run");
    assert_eq!(mir_output.stdout, output.stdout);

    assert_eq!(
        BuiltinMember::VecPush.receiver_passing(),
        aura_compiler::ast::ReceiverKind::BorrowMut
    );
    assert_eq!(
        BuiltinMember::MapSet.receiver_passing(),
        aura_compiler::ast::ReceiverKind::BorrowMut
    );
    assert_eq!(
        BuiltinMember::VecLen.receiver_passing(),
        aura_compiler::ast::ReceiverKind::Borrow
    );
    assert_eq!(
        BuiltinMember::StringContains.receiver_passing(),
        aura_compiler::ast::ReceiverKind::Borrow
    );

    let invalid_escape_cases = [
        (
            "def main() -> None:\n    text = \"\\x4g\"\n",
            "invalid hexadecimal escape sequence",
        ),
        (
            "def main() -> None:\n    text = \"\\xg4\"\n",
            "invalid hexadecimal escape sequence",
        ),
        (
            "def main() -> None:\n    text = \"\\u{}\"\n",
            "unicode escape sequences must include at least one hexadecimal digit",
        ),
        (
            "def main() -> None:\n    text = \"\\u{110000}\"\n",
            "unicode escape sequence is out of range",
        ),
        (
            "def main() -> None:\n    text = \"\\u{100000000}\"\n",
            "unicode escape sequence is out of range",
        ),
        (
            "def main() -> None:\n    text = \"\\u{zz}\"\n",
            "invalid unicode escape sequence",
        ),
        (
            "def main() -> None:\n    text = \"\\u{12",
            "unterminated string literal",
        ),
        (
            "def main() -> None:\n    text = \"\\u1234\"\n",
            "unicode escape sequences must use the form `\\u{...}`",
        ),
        (
            "def main() -> None:\n    text = \"\\x",
            "unsupported escape sequence `\\x`",
        ),
        (
            "def main() -> None:\n    text = \"\\x4",
            "unsupported escape sequence `\\x`",
        ),
        (
            "def main() -> None:\n    text = \"\\x4\"\n",
            "invalid hexadecimal escape sequence",
        ),
    ];
    for (source, expected) in invalid_escape_cases {
        let error = check_source(source).expect_err("invalid escape should fail through check");
        assert!(
            error.message.contains(expected),
            "expected `{expected}`, got `{}`",
            error.message
        );
    }

    let arity_error = check_source("def main() -> None:\n    print(1, 2)\n")
        .expect_err("too many builtin args should fail through check");
    assert!(
        arity_error
            .message
            .contains("`print` expects 1 argument, found 2"),
        "unexpected arity diagnostic: {}",
        arity_error.message
    );

    for (source, expected) in [
        (
            "def main() -> None:\n    value = 1e\n",
            "invalid floating-point literal",
        ),
        (
            "def main() -> None:\n    value = 1e99999\n",
            "floating-point literal is out of range",
        ),
    ] {
        let error = check_source(source).expect_err("invalid float literal should fail");
        assert!(
            error.message.contains(expected),
            "expected `{expected}`, got `{}`",
            error.message
        );
    }
}

#[test]
fn path_surface_covers_modules_analysis_completion_and_direct_codegen() {
    let temp = TempDir::new("aura-coverage-surface");
    temp.write(
        "pkg/named.au",
        r#"public trait Named:
    def name(self) -> str
"#,
    );
    temp.write(
        "pkg/user.au",
        r#"from pkg.named import Named

public class User:
    public label: str

impl Named for User:
    def name(self) -> str:
        return self.label.clone()

public enum Outcome:
    Ready(code: int32, reason: str)
    Empty
"#,
    );
    temp.write(
        "helpers/factory.au",
        r#"from pkg.user import User

public def describe_user(name: own str) -> str:
    user = User(label=name)
    return user.name()
"#,
    );
    let main_source = r#"import pkg.user
from helpers.factory import describe_user

def main() -> int32:
    user: pkg.user.User = pkg.user.User(label="Ada")
    outcome = pkg.user.Outcome.Ready(code=7, reason="ok")
    print(describe_user(name=user.label.clone()))
    print(user.name())
    return 0
"#;
    let main_path = temp.write("main.au", main_source);

    let program = check_path(&main_path).expect("package program should type-check");
    let analysis = analyze_path_source(&main_path, main_source);
    assert!(
        analysis.diagnostics.is_empty(),
        "path analysis should stay clean: {:?}",
        analysis.diagnostics
    );
    assert!(
        analysis
            .occurrences
            .iter()
            .any(|occurrence| occurrence.hover == "```aura\nenum pkg.user.Outcome\n```"),
        "qualified enum references should expose the imported enum hover"
    );
    assert!(
        analysis.occurrences.iter().any(|occurrence| {
            occurrence.hover
                == "```aura\nvariant Ready(code: own int32, reason: own str) -> pkg.user.Outcome\n```"
        }),
        "qualified enum constructors should expose every named payload as owned"
    );

    let completion_source = r#"import pkg.user
from helpers.factory import describe_user

def main() -> int32:
    user: pkg.user.User = pkg.user.User(label="Ada")
    user.
    print(describe_user(name=user.label.clone()))
    return 0
"#;
    let (line, character) = line_and_character(completion_source, "user.");
    let completions =
        complete_path_source(&main_path, completion_source, line, character, Some('.'))
            .expect("path completion should recover through imports");
    let completion_names = completions
        .iter()
        .map(|item| item.name.as_str())
        .collect::<BTreeSet<_>>();
    assert!(!completion_names.is_empty());

    let enum_completion_source = r#"import pkg.user

def main() -> int32:
    outcome = pkg.user.Outcome.
    return 0
"#;
    let (line, character) = line_and_character(enum_completion_source, "pkg.user.Outcome.");
    let enum_completions = complete_path_source(
        &main_path,
        enum_completion_source,
        line,
        character,
        Some('.'),
    )
    .expect("qualified imported enum completion should recover through the module namespace");
    assert_eq!(
        enum_completions
            .iter()
            .find(|completion| completion.name == "Ready")
            .map(|completion| completion.detail.as_str()),
        Some("Ready(code: own int32, reason: own str) -> pkg.user.Outcome")
    );

    let output = run_path(&main_path).expect("package program should run");
    let mir = lower_path_to_mir(&main_path).expect("package program should lower to MIR");
    let mir_output = run_mir(&mir).expect("package program MIR should run");
    assert_eq!(output.stdout, "Ada\nAda\n");
    assert_eq!(mir_output.stdout, output.stdout);
    assert!(!program.module.items.is_empty());

    let object =
        emit_host_native_object(&mir).expect("package program should emit a native object");
    assert!(!object.is_empty());
}

#[test]
fn maintained_example_subset_runs_via_public_entrypoints_and_direct_codegen() {
    let root = repo_root();
    let examples = [
        "examples/collections/slices.au",
        "examples/collections/list_polish.au",
        "examples/collections/dict_basics.au",
        "examples/collections/set_basics.au",
        "examples/numbers/numeric_builtins.au",
        "examples/strings/string_methods.au",
        "examples/strings/string_parsing_and_formatting.au",
        "examples/concurrency/task_group_start.au",
        "examples/concurrency/queue_timeout.au",
        "examples/concurrency/queue_get_timeout_named.au",
        "examples/modules/trait_impl_imports.au",
        "examples/traits/operator_traits.au",
    ];

    for relative in examples {
        let path = root.join(relative);
        let source = fs::read_to_string(&path).expect("example source should exist");
        let analysis = analyze_path_source(&path, &source);
        assert!(
            analysis.diagnostics.is_empty(),
            "maintained example analysis should stay clean for {}: {:?}",
            path.display(),
            analysis.diagnostics
        );

        let output = run_path(&path).expect("maintained example should run");
        let mir = lower_path_to_mir(&path).expect("maintained example should lower to MIR");
        let mir_output = run_mir(&mir).expect("maintained example MIR should run");
        assert_eq!(mir_output.stdout, output.stdout, "{}", path.display());

        let object = emit_host_native_object(&mir).unwrap_or_else(|error| {
            panic!(
                "maintained example should emit a native object for {}: {error}",
                path.display()
            )
        });
        assert!(!object.is_empty(), "{}", path.display());
    }
}

#[test]
fn json_semantics_public_analysis_exposes_canonical_enum_identity_and_variant_payloads() {
    let source = r#"
import json

def describe(value: own json.Value) -> str:
    match own value:
        case json.Value.String(text):
            return text
        case json.Value.Object(entries):
            return entries.len().to_string()
        case json.Value.Null:
            return "null"
        case _:
            return "other"

def main() -> int32:
    print(describe(json.Value.String("aura")))
    return 0
"#;

    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "valid JSON source should analyze cleanly: {:?}",
        analysis.diagnostics
    );
    assert!(
        analysis
            .occurrences
            .iter()
            .any(|occurrence| occurrence.hover == "```aura\nenum json.Value\n```"),
        "json.Value occurrences should expose their canonical module-qualified identity"
    );
    assert!(
        analysis.occurrences.iter().any(|occurrence| {
            occurrence.hover == "```aura\nvariant String(own str) -> json.Value\n```"
        }),
        "json.Value.String occurrences should expose their owned payload and canonical result type"
    );
    assert!(
        analysis.occurrences.iter().any(|occurrence| {
            occurrence.hover
                == "```aura\nvariant Object(own dict[str, json.Value]) -> json.Value\n```"
        }),
        "json.Value.Object occurrences should retain the recursive canonical payload type"
    );

    let completion_source = r#"
import json

def main() -> int32:
    value = json.Value.
    return 0
"#;
    let (line, character) = line_and_character(completion_source, "json.Value.");
    let completions = complete_source(completion_source, line, character, Some('.'))
        .expect("qualified JSON enum completion should recover after a dangling dot");
    assert_eq!(
        completions
            .iter()
            .find(|completion| completion.name == "String")
            .map(|completion| completion.detail.as_str()),
        Some("String(own str) -> json.Value")
    );
    assert_eq!(
        completions
            .iter()
            .find(|completion| completion.name == "Object")
            .map(|completion| completion.detail.as_str()),
        Some("Object(own dict[str, json.Value]) -> json.Value")
    );

    let method_completion_source = r#"
class Document:
    content: str

    def render(self) -> str:
        self.

def main() -> int32:
    return 0
"#;
    let (line, character) = line_and_character(method_completion_source, "self.");
    let method_completions = complete_source(method_completion_source, line, character, Some('.'))
        .expect("member completion should recover the enclosing method and self binding");
    assert!(
        method_completions
            .iter()
            .any(|completion| completion.name == "content" && completion.kind == "field"),
        "self completion inside a method should include the receiver's fields"
    );
    assert!(
        method_completions
            .iter()
            .any(|completion| completion.name == "render" && completion.kind == "method"),
        "self completion inside a method should include the receiver's methods"
    );

    let output = run_source(source).expect("canonical JSON analysis source should execute");
    assert_eq!(output.stdout, "aura\n");
    let mir = lower_source_to_mir(source).expect("canonical JSON analysis source should lower");
    let mir_output = run_mir(&mir).expect("canonical JSON analysis MIR should execute");
    assert_eq!(mir_output.stdout, output.stdout);
    assert!(!emit_host_native_object(&mir)
        .expect("canonical JSON analysis source should emit a native object")
        .is_empty());
}

#[test]
fn json_semantics_public_entrypoints_preserve_nested_noncopy_moves_across_value_contexts() {
    let source = r#"
import json

class Holder:
    value: json.Value
    sibling: str

enum Inner:
    Text(json.Value)

enum Outer:
    Wrapped(Inner)
    Empty

def take_string_value(value: own json.Value) -> str:
    match own value:
        case json.Value.String(text):
            return text
        case _:
            return "not-string"

def extract(value: own Outer) -> str:
    return match own value:
        case Outer.Wrapped(Inner.Text(json.Value.String(text))): text
        case Outer.Wrapped(Inner.Text(_)): "not-string"
        case Outer.Empty: "empty"

def main() -> int32:
    labels = {"json", "ownership", "parity"}
    empty: set[str] = {}
    print(labels.len())
    print(empty.is_empty())

    holder = Holder(value=json.Value.String("moved"), sibling="preserved")
    print(take_string_value(holder.value))
    print(holder.sibling)

    print(extract(Outer.Wrapped(Inner.Text(json.Value.String("nested")))))

    array = json.Value.Array([json.Value.Int(1), json.Value.Bool(true)])
    print(json.dumps(array))
    object = json.Value.Object({"z": json.Value.Null, "a": json.Value.String("first")})
    print(json.dumps(object))
    return 0
"#;
    let expected = concat!(
        "3\n",
        "true\n",
        "moved\n",
        "preserved\n",
        "nested\n",
        "[1,true]\n",
        "{\"a\":\"first\",\"z\":null}\n",
    );

    check_source(source).expect("nested non-copy JSON source should type-check");
    let output = run_source(source).expect("nested non-copy JSON source should execute");
    assert_eq!(output.stdout, expected);

    let mir = lower_source_to_mir(source).expect("nested non-copy JSON source should lower");
    let mir_output = run_mir(&mir).expect("nested non-copy JSON MIR should execute");
    assert_eq!(mir_output.stdout, expected);
    assert!(!emit_host_native_object(&mir)
        .expect("nested non-copy JSON source should emit a native object")
        .is_empty());
}

#[test]
fn adr0038_public_entrypoints_preserve_views_closure_loans_and_backend_lowering() {
    let source = r#"
class Pair:
    left: int64
    right: int64

def choose(pair: mut Pair, choose_left: bool) -> view mut int64 from pair:
    if choose_left:
        return view mut pair.left
    return view mut pair.right

def main() -> int32:
    mut pair = Pair(left=1, right=2)
    view mut selected = choose(pair, false)
    selected = 9
    print(selected)

    mut values = [1]
    mut update: def(int64) -> None = lambda [mut values] item: values.append(item)
    update(2)
    print(values[1])
    return 0
"#;

    let analysis = analyze_source(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    assert!(analysis
        .occurrences
        .iter()
        .any(|occurrence| occurrence.hover.contains("view mut selected: int64")));

    check_source(source).expect("ADR-0038 public source should type-check");
    let output = run_source(source).expect("ADR-0038 public source should execute through MIR");
    assert_eq!(output.stdout, "9\n2\n");

    let mir = lower_source_to_mir(source).expect("ADR-0038 public source should lower to MIR");
    let instructions = mir
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert!(instructions
        .iter()
        .any(|instruction| matches!(instruction, Instruction::BeginReturnedLoan { .. })));
    assert!(instructions
        .iter()
        .any(|instruction| matches!(instruction, Instruction::WriteLoan { .. })));
    assert!(instructions
        .iter()
        .any(|instruction| matches!(instruction, Instruction::ReturnLoan { .. })));
    assert!(!emit_host_native_object(&mir)
        .expect("ADR-0038 MIR should emit a direct-backend object")
        .is_empty());

    let unstable = check_source("def main():\n    view item = 1\n")
        .expect_err("view bindings require addressable places");
    assert_eq!(unstable.code, "AU3004");

    let immutable_closure = check_source(
        "def main():\n    mut values = [1]\n    update: def(int64) -> None = lambda [mut values] item: values.append(item)\n    update(2)\n",
    )
    .expect_err("mutable-repeatable closures require mutable closure places");
    assert_eq!(immutable_closure.code, "AU3003");
}
