use super::{
    absolutize, analyze_path_source, builtin_imports, canonicalize_if_exists, check_path,
    check_path_with_source, check_source, emit_host_native_object, exported_binding,
    exported_namespace, find_type_namespace_path, import_exists_from_root, infer_package_root,
    insert_namespace_import, is_builtin_export_type, local_item_exists, logical_module_name,
    lower_path_to_checked_mir, lower_path_to_mir, lower_path_with_source_to_mir,
    lower_source_to_mir, parse_source, qualify_enum_decl_for_export, qualify_export_bounds,
    qualify_export_type, qualify_export_type_ref, qualify_impl_decl_for_export,
    qualify_imported_module_namespaces, run_checked_mir_entry_with_stdout_sink_and_program_args,
    run_mir, run_path, run_path_entry_with_stdout_sink_and_program_args, run_path_with_source,
    run_path_with_source_and_stdout_sink, run_path_with_source_and_stdout_sink_and_program_args,
    run_path_with_stdout_sink, run_path_with_stdout_sink_and_program_args, run_serialized_mir,
    run_source, run_source_with_stdout_sink, sha256_hex, update_git_dependencies_in_working_dir,
    ModuleLoader, StdoutSink, Value,
};
use crate::ast::TypeRef;
use crate::diag::Span;
use crate::integer::IntegerValue;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

const POINT_SOURCE: &str = include_str!("../../../examples/point.au");
const BASIC_ADDITION_SOURCE: &str = include_str!("../../../examples/basic_addition.au");
const TOP_LEVEL_ADDITION_SOURCE: &str = include_str!("../../../examples/top_level_addition.au");
const CONTROL_FLOW_SOURCE: &str = include_str!("../../../examples/control_flow.au");
static IO_EXAMPLE_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn public_sha256_hex_is_lowercase_and_preserves_leading_zeroes() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"Aura"),
        "224ea01e4a299102cf8da1698a931bad291415dcefea7493576c17cf1fa960b9"
    );
    assert_eq!(sha256_hex(&[0]).len(), 64);
}

fn lock_io_example() -> std::sync::MutexGuard<'static, ()> {
    IO_EXAMPLE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn hosted_ci_timing_policy_preserves_local_limits_and_scales_runner_limits() {
    let local = StdDuration::from_millis(125);
    assert_eq!(crate::timing_limit_for_hosted_ci(local, false), local);
    assert_eq!(
        crate::timing_limit_for_hosted_ci(local, true),
        StdDuration::from_millis(500)
    );
}

fn captured_stdout_sink() -> (Arc<Mutex<String>>, StdoutSink) {
    let captured = Arc::new(Mutex::new(String::new()));
    let sink_capture = captured.clone();
    let sink: StdoutSink = Arc::new(move |chunk| {
        sink_capture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_str(chunk);
    });
    (captured, sink)
}

#[test]
fn public_path_entry_and_dependency_update_facades_preserve_results() {
    let temp = TempDir::new("aura-public-path-facades");
    fs::create_dir_all(temp.path().join("src")).expect("package source directory should exist");
    fs::write(
        temp.path().join("Aura.toml"),
        "[package]\nname = \"facades\"\nversion = \"0.3.0\"\nedition = \"2026\"\n",
    )
    .expect("package manifest should be written");
    let entry_path = temp.path().join("src/main.au");
    fs::write(
        &entry_path,
        concat!(
            "import sys\n\n",
            "def selected() -> int64:\n",
            "    print(\"selected\")\n",
            "    print(sys.args().len())\n",
            "    return 17\n\n",
            "def main() -> int32:\n",
            "    return 0\n",
        ),
    )
    .expect("package entry should be written");

    let (captured, sink) = captured_stdout_sink();
    let output = run_path_entry_with_stdout_sink_and_program_args(
        &entry_path,
        Some("selected"),
        Some(sink),
        vec!["first".to_string(), "second".to_string()],
    )
    .expect("the public path facade should execute the selected entry");
    assert_eq!(output.stdout, "selected\n2\n");
    assert_eq!(output.value, Value::Int(IntegerValue::from_signed(17)));
    assert_eq!(
        captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_str(),
        "selected\n2\n"
    );

    let update = update_git_dependencies_in_working_dir(temp.path(), None)
        .expect("the public update facade should write a lock for the package");
    assert!(update.updated_packages.is_empty());
    assert_eq!(
        update.lockfile_root,
        fs::canonicalize(temp.path()).expect("package root should canonicalize")
    );
    let lockfile = fs::read_to_string(temp.path().join("Aura.lock"))
        .expect("the public update facade should write Aura.lock");
    assert!(lockfile.contains("name = \"facades\""));
}

#[test]
fn source_only_public_apis_reject_unmanifested_ffi() {
    let source = "extern \"C\" def getpid() -> int32\n";
    for (api, result) in [
        ("check_source", check_source(source).map(|_| ())),
        (
            "lower_source_to_mir",
            lower_source_to_mir(source).map(|_| ()),
        ),
        ("run_source", run_source(source).map(|_| ())),
    ] {
        let diagnostic = result.expect_err("source-only FFI must require a package manifest");
        assert_eq!(diagnostic.code, "AU2999", "{api}: {diagnostic:?}");
        assert!(
            diagnostic.message.contains("Aura.toml")
                && diagnostic.message.contains("allow_ffi = true"),
            "{api}: {}",
            diagnostic.message
        );
    }
}

#[test]
fn exported_bindings_preserve_public_opaque_handle_identity_and_privacy() {
    let module = parse_source(
        "public extern \"C\" opaque class PublicHandle\nextern \"C\" opaque class PrivateHandle\n",
    )
    .expect("opaque handle declarations should parse");
    let program = super::check_module_with_builtin_imports(module)
        .expect("opaque handle declarations should type check");

    match exported_binding(&program, "PublicHandle").expect("public opaque handle should export") {
        crate::sema::ImportedBinding::OpaqueHandle(info) => {
            assert_eq!(info.module_name, "<main>");
            assert_eq!(info.decl.name, "PublicHandle");
            assert!(info.decl.public);
        }
        other => panic!("expected opaque handle export, found {other:?}"),
    }
    assert!(
        exported_binding(&program, "PrivateHandle").is_none(),
        "private opaque handles must not become import bindings"
    );
}

#[cfg(unix)]
#[test]
fn manifest_authorized_path_run_uses_the_trusted_ffi_runtime_route() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/packages/ffi_getpid/src/main.au");
    let output = run_path(&path).expect("the maintained FFI package should run by path");
    assert_eq!(output.stdout, "true\n");
    assert_eq!(output.value, Value::Int(IntegerValue::from_signed(0)));
}

#[cfg(unix)]
#[test]
fn runtime_wrapper_matrix_preserves_sinks_args_entries_and_manifest_authorized_ffi() {
    let (source_capture, source_sink) = captured_stdout_sink();
    let source_output =
        run_source_with_stdout_sink("def main():\n    print(\"source-safe\")\n", source_sink)
            .expect("source-only stdout wrapper should run ordinary safe source");
    assert_eq!(source_output.stdout, "source-safe\n");
    assert_eq!(
        source_capture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_str(),
        "source-safe\n"
    );

    let temp = TempDir::new("aura-runtime-wrapper-matrix");
    fs::write(
        temp.path().join("Aura.toml"),
        r#"[package]
name = "runtime_wrapper_matrix"
version = "0.1.0"
edition = "2026"
allow_ffi = true
"#,
    )
    .expect("test package manifest should be written");
    fs::create_dir_all(temp.path().join("src")).expect("test package source dir should be created");
    let main_path = temp.path().join("src/main.au");
    fs::write(
        &main_path,
        r#"import sys

extern "C" def getpid() -> int32

def selected():
    print(getpid() > 0)
    print(sys.args().len())

def main():
    print(getpid() > 0)
    print(sys.args().len())
"#,
    )
    .expect("test package entry should be written");
    let override_source = r#"import sys

extern "C" def getpid() -> int32

def main():
    print(getpid() > 0)
    print(sys.args().len())
"#;

    let overridden = run_path_with_source(&main_path, override_source)
        .expect("manifest-authorized source override should execute FFI");
    assert_eq!(overridden.stdout, "true\n0\n");

    let (override_capture, override_sink) = captured_stdout_sink();
    let overridden_with_sink =
        run_path_with_source_and_stdout_sink(&main_path, override_source, override_sink)
            .expect("manifest-authorized source override should preserve its stdout sink");
    assert_eq!(overridden_with_sink.stdout, "true\n0\n");
    assert_eq!(
        override_capture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_str(),
        "true\n0\n"
    );

    let (override_args_capture, override_args_sink) = captured_stdout_sink();
    let overridden_with_args = run_path_with_source_and_stdout_sink_and_program_args(
        &main_path,
        override_source,
        override_args_sink,
        vec!["alpha".to_string(), "beta".to_string()],
    )
    .expect("manifest-authorized source override should receive explicit program args");
    assert_eq!(overridden_with_args.stdout, "true\n2\n");
    assert_eq!(
        override_args_capture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_str(),
        "true\n2\n"
    );

    let (path_capture, path_sink) = captured_stdout_sink();
    let path_output = run_path_with_stdout_sink(&main_path, path_sink)
        .expect("manifest-authorized path should preserve its stdout sink");
    assert_eq!(path_output.stdout, "true\n0\n");
    assert_eq!(
        path_capture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_str(),
        "true\n0\n"
    );

    let (path_args_capture, path_args_sink) = captured_stdout_sink();
    let path_with_args = run_path_with_stdout_sink_and_program_args(
        &main_path,
        path_args_sink,
        vec!["one".to_string(), "two".to_string(), "three".to_string()],
    )
    .expect("manifest-authorized path should receive explicit program args");
    assert_eq!(path_with_args.stdout, "true\n3\n");
    assert_eq!(
        path_args_capture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_str(),
        "true\n3\n"
    );

    let (entry_capture, entry_sink) = captured_stdout_sink();
    let selected = run_path_entry_with_stdout_sink_and_program_args(
        &main_path,
        Some("selected"),
        Some(entry_sink),
        vec!["entry".to_string()],
    )
    .expect("manifest-authorized selected entry should execute FFI with explicit args");
    assert_eq!(selected.stdout, "true\n1\n");
    assert_eq!(
        entry_capture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_str(),
        "true\n1\n"
    );

    let checked_mir = lower_path_to_checked_mir(&main_path)
        .expect("manifest checking should produce an opaque trusted MIR module");
    let first = run_checked_mir_entry_with_stdout_sink_and_program_args(
        &checked_mir,
        Some("selected"),
        None,
        vec!["first".to_string()],
    )
    .expect("checked MIR should preserve manifest-authorized FFI for a selected entry");
    let second = run_checked_mir_entry_with_stdout_sink_and_program_args(
        &checked_mir,
        Some("main"),
        None,
        vec!["one".to_string(), "two".to_string()],
    )
    .expect("the same checked MIR should be reusable for another selected entry");
    assert_eq!(first.stdout, "true\n1\n");
    assert_eq!(second.stdout, "true\n2\n");
}

const EXAMPLE_CASES: &[(&str, &str)] = &[
    (
        "examples/basics/top_level_script.au",
        include_str!("../../../examples/basics/top_level_script.au"),
    ),
    (
        "examples/basics/main_function.au",
        include_str!("../../../examples/basics/main_function.au"),
    ),
    (
        "examples/basics/mutable_bindings.au",
        include_str!("../../../examples/basics/mutable_bindings.au"),
    ),
    (
        "examples/basics/default_arguments.au",
        include_str!("../../../examples/basics/default_arguments.au"),
    ),
    (
        "examples/basics/pass_keyword.au",
        include_str!("../../../examples/basics/pass_keyword.au"),
    ),
    (
        "examples/classes/point_distance.au",
        include_str!("../../../examples/classes/point_distance.au"),
    ),
    (
        "examples/classes/default_fields.au",
        include_str!("../../../examples/classes/default_fields.au"),
    ),
    (
        "examples/classes/methods.au",
        include_str!("../../../examples/classes/methods.au"),
    ),
    (
        "examples/control_flow/if_elif_else.au",
        include_str!("../../../examples/control_flow/if_elif_else.au"),
    ),
    (
        "examples/control_flow/for_range.au",
        include_str!("../../../examples/control_flow/for_range.au"),
    ),
    (
        "examples/control_flow/while_break_continue.au",
        include_str!("../../../examples/control_flow/while_break_continue.au"),
    ),
    (
        "examples/enums/result_match.au",
        include_str!("../../../examples/enums/result_match.au"),
    ),
    (
        "examples/enums/result_option.au",
        include_str!("../../../examples/enums/result_option.au"),
    ),
    (
        "examples/enums/explicit_type_args.au",
        include_str!("../../../examples/enums/explicit_type_args.au"),
    ),
    (
        "examples/generics/box_and_wrapper.au",
        include_str!("../../../examples/generics/box_and_wrapper.au"),
    ),
    (
        "examples/traits/greeter.au",
        include_str!("../../../examples/traits/greeter.au"),
    ),
    (
        "examples/traits/multiple_bounds.au",
        include_str!("../../../examples/traits/multiple_bounds.au"),
    ),
    (
        "examples/numbers/float_sqrt.au",
        include_str!("../../../examples/numbers/float_sqrt.au"),
    ),
    (
        "examples/numbers/float32_values.au",
        include_str!("../../../examples/numbers/float32_values.au"),
    ),
    (
        "examples/numbers/numeric_casts.au",
        include_str!("../../../examples/numbers/numeric_casts.au"),
    ),
    (
        "examples/strings/greeting.au",
        include_str!("../../../examples/strings/greeting.au"),
    ),
    (
        "examples/concurrency/task_group_queue_sum.au",
        include_str!("../../../examples/concurrency/task_group_queue_sum.au"),
    ),
    (
        "examples/concurrency/task_group_cancel.au",
        include_str!("../../../examples/concurrency/task_group_cancel.au"),
    ),
    (
        "examples/concurrency/queue_get_timeout.au",
        include_str!("../../../examples/concurrency/queue_get_timeout.au"),
    ),
    (
        "examples/concurrency/sleep_builtin.au",
        include_str!("../../../examples/concurrency/sleep_builtin.au"),
    ),
    (
        "examples/concurrency/send_result.au",
        include_str!("../../../examples/concurrency/send_result.au"),
    ),
    (
        "examples/concurrency/bounded_queue.au",
        include_str!("../../../examples/concurrency/bounded_queue.au"),
    ),
    (
        "examples/concurrency/task_group_start_soon.au",
        include_str!("../../../examples/concurrency/task_group_start_soon.au"),
    ),
    (
        "examples/concurrency/queue_put_timeout.au",
        include_str!("../../../examples/concurrency/queue_put_timeout.au"),
    ),
    (
        "examples/enums/wildcard_match.au",
        include_str!("../../../examples/enums/wildcard_match.au"),
    ),
    (
        "examples/generics/generic_method_calls.au",
        include_str!("../../../examples/generics/generic_method_calls.au"),
    ),
    (
        "examples/generics/bounded_types.au",
        include_str!("../../../examples/generics/bounded_types.au"),
    ),
    (
        "examples/traits/marker_trait.au",
        include_str!("../../../examples/traits/marker_trait.au"),
    ),
    (
        "examples/traits/specialized_generic_impl.au",
        include_str!("../../../examples/traits/specialized_generic_impl.au"),
    ),
    (
        "examples/concurrency/minute_duration.au",
        include_str!("../../../examples/concurrency/minute_duration.au"),
    ),
    (
        "examples/traits/generic_dispatch_multiple_types.au",
        include_str!("../../../examples/traits/generic_dispatch_multiple_types.au"),
    ),
    (
        "examples/strings/string_methods.au",
        include_str!("../../../examples/strings/string_methods.au"),
    ),
    (
        "examples/numbers/numeric_builtins.au",
        include_str!("../../../examples/numbers/numeric_builtins.au"),
    ),
    (
        "examples/collections/dict_basics.au",
        include_str!("../../../examples/collections/dict_basics.au"),
    ),
    (
        "examples/collections/set_basics.au",
        include_str!("../../../examples/collections/set_basics.au"),
    ),
    (
        "examples/strings/string_parsing_and_formatting.au",
        include_str!("../../../examples/strings/string_parsing_and_formatting.au"),
    ),
    (
        "examples/traits/generic_trait_bounds.au",
        include_str!("../../../examples/traits/generic_trait_bounds.au"),
    ),
    (
        "examples/traits/operator_traits.au",
        include_str!("../../../examples/traits/operator_traits.au"),
    ),
    (
        "examples/traits/ordering_traits.au",
        include_str!("../../../examples/traits/ordering_traits.au"),
    ),
    (
        "examples/basics/copy_return_selection.au",
        include_str!("../../../examples/basics/copy_return_selection.au"),
    ),
    (
        "examples/io/process_run.au",
        include_str!("../../../examples/io/process_run.au"),
    ),
    (
        "examples/io/process_pipes.au",
        include_str!("../../../examples/io/process_pipes.au"),
    ),
];
const ADDITIONAL_EXAMPLE_CASES: &[(&str, &str, &str)] = &[
    (
        "examples/basic_addition.au",
        include_str!("../../../examples/basic_addition.au"),
        "16\n",
    ),
    (
        "examples/bytes/codecs_and_hashing.au",
        include_str!("../../../examples/bytes/codecs_and_hashing.au"),
        "4175726120f09f8c8c\nAura 🌌\nAAH+/w==\n0001feff\nba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n4175726120f09f8c8c\n[0, 1, 254, 255]\n",
    ),
    (
        "examples/basics/assertions.au",
        include_str!("../../../examples/basics/assertions.au"),
        "checking\nall assertions passed\n",
    ),
    (
        "examples/basics/multiline_expressions.au",
        include_str!("../../../examples/basics/multiline_expressions.au"),
        "80\n20\n",
    ),
    (
        "examples/control_flow/conditional_expressions.au",
        include_str!("../../../examples/control_flow/conditional_expressions.au"),
        "ready\nhigh\nmid\nlow\n",
    ),
    (
        "examples/basics/borrow_parameters.au",
        include_str!("../../../examples/basics/borrow_parameters.au"),
        "41\n42\n42\n",
    ),
    (
        "examples/basics/named_arguments.au",
        include_str!("../../../examples/basics/named_arguments.au"),
        "hello, aura\n7\n",
    ),
    (
        "examples/basics/named_builtin_arguments.au",
        include_str!("../../../examples/basics/named_builtin_arguments.au"),
        "10\n",
    ),
    (
        "examples/basics/none_values.au",
        include_str!("../../../examples/basics/none_values.au"),
        "1\n",
    ),
    (
        "examples/basics/simple_example.au",
        include_str!("../../../examples/basics/simple_example.au"),
        "Ayoola Olafenwa\n834.6\n",
    ),
    (
        "examples/classes/copy_class.au",
        include_str!("../../../examples/classes/copy_class.au"),
        "1\n2\n",
    ),
    (
        "examples/classes/indirect_recursive.au",
        include_str!("../../../examples/classes/indirect_recursive.au"),
        "2\n",
    ),
    (
        "examples/classes/mutating_methods.au",
        include_str!("../../../examples/classes/mutating_methods.au"),
        "6\n1\n",
    ),
    (
        "examples/collections/list_basics.au",
        include_str!("../../../examples/collections/list_basics.au"),
        "3\n1\n2\n2\n20\n1\n99\nfalse\n",
    ),
    (
        "examples/collections/list_iteration.au",
        include_str!("../../../examples/collections/list_iteration.au"),
        "Ada\nGrace\n2\n9\n",
    ),
    (
        "examples/collections/list_polish.au",
        include_str!("../../../examples/collections/list_polish.au"),
        "Ada\nGrace\ntrue\n4\n1\n14\n13\n12\n11\ntrue\n100\ntrue\ntrue\n",
    ),
    (
        "examples/collections/slices.au",
        include_str!("../../../examples/collections/slices.au"),
        "[20, 30]\n[10, 20]\n[30, 40]\n[10, 20, 30, 40]\n[10, 20, 30, 40]\n[99, 30]\n🎉\nA🎉\n🎉Z\nA🎉Z\nA🎉Z\n",
    ),
    (
        "examples/concurrency/queue_iteration.au",
        include_str!("../../../examples/concurrency/queue_iteration.au"),
        "1\n2\n",
    ),
    (
        "examples/concurrency/task_group_start.au",
        include_str!("../../../examples/concurrency/task_group_start.au"),
        "2\n4\n6\n",
    ),
    (
        "examples/concurrency/queue_timeout.au",
        include_str!("../../../examples/concurrency/queue_timeout.au"),
        "timeout\n",
    ),
    (
        "examples/concurrency/bounded_queue.au",
        include_str!("../../../examples/concurrency/bounded_queue.au"),
        "queued 1\nqueued 2\n3\n",
    ),
    (
        "examples/concurrency/queue_get_timeout_named.au",
        include_str!("../../../examples/concurrency/queue_get_timeout_named.au"),
        "Option.None\n",
    ),
    (
        "examples/control_flow.au",
        include_str!("../../../examples/control_flow.au"),
        "ok\n",
    ),
    (
        "examples/control_flow/boolean_logic.au",
        include_str!("../../../examples/control_flow/boolean_logic.au"),
        "ready\ntrue\n",
    ),
    (
        "examples/control_flow/match_literals.au",
        include_str!("../../../examples/control_flow/match_literals.au"),
        "negative\nzero\nmany\nyes\nno\nrepo\nother\n",
    ),
    (
        "examples/enums/match_borrow.au",
        include_str!("../../../examples/enums/match_borrow.au"),
        "ok\n",
    ),
    (
        "examples/error_handling/try_result.au",
        include_str!("../../../examples/error_handling/try_result.au"),
        "6\ndivision by zero\n",
    ),
    (
        "examples/generics/generic_constructor_specialization.au",
        include_str!("../../../examples/generics/generic_constructor_specialization.au"),
        "42\n",
    ),
    (
        "examples/io/tcp_echo.au",
        include_str!("../../../examples/io/tcp_echo.au"),
        "echo:ping\n",
    ),
    (
        "examples/io/bytes_file_io.au",
        include_str!("../../../examples/io/bytes_file_io.au"),
        "4\n65\n67\n5\n68\n",
    ),
    (
        "examples/io/tcp_bytes.au",
        include_str!("../../../examples/io/tcp_bytes.au"),
        "4\n116\n",
    ),
    (
        "examples/io/udp_echo.au",
        include_str!("../../../examples/io/udp_echo.au"),
        "udp:ping\nping\n",
    ),
    (
        "examples/io/http_roundtrip.au",
        include_str!("../../../examples/io/http_roundtrip.au"),
        "200\nPOST:/hello:body:ok\n",
    ),
    (
        "examples/io/websocket_roundtrip.au",
        include_str!("../../../examples/io/websocket_roundtrip.au"),
        "ws:hi\n",
    ),
    (
        "examples/numbers/uint128_values.au",
        include_str!("../../../examples/numbers/uint128_values.au"),
        "340282366920938463463374607431768211455\n340282366920938463463374607431768211455\n",
    ),
    (
        "examples/numbers/unary_minus.au",
        include_str!("../../../examples/numbers/unary_minus.au"),
        "-5\n-3.5\n2\n",
    ),
    (
        "examples/point.au",
        include_str!("../../../examples/point.au"),
        "5.0\n",
    ),
    (
        "examples/simple_addition.au",
        include_str!("../../../examples/simple_addition.au"),
        "156\n",
    ),
    (
        "examples/strings/borrow_str.au",
        include_str!("../../../examples/strings/borrow_str.au"),
        "Hello, Aura\n",
    ),
    (
        "examples/strings/f_strings.au",
        include_str!("../../../examples/strings/f_strings.au"),
        "Hello, Aura 42\n",
    ),
    (
        "examples/strings/string_clone.au",
        include_str!("../../../examples/strings/string_clone.au"),
        "aura\n",
    ),
    (
        "examples/top_level_addition.au",
        include_str!("../../../examples/top_level_addition.au"),
        "16\n",
    ),
    (
        "examples/traits/generic_trait_impl.au",
        include_str!("../../../examples/traits/generic_trait_impl.au"),
        "11\n",
    ),
    (
        "examples/traits/specialized_trait_dispatch.au",
        include_str!("../../../examples/traits/specialized_trait_dispatch.au"),
        "7\nhi\n",
    ),
    (
        "examples/traits/trait_associated_factory.au",
        include_str!("../../../examples/traits/trait_associated_factory.au"),
        "7\n",
    ),
    (
        "examples/traits/builtin_target_traits.au",
        include_str!("../../../examples/traits/builtin_target_traits.au"),
        "list of 2\ntext of 5\n",
    ),
    (
        "examples/control_flow/membership_and_chains.au",
        include_str!("../../../examples/control_flow/membership_and_chains.au"),
        "true\ntrue\ntrue\ntrue\ntrue\ntrue\nfalse\n",
    ),
    (
        "examples/control_flow/enumerate_and_zip.au",
        include_str!("../../../examples/control_flow/enumerate_and_zip.au"),
        "0: alpha\n1: beta\nalpha:80\nbeta:443\n3\n",
    ),
    (
        "examples/basics/len_and_str.au",
        include_str!("../../../examples/basics/len_and_str.au"),
        "2\n4\n2\n[alpha, beta]\n",
    ),
];

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

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn zero_exit_value() -> Value {
    Value::Int(crate::integer::IntegerValue::zero())
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("compiler crate should live under repo root")
        .to_path_buf()
}

fn run_with_large_stack<T, F>(operation: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(operation)
        .expect("large-stack helper thread should spawn")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

enum TimedCaseOutcome<T> {
    Completed(T),
    Panicked,
}

fn run_corpus_case_with_timeout<T, F>(
    operation: &str,
    path: &std::path::Path,
    action: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let timeout = corpus_case_timeout_for_environment(
        cfg!(coverage),
        std::env::var_os("GITHUB_ACTIONS").is_some(),
    );
    run_corpus_case_with_timeout_duration(operation, path, timeout, action)
}

fn corpus_case_timeout_for_environment(coverage: bool, hosted_ci: bool) -> StdDuration {
    let local = StdDuration::from_secs(10);
    if coverage || hosted_ci {
        local.saturating_mul(4)
    } else {
        local
    }
}

fn run_corpus_case_with_timeout_duration<T, F>(
    operation: &str,
    path: &std::path::Path,
    timeout: StdDuration,
    action: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let handle = thread::Builder::new()
        .name(format!("aura-corpus-{operation}"))
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let outcome = match catch_unwind(AssertUnwindSafe(action)) {
                Ok(value) => TimedCaseOutcome::Completed(value),
                Err(_) => TimedCaseOutcome::Panicked,
            };
            let _ = sender.send(outcome);
        })
        .map_err(|error| {
            format!(
                "failed to start {operation} for {}: {error}",
                path.display()
            )
        })?;

    match receiver.recv_timeout(timeout) {
        Ok(TimedCaseOutcome::Completed(value)) => {
            handle.join().map_err(|_| {
                format!(
                    "{operation} helper thread panicked after reporting completion for {}",
                    path.display()
                )
            })?;
            Ok(value)
        }
        Ok(TimedCaseOutcome::Panicked) => {
            let _ = handle.join();
            Err(format!("{operation} panicked for {}", path.display()))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
            "{operation} timed out after {} milliseconds for {}",
            timeout.as_millis(),
            path.display()
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(format!(
            "{operation} helper disconnected for {}",
            path.display()
        )),
    }
}

#[test]
fn corpus_case_timeout_reports_the_operation_and_path() {
    let path = PathBuf::from("slow.au");
    let error = run_corpus_case_with_timeout_duration(
        "runtime probe",
        &path,
        StdDuration::from_millis(10),
        || thread::sleep(StdDuration::from_millis(100)),
    )
    .expect_err("slow corpus operation should time out");
    assert!(error.contains("runtime probe timed out"));
    assert!(error.contains("slow.au"));
}

#[test]
fn corpus_case_timeout_scales_for_coverage_and_hosted_ci() {
    assert_eq!(
        corpus_case_timeout_for_environment(false, false),
        StdDuration::from_secs(10)
    );
    assert_eq!(
        corpus_case_timeout_for_environment(true, false),
        StdDuration::from_secs(40)
    );
    assert_eq!(
        corpus_case_timeout_for_environment(false, true),
        StdDuration::from_secs(40)
    );
    assert_eq!(
        corpus_case_timeout_for_environment(true, true),
        StdDuration::from_secs(40)
    );
}

fn escape_aura_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "linux")]
fn current_process_thread_count() -> usize {
    fs::read_dir("/proc/self/task")
        .expect("linux thread directory should exist")
        .count()
}

#[cfg(target_os = "macos")]
fn current_process_thread_count() -> usize {
    let output = Command::new("ps")
        .args(["-M", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps should report process threads");
    assert!(
        output.status.success(),
        "ps should succeed when counting threads"
    );
    let stdout = String::from_utf8(output.stdout).expect("ps output should be utf-8");
    stdout.lines().skip(1).count()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn current_process_thread_count() -> usize {
    1
}

fn type_ref(name: &str) -> TypeRef {
    TypeRef::named(name, vec![], false, crate::diag::Span::new(1, 1))
}

fn named_ref_name(type_ref: &TypeRef) -> &str {
    type_ref
        .named_parts()
        .map(|(name, _)| name)
        .expect("test expected a named type reference")
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

fn collect_aura_files_recursive(dir: &std::path::Path) -> Vec<PathBuf> {
    fn visit(dir: &std::path::Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("failed to read {}: {}", dir.display(), error))
        {
            let path = entry
                .unwrap_or_else(|error| {
                    panic!("failed to read entry under {}: {}", dir.display(), error)
                })
                .path();
            if path.is_dir() {
                visit(&path, files);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) == Some("au") {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    visit(dir, &mut files);
    files.sort();
    files
}

fn should_execute_runtime_corpus_case(path: &std::path::Path) -> bool {
    let root = repo_root();
    let relative = path
        .strip_prefix(&root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    !include_str!("../tests/runtime-corpus-exclusions.txt")
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .filter_map(|line| line.split_once('|'))
        .any(|(excluded, _reason)| excluded.trim() == relative)
}

#[test]
fn runtime_corpus_exclusions_are_explicit_not_filename_heuristics() {
    let root = repo_root();
    assert!(
        should_execute_runtime_corpus_case(&root.join("test_edge/test09_sleep.au")),
        "short sleep fixtures should execute even when their filename contains sleep"
    );
    assert!(
        !should_execute_runtime_corpus_case(&root.join("test_recheck/gap4b_minute_literal.au")),
        "the explicit one-minute fixture should remain classified as a long-running case"
    );
}

#[test]
fn path_wrapper_functions_cover_success_and_loader_error_paths() {
    let temp = TempDir::new("aura-lib-coverage");
    let main_path = temp.path().join("main.au");
    fs::write(&main_path, "def main():\n    print(1)\n").expect("failed to write main file");

    check_source(
        "from fs import exists\n\ndef main() -> None:\n    if exists(\"missing\"):\n        pass\n",
    )
    .expect("source-level builtin from imports should type-check");
    let duplicate_source_builtin = check_source("from fs import exists\nfrom fs import exists\n")
        .expect_err("duplicate source-level builtin imports should fail");
    assert!(duplicate_source_builtin
        .message
        .contains("duplicate import binding `exists`"));
    let duplicate_source_module_alias = check_source("import fs as files\nimport fs as files\n")
        .expect_err("duplicate source-level builtin module aliases should fail");
    assert!(duplicate_source_module_alias
        .message
        .contains("duplicate import binding `files`"));
    let missing_source_builtin =
        check_source("from fs import definitely_missing\n").expect_err("unknown builtin export");
    assert!(missing_source_builtin
        .message
        .contains("module `fs` has no export named `definitely_missing`"));

    let builtin_from_path = temp.path().join("builtin_from.au");
    fs::write(
        &builtin_from_path,
        "from fs import exists\n\ndef main() -> None:\n    if exists(\"missing\"):\n        pass\n",
    )
    .expect("failed to write builtin-from import file");
    check_path(&builtin_from_path).expect("path-level builtin from imports should type-check");

    let duplicate_builtin_from_path = temp.path().join("duplicate_builtin_from.au");
    fs::write(
        &duplicate_builtin_from_path,
        "from fs import exists\nfrom fs import exists\n",
    )
    .expect("failed to write duplicate builtin-from import file");
    let duplicate_builtin = check_path(&duplicate_builtin_from_path)
        .expect_err("duplicate path-level builtin from imports should fail");
    assert!(duplicate_builtin
        .message
        .contains("duplicate import binding `exists`"));

    let missing_builtin_from_path = temp.path().join("missing_builtin_from.au");
    fs::write(
        &missing_builtin_from_path,
        "from fs import definitely_missing\n",
    )
    .expect("failed to write missing builtin-from import file");
    let missing_builtin = check_path(&missing_builtin_from_path)
        .expect_err("unknown path-level builtin from imports should fail");
    assert!(missing_builtin
        .message
        .contains("module `fs` has no export named `definitely_missing`"));

    let non_builtin_from =
        parse_source("from local import Thing\n").expect("non-builtin from import should parse");
    assert!(builtin_imports(&non_builtin_from)
        .expect("non-builtin from imports should be ignored by builtin collection")
        .is_empty());

    check_path(&main_path).expect("check_path should succeed");
    check_path_with_source(&main_path, "def main():\n    print(2)\n")
        .expect("check_path_with_source should succeed");

    let path_output = run_path(&main_path).expect("run_path should succeed");
    assert_eq!(path_output.stdout, "1\n");
    let override_output = run_path_with_source(&main_path, "def main():\n    print(2)\n")
        .expect("run_path_with_source should succeed");
    assert_eq!(override_output.stdout, "2\n");

    lower_path_to_mir(&main_path).expect("lower_path_to_mir should succeed");
    lower_path_with_source_to_mir(&main_path, "def main():\n    print(4)\n")
        .expect("lower_path_with_source_to_mir should succeed");

    let cyclic_dir = TempDir::new("aura-lib-cycle");
    let a_path = cyclic_dir.path().join("a.au");
    let b_path = cyclic_dir.path().join("b.au");
    fs::write(&a_path, "import b\n\ndef main():\n    pass\n").expect("write a");
    fs::write(&b_path, "import a\n\ndef helper():\n    pass\n").expect("write b");
    let cyclic = check_path(&a_path).expect_err("cyclic imports should fail");
    assert!(cyclic.message.contains("cyclic import involving"));
}

#[test]
fn module_loader_package_qualification_ignores_paths_outside_graph_sources() {
    let temp = TempDir::new("aura-lib-package-qualification");
    let src_dir = temp.path().join("src");
    fs::create_dir_all(&src_dir).expect("failed to create package src");
    fs::write(
        temp.path().join("Aura.toml"),
        "[package]\nname = \"rootpkg\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .expect("failed to write manifest");
    let main_path = src_dir.join("main.au");
    fs::write(&main_path, "def main():\n    pass\n").expect("failed to write main module");

    let loader = ModuleLoader::new(&main_path).expect("package loader should initialize");
    let canonical_main = fs::canonicalize(&main_path).expect("main path should canonicalize");
    assert_eq!(loader.module_name_for_path(&canonical_main), "main");
    let mut program = check_source("def main():\n    pass\n").expect("program should check");
    let outside_path = temp.path().join("outside.au");
    fs::write(&outside_path, "def helper():\n    pass\n").expect("failed to write outside file");
    loader.qualify_program_imported_modules(&outside_path, &mut program);
    assert!(program.imported_modules.is_empty());
    loader.qualify_program_imported_modules(&canonical_main, &mut program);
    assert!(program.imported_modules.is_empty());
}

#[test]
fn module_constant_plan_is_dependency_first_import_ordered_and_diamond_safe() {
    let temp = TempDir::new("aura-module-constant-plan");
    let main_path = temp.path().join("main.au");
    fs::write(temp.path().join("shared.au"), "public marker = 1\n").expect("write shared module");
    fs::write(
        temp.path().join("beta.au"),
        "import shared\npublic marker = shared.marker + 1\n",
    )
    .expect("write beta module");
    fs::write(
        temp.path().join("alpha.au"),
        "import shared\npublic marker = shared.marker + 2\n",
    )
    .expect("write alpha module");
    fs::write(
        &main_path,
        "import beta\nimport alpha\nroot = beta.marker + alpha.marker\n\ndef main():\n    print(root)\n",
    )
    .expect("write entry module");

    let program = check_path(&main_path).expect("constant modules should check");
    let plan = program
        .constant_init_plan
        .iter()
        .map(|constant| format!("{}::{}", constant.module_name, constant.decl.name))
        .collect::<Vec<_>>();
    assert_eq!(
        plan,
        [
            "shared::marker",
            "beta::marker",
            "alpha::marker",
            "main::root",
        ]
    );

    let mir = lower_path_to_mir(&main_path).expect("constant modules should lower");
    assert_eq!(
        mir.constants
            .iter()
            .map(|constant| constant.key.as_str())
            .collect::<Vec<_>>(),
        plan
    );
}

#[test]
fn module_constant_plan_reports_an_entry_missing_from_the_loader_cache() {
    let temp = TempDir::new("aura-module-constant-plan-missing-cache");
    let main_path = temp.path().join("main.au");
    fs::write(&main_path, "root = 1\n").expect("write entry module");
    let loader = ModuleLoader::new(&main_path).expect("package loader should initialize");

    let error = loader
        .build_constant_init_plan(&main_path)
        .expect_err("an unloaded entry must not produce a partial constant plan");
    assert!(
        error
            .message
            .contains("constant initialization plan is missing loaded module"),
        "unexpected invariant diagnostic: {error:?}"
    );
    assert!(
        error.message.contains(&main_path.display().to_string()),
        "the invariant diagnostic should identify the missing module: {error:?}"
    );
}

#[test]
fn imported_rng_clone_obligations_and_qualified_wrapper_identity_survive_namespaces() {
    let temp = TempDir::new("aura-rng-clone-obligation-imports");
    let utils_path = temp.path().join("utils.au");
    let wrapped_path = temp.path().join("wrapped.au");
    let other_path = temp.path().join("other.au");
    fs::write(
        &utils_path,
        r#"public def duplicate[T](values: list[T]) -> list[T]:
    return values.copy()

public class Duplicator:
    public def duplicate[T](values: list[T]) -> list[T]:
        return values.copy()
"#,
    )
    .expect("write generic clone helper module");
    fs::write(
        &wrapped_path,
        r#"import random

public class Holder:
    public generator: random.Rng

public enum Status:
    Value(random.Rng)
"#,
    )
    .expect("write wrapped Rng module");
    fs::write(
        &other_path,
        r#"import random

public class Holder:
    public generator: int64

public enum Status:
    Value(int64)
"#,
    )
    .expect("write same-leaf nominal collision module");

    let safe_path = temp.path().join("safe.au");
    fs::write(
        &safe_path,
        r#"import utils
from utils import Duplicator

def main() -> int32:
    values = [1, 2]
    function_copies = utils.duplicate(values)
    method_copies = Duplicator.duplicate(values)
    print(function_copies)
    print(method_copies)
    return 0
"#,
    )
    .expect("write safe imported generic use");
    check_path(&safe_path).expect("safe imported function and method instantiations should check");

    for (file_name, extra_import, helper) in [
        ("bad_function.au", "", "utils.duplicate"),
        (
            "bad_method.au",
            "from utils import Duplicator\n",
            "Duplicator.duplicate",
        ),
    ] {
        let path = temp.path().join(file_name);
        fs::write(
            &path,
            format!(
                "import random\nimport utils\n{extra_import}\ndef main() -> int32:\n    values = [random.Rng(seed=1)]\n    copies = {helper}(values)\n    print(copies)\n    return 0\n"
            ),
        )
        .expect("write rejected imported generic use");
        let error = check_path(&path)
            .expect_err("imported clone obligations must reject Rng instantiations");
        assert_eq!(error.code, "AU3007", "unexpected diagnostic for {helper}");
    }

    let collision_path = temp.path().join("collision.au");
    fs::write(
        &collision_path,
        r#"import random
import other
import wrapped

class Holder:
    value: int32

def main() -> int32:
    holders = [wrapped.Holder(random.Rng(seed=1))]
    copies = holders.copy()
    print(copies)
    return 0
"#,
    )
    .expect("write qualified wrapper collision use");
    let collision = check_path(&collision_path)
        .expect_err("a qualified imported wrapper must not collapse to a same-leaf local class");
    assert_eq!(collision.code, "AU3007");
    assert!(collision.message.contains("wrapped.Holder"));

    let enum_collision_path = temp.path().join("enum_collision.au");
    fs::write(
        &enum_collision_path,
        r#"import random
import other
import wrapped

enum Status:
    Ready

def main() -> int32:
    statuses = [wrapped.Status.Value(random.Rng(seed=1))]
    copies = statuses.copy()
    print(copies)
    return 0
"#,
    )
    .expect("write qualified enum collision use");
    let enum_collision = check_path(&enum_collision_path)
        .expect_err("a qualified imported enum must not collapse to a same-leaf local enum");
    assert_eq!(enum_collision.code, "AU3007");
    assert!(enum_collision.message.contains("wrapped.Status"));

    let from_import_path = temp.path().join("from_import.au");
    fs::write(
        &from_import_path,
        r#"import random
from wrapped import Holder

def make() -> Holder:
    return Holder(random.Rng(seed=1))

def main() -> int32:
    holders = [make()]
    copies = holders.copy()
    print(copies)
    return 0
"#,
    )
    .expect("write from-import constructor use");
    let from_import = check_path(&from_import_path).expect_err(
        "a from-import constructor result must retain its defining module's nominal identity",
    );
    assert_eq!(from_import.code, "AU3007");
    assert!(from_import.message.contains("wrapped.Holder"));
}

#[test]
fn imported_same_leaf_class_identity_survives_mir_and_direct_lowering() {
    let temp = TempDir::new("aura-imported-same-leaf-class-identity");
    let named_path = temp.path().join("named.au");
    let remote_path = temp.path().join("remote.au");
    let main_path = temp.path().join("main.au");
    fs::write(
        &named_path,
        r#"public trait Named:
    def name(self) -> str
"#,
    )
    .expect("write shared trait module");
    fs::write(
        &remote_path,
        r#"from named import Named

public class User:
    public label: str

    public def associated() -> str:
        return "remote-associated"

    public def inherent(self) -> str:
        return f"remote-inherent:{self.label}"

impl Named for User:
    def name(self) -> str:
        return f"remote:{self.label}"
"#,
    )
    .expect("write remote same-leaf class module");
    fs::write(
        &main_path,
        r#"from named import Named
import remote

class User:
    label: str

    def associated() -> str:
        return "local-associated"

    def inherent(self) -> str:
        return f"local-inherent:{self.label}"

impl Named for User:
    def name(self) -> str:
        return f"local:{self.label}"

def main() -> int32:
    local = User(label="L")
    imported = remote.User(label="R")
    print(User.associated())
    print(remote.User.associated())
    print(local.inherent())
    print(imported.inherent())
    print(local.name())
    print(imported.name())
    return 0
"#,
    )
    .expect("write same-leaf dispatch entry module");

    let output = run_path(&main_path).expect("same-leaf class identities should run distinctly");
    assert_eq!(
        output.stdout,
        "local-associated\nremote-associated\nlocal-inherent:L\nremote-inherent:R\nlocal:L\nremote:R\n"
    );

    let mir = lower_path_to_mir(&main_path).expect("same-leaf class identities should lower");
    assert!(mir.classes.iter().any(|class| class.name == "User"));
    assert!(mir.classes.iter().any(|class| class.name == "remote.User"));
    assert!(emit_host_native_object(&mir).is_ok());
}

#[test]
fn qualified_inherent_associated_methods_use_class_type_arguments_for_clone_safety() {
    let temp = TempDir::new("aura-qualified-associated-clone-safety");
    let factory_path = temp.path().join("factory.au");
    fs::write(
        &factory_path,
        r#"public class Factory[T]:
    public def probe() -> int32:
        values = list[T]()
        copies = values.copy()
        print(copies)
        return 0
"#,
    )
    .expect("write generic associated-method module");

    let safe_path = temp.path().join("safe.au");
    fs::write(
        &safe_path,
        r#"import factory

def main() -> int32:
    print(factory.Factory[int64].probe())
    with group = TaskGroup():
        group.start_soon(factory.Factory[int64].probe)
    return 0
"#,
    )
    .expect("write safe qualified associated-method use");
    check_path(&safe_path)
        .expect("a qualified associated method should use its safe class specialization");

    let unsafe_path = temp.path().join("unsafe.au");
    fs::write(
        &unsafe_path,
        r#"import factory
import random

def main() -> int32:
    print(factory.Factory[random.Rng].probe())
    return 0
"#,
    )
    .expect("write unsafe qualified associated-method use");
    let error = check_path(&unsafe_path)
        .expect_err("a qualified associated method must reject an unsafe class specialization");
    assert_eq!(error.code, "AU3007");
    assert!(error.message.contains("non-cloneable `random.Rng`"));

    let task_path = temp.path().join("unsafe_task.au");
    fs::write(
        &task_path,
        r#"import factory
import random

def main() -> int32:
    with group = TaskGroup():
        group.start_soon(factory.Factory[random.Rng].probe)
    return 0
"#,
    )
    .expect("write unsafe qualified associated-method task target");
    let task_error = check_path(&task_path).expect_err(
        "a qualified associated-method task target must reject an unsafe specialization",
    );
    assert_eq!(task_error.code, "AU3007");
    assert!(task_error.message.contains("non-cloneable `random.Rng`"));
}

#[test]
fn module_loader_helper_functions_cover_namespace_and_export_paths() {
    let temp = TempDir::new("aura-lib-helpers");
    let pkg_dir = temp.path().join("pkg");
    fs::create_dir_all(&pkg_dir).expect("failed to create pkg dir");
    let user_path = pkg_dir.join("user.au");
    let named_path = pkg_dir.join("named.au");

    fs::write(
        &named_path,
        "public trait Named:\n    def name(self) -> str\n",
    )
    .expect("write named module");
    fs::write(
        &user_path,
        [
            "from pkg.named import Named",
            "",
            "public answer = 42",
            "",
            "public class Box[T]:",
            "    value: T",
            "    public def read(own self) -> T:",
            "        return self.value",
            "",
            "class Hidden:",
            "    value: int32",
            "",
            "public enum Flag[T]:",
            "    Ready",
            "    Value(Box[T])",
            "",
            "enum Secret:",
            "    Hidden",
            "",
            "public trait Show[T]:",
            "    def render(self, other: T) -> str",
            "",
            "trait HiddenTrait:",
            "    def hide(self)",
            "",
            "public def wrap(value: own Box[int32]) -> Box[int32]:",
            "    return value",
            "",
            "def hidden() -> int32:",
            "    return 0",
            "",
            "impl[T] Show[T] for Box[T]:",
            "    def render(self, other: T) -> str:",
            "        return \"ok\"",
        ]
        .join("\n"),
    )
    .expect("write user module");

    let relative_path = pkg_dir
        .strip_prefix(std::env::current_dir().expect("cwd"))
        .unwrap_or(&pkg_dir)
        .join("user.au");
    assert!(absolutize(&relative_path).is_absolute());
    assert_eq!(
        absolutize(&user_path),
        fs::canonicalize(&user_path).expect("user path should canonicalize")
    );
    assert_eq!(
        canonicalize_if_exists(&user_path).expect("existing helper paths should canonicalize"),
        fs::canonicalize(&user_path).expect("user path should canonicalize")
    );
    let canonical_temp = fs::canonicalize(temp.path()).expect("temp root should canonicalize");
    let missing_nested = temp.path().join("missing").join("leaf.au");
    assert_eq!(
        absolutize(&missing_nested),
        canonical_temp.join("missing").join("leaf.au")
    );
    assert_eq!(
        canonicalize_if_exists(&temp.path().join("future.au"))
            .expect("missing file under existing root should resolve against root"),
        canonical_temp.join("future.au")
    );
    assert_eq!(
        canonicalize_if_exists(temp.path()).expect("existing roots should canonicalize"),
        canonical_temp
    );
    let missing_leaf = PathBuf::from(format!(
        "aura-lib-missing-leaf-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough for a test")
            .as_nanos()
    ));
    assert_eq!(
        canonicalize_if_exists(&missing_leaf)
            .expect("single missing helper leaves should pass through"),
        missing_leaf
    );

    let inferred_root =
        infer_package_root(&user_path, Some(&fs::read_to_string(&user_path).unwrap()))
            .expect("package root should infer");
    assert_eq!(
        inferred_root,
        fs::canonicalize(temp.path()).expect("temp root should canonicalize")
    );
    assert!(import_exists_from_root(
        temp.path(),
        &["pkg".to_string(), "named".to_string()]
    ));
    assert_eq!(logical_module_name(temp.path(), &user_path), "pkg.user");
    assert!(is_builtin_export_type("str"));
    assert!(is_builtin_export_type("int"));
    assert!(!is_builtin_export_type("Box"));

    let mut program = check_path(&user_path).expect("user module should check");
    assert!(local_item_exists(&program, "Box"));
    assert!(local_item_exists(&program, "answer"));
    assert!(!local_item_exists(&program, "missing"));

    let imported_named = check_path(&named_path).expect("named module should check");
    let remote_namespace =
        exported_namespace(&["pkg".to_string(), "named".to_string()], &imported_named);
    let mut remote_only_namespace = remote_namespace.clone();
    remote_only_namespace.classes.insert(
        "Remote".to_string(),
        program.classes.get("Box").expect("box info").clone(),
    );
    remote_only_namespace.all_classes.insert(
        "Remote".to_string(),
        program.classes.get("Box").expect("box info").clone(),
    );
    program
        .imported_modules
        .insert("named".to_string(), remote_only_namespace.clone());
    program.module_name = "pkg.user".to_string();
    program.source_path = Some(user_path.display().to_string());

    let qualified_local = qualify_export_type(
        &program,
        &crate::sema::Type::Named("Box".to_string(), vec![crate::sema::Type::named("int32")]),
    );
    assert_eq!(
        qualified_local,
        crate::sema::Type::Named(
            "pkg.user.Box".to_string(),
            vec![crate::sema::Type::named("int32")]
        )
    );

    let qualified_imported = qualify_export_type(&program, &crate::sema::Type::named("Remote"));
    assert_eq!(
        qualified_imported,
        crate::sema::Type::named("pkg.named.Remote")
    );
    let qualified_function = qualify_export_type(
        &program,
        &crate::sema::Type::Function {
            params: vec![
                crate::sema::FunctionParamContract {
                    name: "local".to_string(),
                    ty: crate::sema::Type::named("Box"),
                    passing: crate::ast::ReceiverKind::BorrowMut,
                    has_default: true,
                    default_erased: false,
                },
                crate::sema::FunctionParamContract {
                    name: "remote".to_string(),
                    ty: crate::sema::Type::Tuple(vec![crate::sema::Type::named("Remote")]),
                    passing: crate::ast::ReceiverKind::Value,
                    has_default: false,
                    default_erased: true,
                },
            ],
            return_type: Box::new(crate::sema::Type::Function {
                params: Vec::new(),
                return_type: Box::new(crate::sema::Type::named("Box")),
            }),
        },
    );
    assert_eq!(
        qualified_function,
        crate::sema::Type::Function {
            params: vec![
                crate::sema::FunctionParamContract {
                    name: "local".to_string(),
                    ty: crate::sema::Type::named("pkg.user.Box"),
                    passing: crate::ast::ReceiverKind::BorrowMut,
                    has_default: true,
                    default_erased: false,
                },
                crate::sema::FunctionParamContract {
                    name: "remote".to_string(),
                    ty: crate::sema::Type::Tuple(vec![crate::sema::Type::named(
                        "pkg.named.Remote"
                    )]),
                    passing: crate::ast::ReceiverKind::Value,
                    has_default: false,
                    default_erased: true,
                },
            ],
            return_type: Box::new(crate::sema::Type::Function {
                params: Vec::new(),
                return_type: Box::new(crate::sema::Type::named("pkg.user.Box")),
            }),
        }
    );

    let mut ambiguous_modules = BTreeMap::new();
    let mut first = remote_namespace.clone();
    first.path = "pkg.named".to_string();
    let mut second = remote_namespace.clone();
    second.path = "pkg.alt".to_string();
    ambiguous_modules.insert(first.name.clone(), first);
    ambiguous_modules.insert("alt".to_string(), second);
    let mut found = None;
    let mut ambiguous = false;
    find_type_namespace_path(&ambiguous_modules, "Named", &mut found, &mut ambiguous);
    assert!(found.is_some());
    assert!(ambiguous);

    let qualified_ref = qualify_export_type_ref(
        &program,
        &TypeRef::named(
            "Box",
            vec![type_ref("int32")],
            false,
            crate::diag::Span::new(1, 1),
        ),
    );
    assert_eq!(named_ref_name(&qualified_ref), "pkg.user.Box");
    assert_eq!(
        named_ref_name(&qualified_ref.named_parts().expect("named ref").1[0]),
        "int32"
    );
    let qualified_function_ref = qualify_export_type_ref(
        &program,
        &TypeRef::function(
            vec![
                type_ref("Box"),
                TypeRef::tuple(vec![type_ref("Remote")], false, Span::new(1, 1)),
            ],
            TypeRef::function(Vec::new(), type_ref("Box"), Span::new(1, 1)),
            Span::new(1, 1),
        ),
    );
    let (params, nested_return) = qualified_function_ref
        .function_parts()
        .expect("qualified function type should remain structural");
    assert_eq!(named_ref_name(&params[0].ty), "pkg.user.Box");
    assert_eq!(
        named_ref_name(
            &params[1]
                .ty
                .elements()
                .expect("tuple parameter should remain structural")[0]
        ),
        "pkg.named.Remote"
    );
    let (nested_params, return_type) = nested_return
        .function_parts()
        .expect("nested function return should remain structural");
    assert!(nested_params.is_empty());
    assert_eq!(named_ref_name(return_type), "pkg.user.Box");
    assert_eq!(
        named_ref_name(&qualify_export_type_ref(&program, &type_ref("str"))),
        "str"
    );
    assert_eq!(
        qualify_export_type(&program, &crate::sema::Type::TypeParam("T".to_string())),
        crate::sema::Type::TypeParam("T".to_string())
    );
    assert_eq!(
        qualify_export_type(
            &program,
            &crate::sema::Type::Module("pkg.named".to_string())
        ),
        crate::sema::Type::Module("pkg.named".to_string())
    );
    assert_eq!(
        qualify_export_type(&program, &crate::sema::Type::Unit),
        crate::sema::Type::Unit
    );
    let mut type_param_bounds = BTreeMap::new();
    type_param_bounds.insert(
        "T".to_string(),
        vec![type_ref("Box"), type_ref("Remote"), type_ref("str")],
    );
    let qualified_bounds = qualify_export_bounds(&program, &type_param_bounds);
    let qualified_bound_names = qualified_bounds
        .get("T")
        .expect("bounds should preserve type parameter")
        .iter()
        .map(named_ref_name)
        .collect::<Vec<_>>();
    assert_eq!(
        qualified_bound_names,
        vec!["pkg.user.Box", "pkg.named.Remote", "str"]
    );
    let qualified_enum =
        qualify_enum_decl_for_export(&program, &program.enums.get("Flag").expect("flag").decl);
    assert_eq!(
        named_ref_name(&qualified_enum.variants[1].payloads[0].ty),
        "pkg.user.Box"
    );
    let qualified_impl = qualify_impl_decl_for_export(&program, &program.trait_impls[0].decl);
    assert_eq!(qualified_impl.trait_name, "Show");
    assert_eq!(named_ref_name(&qualified_impl.trait_args[0]), "T");

    let mut namespace_map = BTreeMap::new();
    let mut local_namespace = remote_namespace.clone();
    local_namespace.name = "local".to_string();
    local_namespace.path = "pkg.local".to_string();
    namespace_map.insert("named".to_string(), remote_namespace.clone());
    namespace_map.insert("local".to_string(), local_namespace);
    qualify_imported_module_namespaces(
        &mut namespace_map,
        "dep",
        &BTreeSet::from(["named".to_string()]),
    );
    assert_eq!(
        namespace_map.get("named").expect("dependency alias").path,
        "pkg.named"
    );
    assert_eq!(
        namespace_map.get("local").expect("local namespace").path,
        "dep.pkg.local"
    );

    match exported_binding(&program, "wrap").expect("public function export") {
        crate::sema::ImportedBinding::Function(info) => {
            assert_eq!(named_ref_name(&info.decl.return_type), "pkg.user.Box");
        }
        other => panic!("expected function binding, found {other:?}"),
    }
    assert!(exported_binding(&program, "hidden").is_none());

    let namespace = exported_namespace(&["pkg".to_string(), "user".to_string()], &program);
    assert!(namespace.functions.contains_key("wrap"));
    assert!(namespace.classes.contains_key("Box"));
    assert!(namespace.enums.contains_key("Flag"));
    assert!(namespace.traits.contains_key("Show"));
    assert!(!namespace.functions.contains_key("hidden"));
    assert!(!namespace.classes.contains_key("Hidden"));
    assert_eq!(namespace.path, "pkg.user");

    let root_namespace = exported_namespace(&[], &program);
    assert_eq!(root_namespace.name, "pkg.user");
    assert_eq!(root_namespace.path, "");
    assert!(root_namespace.all_classes.contains_key("Box"));

    let mut bindings = BTreeMap::new();
    insert_namespace_import(
        &mut bindings,
        &[],
        namespace.clone(),
        crate::diag::Span::new(1, 1),
    )
    .expect("empty namespace import should be ignored");
    insert_namespace_import(
        &mut bindings,
        &["pkg".to_string()],
        namespace.clone(),
        crate::diag::Span::new(1, 1),
    )
    .expect("single-segment namespace import should work");
    insert_namespace_import(
        &mut bindings,
        &["pkg".to_string(), "user".to_string()],
        namespace.clone(),
        crate::diag::Span::new(1, 1),
    )
    .expect("nested namespace import should work");
    let root = match bindings.get("pkg").expect("pkg binding") {
        crate::sema::ImportedBinding::Module(root) => root,
        other => panic!("expected module binding, found {other:?}"),
    };
    assert!(root.modules.contains_key("user"));

    bindings.insert(
        "pkg".to_string(),
        crate::sema::ImportedBinding::Function(
            program.functions.get("wrap").expect("wrap info").clone(),
        ),
    );
    let duplicate = insert_namespace_import(
        &mut bindings,
        &["pkg".to_string(), "other".to_string()],
        namespace,
        crate::diag::Span::new(1, 1),
    )
    .expect_err("non-module root bindings should reject namespace imports");
    assert!(duplicate.message.contains("duplicate import binding `pkg`"));
}

#[test]
fn exported_callable_types_are_qualified_in_imported_analysis_hovers() {
    let temp = TempDir::new("aura-lib-exported-callable-analysis");
    let pkg_dir = temp.path().join("pkg");
    fs::create_dir_all(&pkg_dir).expect("failed to create pkg dir");
    let api_path = pkg_dir.join("api.au");
    let main_path = temp.path().join("main.au");
    fs::write(
        &api_path,
        [
            "public class Token:",
            "    public value: int32",
            "",
            "public def identity(value: own Token) -> Token:",
            "    return value",
            "",
            "public def choose(transform: def(own Token) -> Token) -> def(own Token) -> Token:",
            "    return transform",
        ]
        .join("\n"),
    )
    .expect("write exported callable API");
    let source = [
        "import pkg.api",
        "",
        "def main() -> int32:",
        "    selected = pkg.api.choose",
        "    transform = selected(pkg.api.identity)",
        "    token = pkg.api.Token(value=1)",
        "    result = transform(token)",
        "    print(result.value)",
        "    return 0",
    ]
    .join("\n");
    fs::write(&main_path, &source).expect("write callable API consumer");

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
        hovers.contains(
            &"```aura\nbinding selected: def(def(own pkg.api.Token) -> pkg.api.Token) -> def(own pkg.api.Token) -> pkg.api.Token\n```"
        ),
        "the imported higher-order function value must expose qualified parameter and return types: {hovers:?}"
    );
    assert!(
        hovers
            .contains(&"```aura\nbinding transform: def(own pkg.api.Token) -> pkg.api.Token\n```"),
        "the indirect call must preserve the qualified callable return type: {hovers:?}"
    );
    assert!(
        hovers.contains(&"```aura\nbinding result: pkg.api.Token\n```"),
        "calling the imported function value must preserve its qualified result type: {hovers:?}"
    );
    assert!(
        hovers
            .iter()
            .any(|hover| hover.contains("field value: int32")),
        "the qualified result must still resolve the exported Token field"
    );
}

#[test]
fn module_loader_reports_import_resolution_and_export_errors() {
    let temp = TempDir::new("aura-lib-import-errors");
    let pkg_dir = temp.path().join("pkg");
    fs::create_dir_all(&pkg_dir).expect("failed to create pkg dir");
    let module_path = pkg_dir.join("mod.au");
    fs::write(
        &module_path,
        [
            "def hidden() -> int32:",
            "    return 0",
            "",
            "public class Box:",
            "    value: int32",
            "",
            "public enum Flag:",
            "    Ready",
            "",
            "public trait Show:",
            "    def render(self) -> str",
        ]
        .join("\n"),
    )
    .expect("write module");

    let private_main = temp.path().join("private.au");
    fs::write(&private_main, "from pkg.mod import hidden\n").expect("write private main");
    let private_error = check_path(&private_main).expect_err("private imports should fail");
    assert!(private_error
        .message
        .contains("item `hidden` is private in module `pkg.mod`"));

    let missing_main = temp.path().join("missing.au");
    fs::write(&missing_main, "from pkg.mod import Missing\n").expect("write missing main");
    let missing_error = check_path(&missing_main).expect_err("missing exports should fail");
    assert!(missing_error
        .message
        .contains("module `pkg.mod` has no export named `Missing`"));

    let duplicate_main = temp.path().join("duplicate.au");
    fs::write(
        &duplicate_main,
        "from pkg.mod import Box\nfrom pkg.mod import Box\n",
    )
    .expect("write duplicate main");
    let duplicate_error =
        check_path(&duplicate_main).expect_err("duplicate import bindings should fail");
    assert!(duplicate_error
        .message
        .contains("duplicate import binding `Box`"));

    let unresolved_main = temp.path().join("unresolved.au");
    fs::write(&unresolved_main, "import pkg.missing\n").expect("write unresolved main");
    let unresolved_error = check_path(&unresolved_main).expect_err("missing modules should fail");
    assert!(unresolved_error
        .message
        .contains("cannot resolve module `pkg.missing`"));

    let fallback_root = infer_package_root(&duplicate_main, Some("not: valid: aura"))
        .expect("invalid override should fall back to the entry dir");
    assert_eq!(
        fallback_root,
        fs::canonicalize(temp.path()).expect("temp root should canonicalize")
    );

    let program = check_path(&module_path).expect("module should check");
    assert!(matches!(
        exported_binding(&program, "Box"),
        Some(crate::sema::ImportedBinding::Class(_))
    ));
    assert!(matches!(
        exported_binding(&program, "Flag"),
        Some(crate::sema::ImportedBinding::Enum(_))
    ));
    assert!(matches!(
        exported_binding(&program, "Show"),
        Some(crate::sema::ImportedBinding::Trait(_))
    ));
}

#[cfg(unix)]
#[test]
fn module_loader_rejects_symlinked_import_that_escapes_root_without_manifest() {
    let temp = TempDir::new("aura-lib-import-symlink-escape");
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&root).expect("failed to create package root");
    fs::create_dir_all(&outside).expect("failed to create outside dir");

    let outside_module = outside.join("evil.au");
    fs::write(
        &outside_module,
        "public def helper() -> int32:\n    return 1\n",
    )
    .expect("failed to write outside module");
    std::os::unix::fs::symlink(&outside_module, root.join("evil.au"))
        .expect("failed to create escaping module symlink");

    let main_path = root.join("main.au");
    fs::write(
        &main_path,
        "from evil import helper\n\ndef main() -> None:\n    print(helper())\n",
    )
    .expect("failed to write main module");

    let error = check_path(&main_path).expect_err("symlinked import should be rejected");
    assert!(error.message.contains("escapes package source root"));
}

#[test]
fn parses_the_point_milestone() {
    let module = parse_source(POINT_SOURCE).expect("point program should parse");
    assert_eq!(module.items.len(), 3);
    assert_eq!(module.top_level_stmts.len(), 0);
}

#[test]
fn type_checks_the_point_milestone() {
    check_source(POINT_SOURCE).expect("point program should type-check");
}

#[test]
fn runs_the_point_milestone() {
    let output = run_source(POINT_SOURCE).expect("point program should run");
    assert_eq!(output.stdout, "5.0\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn explicit_mir_runtime_runs_the_point_milestone() {
    let mir = lower_source_to_mir(POINT_SOURCE).expect("point program should lower to MIR");
    let output = run_mir(&mir).expect("point program should run via explicit MIR runtime");
    assert_eq!(output.stdout, "5.0\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn omitted_none_return_type_is_allowed() {
    let module = parse_source(BASIC_ADDITION_SOURCE).expect("basic addition should parse");
    assert_eq!(module.items.len(), 1);
    assert_eq!(module.top_level_stmts.len(), 0);

    let output = run_source(BASIC_ADDITION_SOURCE).expect("basic addition should run");
    assert_eq!(output.stdout, "16\n");
    assert_eq!(output.value, Value::Unit);
}

#[test]
fn top_level_scripts_run_without_main() {
    let module = parse_source(TOP_LEVEL_ADDITION_SOURCE).expect("top-level addition should parse");
    assert_eq!(module.items.len(), 0);
    assert_eq!(module.constants.len(), 3);
    assert_eq!(module.top_level_stmts.len(), 1);

    let output = run_source(TOP_LEVEL_ADDITION_SOURCE).expect("top-level addition should run");
    assert_eq!(output.stdout, "16\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn script_mode_mutable_locals_coexist_with_module_constants() {
    let source = "limit: int32 = 3\nmut counter: int32 = 0\nwhile counter < limit:\n    counter += 1\nprint(counter)\n";
    let module = parse_source(source).expect("script source should parse");
    assert_eq!(module.constants.len(), 1);
    assert_eq!(module.top_level_stmts.len(), 3);

    check_source(source).expect("script source should type-check");
    let output = run_source(source).expect("script source should run");
    assert_eq!(output.stdout, "3\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn top_level_script_plain_and_compound_reassignment_share_the_mutable_local() {
    let source = "mut count = 0\ncount = count + 1\ncount += 1\nprint(count)\n";

    check_source(source).expect("both top-level reassignment forms should type-check");
    let output = run_source(source).expect("both top-level reassignment forms should run");
    assert_eq!(output.stdout, "2\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mutable_top_level_binding_cannot_be_module_storage_beside_main() {
    let error = check_source("mut counter: int32 = 0\ndef main():\n    print(counter)\n")
        .expect_err("mutable module storage must remain rejected");
    assert_eq!(error.code, "AU3003");
    assert_eq!(error.span, Some(crate::diag::Span::new(1, 5)));
    assert!(error.message.contains("module bindings are immutable"));
}

#[test]
fn control_flow_example_runs() {
    check_source(CONTROL_FLOW_SOURCE).expect("control flow example should type-check");
    let output = run_source(CONTROL_FLOW_SOURCE).expect("control flow example should run");
    assert_eq!(output.stdout, "ok\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_reports_recursion_limit_before_overflowing_the_host_stack() {
    let source = include_str!("../../../test_edge/test_recursive_medium.au");
    let source = source.to_string();
    let handle = thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run_source(&source))
        .expect("recursion test thread should spawn");
    let error = handle
        .join()
        .expect("recursion test thread should join")
        .expect_err("medium recursion should diagnose before stack overflow");
    assert!(error.message.contains("maximum call depth of"));
    assert!(error.message.contains("count_down"));
}

#[test]
fn mir_runtime_still_supports_medium_recursion_below_the_limit() {
    let source = "def count_down(n: int32) -> int32:\n    if n <= 0:\n        return 0\n    return count_down(n=n - 1)\n\ndef main() -> int32:\n    print(count_down(n=120))\n    return 0\n";
    let output = run_source(source).expect("medium recursion below the limit should succeed");
    assert_eq!(output.stdout, "0\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_runs_class_methods_example() {
    let source = include_str!("../../../examples/classes/methods.au");
    let mir = lower_source_to_mir(source).expect("methods example should lower to MIR");
    let output = run_mir(&mir).expect("methods example should run via explicit MIR runtime");
    assert_eq!(output.stdout, "4\n8\n0\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_runs_enum_match_example() {
    let source = include_str!("../../../examples/enums/result_match.au");
    let mir = lower_source_to_mir(source).expect("enum match example should lower to MIR");
    let output = run_mir(&mir).expect("enum match example should run via explicit MIR runtime");
    assert_eq!(output.stdout, "42\nbad\n0\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_runs_try_example_natively() {
    let source = include_str!("../../../examples/error_handling/try_result.au");
    let mir = lower_source_to_mir(source).expect("try example should lower to MIR");
    let output = run_mir(&mir).expect("try example should run directly through MIR");
    assert_eq!(output.stdout, "6\ndivision by zero\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn public_run_path_runs_try_example_natively() {
    let source = include_str!("../../../examples/error_handling/try_result.au");
    let output = run_source(source).expect("try example should run through the public run path");
    assert_eq!(output.stdout, "6\ndivision by zero\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn public_run_path_runs_with_example_natively() {
    let source = include_str!("../../../examples/resources/with_resource.au");
    let output = run_source(source).expect("with example should run through the public run path");
    assert_eq!(output.stdout, "demo\nclosed demo\ndone\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_runs_with_example_natively() {
    let source = include_str!("../../../examples/resources/with_resource.au");
    let mir = lower_source_to_mir(source).expect("with example should lower to MIR");
    let output = run_mir(&mir).expect("with example should run directly through MIR");
    assert_eq!(output.stdout, "demo\nclosed demo\ndone\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_runs_queues_example_natively() {
    let source = include_str!("../../../examples/concurrency/task_group_start.au");
    let mir = lower_source_to_mir(source).expect("queue example should lower to MIR");
    let output = run_mir(&mir).expect("queue example should run directly through MIR");
    assert_eq!(output.stdout, "2\n4\n6\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_runs_send_result_example_natively() {
    let source = include_str!("../../../examples/concurrency/send_result.au");
    let mir = lower_source_to_mir(source).expect("send_result example should lower to MIR");
    let output = run_mir(&mir).expect("send_result example should run directly through MIR");
    assert_eq!(output.stdout, "7\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_runs_task_group_start_soon_example_natively() {
    let source = include_str!("../../../examples/concurrency/task_group_start_soon.au");
    let mir =
        lower_source_to_mir(source).expect("task_group_start_soon example should lower to MIR");
    let output =
        run_mir(&mir).expect("task_group_start_soon example should run directly through MIR");
    assert_eq!(output.stdout, "9\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_runs_queue_get_timeout_example_natively() {
    let source = include_str!("../../../examples/concurrency/queue_get_timeout.au");
    let mir = lower_source_to_mir(source).expect("queue_get_timeout example should lower to MIR");
    let output = run_mir(&mir).expect("queue_get_timeout example should run directly through MIR");
    assert_eq!(output.stdout, "Option.None\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_runs_queue_put_timeout_example_natively() {
    let source = include_str!("../../../examples/concurrency/queue_put_timeout.au");
    let mir = lower_source_to_mir(source).expect("queue_put_timeout example should lower to MIR");
    let output = run_mir(&mir).expect("queue_put_timeout example should run directly through MIR");
    assert_eq!(output.stdout, "sent\n4\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_runs_task_group_queue_sum_example_natively() {
    let source = include_str!("../../../examples/concurrency/task_group_queue_sum.au");
    let mir =
        lower_source_to_mir(source).expect("task_group_queue_sum example should lower to MIR");
    let output =
        run_mir(&mir).expect("task_group_queue_sum example should run directly through MIR");
    assert_eq!(output.stdout, "3\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_runtime_runs_task_group_cancel_example_natively() {
    let source = include_str!("../../../examples/concurrency/task_group_cancel.au");
    let mir = lower_source_to_mir(source).expect("task_group_cancel example should lower to MIR");
    let output = run_mir(&mir).expect("task_group_cancel example should run directly through MIR");
    assert_eq!(output.stdout, "0\n1\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn serialized_mir_runner_executes_point_example() {
    let source = include_str!("../../../examples/point.au");
    let mir = lower_source_to_mir(source).expect("point example should lower to MIR");
    let mir_json = serde_json::to_vec(&mir).expect("MIR should serialize to JSON bytes");
    let output = run_serialized_mir(&mir_json, "/virtual/point.au", source)
        .expect("serialized MIR runner should execute point example");
    assert_eq!(output.stdout, "5.0\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn serialized_mir_runner_reports_invalid_embedded_mir() {
    let error = run_serialized_mir(b"{not json", "/virtual/bad.au", "print(value=1)\n")
        .expect_err("invalid embedded MIR should return a diagnostic");
    assert!(
        error.message.contains("failed to deserialize embedded MIR"),
        "unexpected diagnostic: {}",
        error
    );
}

#[test]
fn path_with_source_mir_lowering_resolves_local_module_imports() {
    let temp = TempDir::new("aura-compiler-lower-path-source");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\n",
    )
    .expect("failed to write helper module");
    let main_path = temp.path().join("main.au");
    let source = "import helpers.math\n\ndef main() -> int32:\n    print(helpers.math.double(value=5))\n    return 0\n";
    let mir = lower_path_with_source_to_mir(&main_path, source)
        .expect("path-aware MIR lowering should resolve local imports");
    let output = run_mir(&mir).expect("path-aware MIR lowering should produce runnable MIR");
    assert_eq!(output.stdout, "10\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn imported_function_return_types_keep_members_visible_across_modules() {
    let temp = TempDir::new("aura-compiler-imported-return-members");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/counter.au"),
        [
            "public class Counter:",
            "    public value: int32",
            "",
            "public def make_counter() -> Counter:",
            "    return Counter(value=41)",
            "",
        ]
        .join("\n"),
    )
    .expect("failed to write helper module");
    let main_path = temp.path().join("main.au");
    fs::write(
        &main_path,
        [
            "from helpers.counter import make_counter",
            "",
            "def main() -> int32:",
            "    counter = make_counter()",
            "    print(counter.value)",
            "    return 0",
            "",
        ]
        .join("\n"),
    )
    .expect("failed to write main module");

    let checked = check_path(&main_path)
        .expect("return type members from imported functions should stay visible");
    assert!(
        checked.functions.get("main").is_some(),
        "main should still type-check"
    );

    let output = run_path(&main_path).expect("module program should run");
    assert_eq!(output.stdout, "41\n");
    assert_eq!(output.value, zero_exit_value());

    let mir = lower_path_to_mir(&main_path).expect("module program should lower to MIR");
    let mir_output = run_mir(&mir).expect("module program should run through explicit MIR runtime");
    assert_eq!(mir_output.stdout, "41\n");
    assert_eq!(mir_output.value, zero_exit_value());
}

#[test]
fn imported_tuple_signatures_qualify_each_element_across_modules() {
    let temp = TempDir::new("aura-compiler-imported-tuple-signatures");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/token.au"),
        [
            "public class Token:",
            "    public value: int32",
            "",
            "public def make_token() -> (Token, int32):",
            "    return (Token(value=40), 2)",
            "",
        ]
        .join("\n"),
    )
    .expect("failed to write helper module");
    let main_path = temp.path().join("main.au");
    fs::write(
        &main_path,
        [
            "from helpers.token import make_token",
            "",
            "def main() -> int32:",
            "    token, delta = make_token()",
            "    print(token.value + delta)",
            "    return 0",
            "",
        ]
        .join("\n"),
    )
    .expect("failed to write main module");

    let checked = check_path(&main_path)
        .expect("tuple elements in imported signatures should keep qualified class types");
    let make_token = checked
        .functions
        .get("make_token")
        .expect("imported tuple-returning function should be present");
    assert_eq!(
        make_token.signature.return_type,
        crate::sema::Type::Tuple(vec![
            crate::sema::Type::named("helpers.token.Token"),
            crate::sema::Type::named("int32"),
        ])
    );

    let direct = run_path(&main_path).expect("imported tuple signature should run directly");
    assert_eq!(direct.stdout, "42\n");
    assert_eq!(direct.value, zero_exit_value());

    let mir = lower_path_to_mir(&main_path)
        .expect("imported tuple signature should lower through module boundaries");
    let mir_output = run_mir(&mir).expect("imported tuple signature should run through MIR");
    assert_eq!(mir_output.stdout, "42\n");
    assert_eq!(mir_output.value, zero_exit_value());
}

#[test]
fn imported_nested_closures_preserve_qualified_capture_types_on_both_runtimes() {
    let temp = TempDir::new("aura-compiler-imported-nested-closure-captures");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/token.au"),
        [
            "public class Token:",
            "    public value: int32",
            "",
            "public def nested_value(value: int32) -> int32:",
            "    token = Token(value=value)",
            "    inner: def(int32) -> int32 = lambda delta: token.value + delta",
            "    outer: def(int32) -> int32 = lambda extra: inner(extra) + 1",
            "    return outer(0)",
            "",
        ]
        .join("\n"),
    )
    .expect("failed to write closure helper module");
    let main_path = temp.path().join("main.au");
    fs::write(
        &main_path,
        [
            "from helpers.token import nested_value",
            "",
            "def main() -> int32:",
            "    print(nested_value(value=41))",
            "    return 0",
            "",
        ]
        .join("\n"),
    )
    .expect("failed to write closure consumer");

    let checked = check_path(&main_path)
        .expect("nested closure capture metadata should remain valid across module qualification");
    assert!(
        checked
            .module_registry
            .values()
            .flat_map(|module| module.closures.values())
            .any(|closure| {
                closure.captures.iter().any(|capture| {
                    matches!(
                        &capture.ty,
                        crate::sema::Type::Closure { captures, .. }
                            if captures.iter().any(|nested| {
                                nested.ty
                                    == crate::sema::Type::named("helpers.token.Token")
                            })
                    )
                })
            }),
        "the exported outer closure should retain the fully qualified nested capture type"
    );

    let direct = run_path(&main_path).expect("nested imported closures should run directly");
    assert_eq!(direct.stdout, "42\n");
    assert_eq!(direct.value, zero_exit_value());

    let mir = lower_path_to_mir(&main_path)
        .expect("nested imported closures should lower through module boundaries");
    let mir_output =
        run_mir(&mir).expect("nested imported closures should run through the MIR runtime");
    assert_eq!(mir_output.stdout, "42\n");
    assert_eq!(mir_output.value, zero_exit_value());
}

#[test]
fn broad_scratch_corpus_checks_analysis_and_mir_lowering_do_not_panic() {
    run_with_large_stack(|| {
        let repo_root = repo_root();
        let corpus_dirs = [repo_root.join("test_edge"), repo_root.join("test_recheck")];

        let mut file_count = 0usize;
        let mut checked_ok = 0usize;
        let mut lowered_ok = 0usize;
        let mut emitted_ok = 0usize;

        for dir in corpus_dirs {
            for path in collect_aura_files(&dir) {
                file_count += 1;
                let source = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("failed to read {}: {}", path.display(), error));

                let _ = analyze_path_source(&path, &source);

                if check_path(&path).is_ok() {
                    checked_ok += 1;
                }

                if let Ok(mir) = lower_path_to_mir(&path) {
                    lowered_ok += 1;
                    let emission =
                        run_corpus_case_with_timeout("direct object emission", &path, move || {
                            emit_host_native_object(&mir)
                        })
                        .unwrap_or_else(|error| panic!("{error}"));
                    match emission {
                        Ok(_) => emitted_ok += 1,
                        Err(_) => {}
                    }
                }
            }
        }

        assert!(file_count >= 800, "expected large scratch corpus");
        assert!(checked_ok > 0, "expected some scratch files to type-check");
        assert!(
            lowered_ok > 0,
            "expected some scratch files to lower to MIR"
        );
        assert!(
            emitted_ok > 0,
            "expected some scratch files to emit native direct objects"
        );
    });
}

#[test]
fn broad_scratch_corpus_runtime_paths_do_not_panic() {
    run_with_large_stack(|| {
        let repo_root = repo_root();
        let corpus_dirs = [repo_root.join("test_edge"), repo_root.join("test_recheck")];

        let mut runnable = 0usize;
        let mut run_completed = 0usize;
        let mut explicit_mir_completed = 0usize;

        for dir in corpus_dirs {
            for path in collect_aura_files(&dir) {
                if !should_execute_runtime_corpus_case(&path) {
                    continue;
                }
                if check_path(&path).is_err() {
                    continue;
                }
                runnable += 1;
                if runnable % 50 == 0 {
                    eprintln!(
                        "runtime corpus progress: processed {} runnable files (current: {})",
                        runnable,
                        path.display()
                    );
                }

                let run_path_result = run_corpus_case_with_timeout("public run", &path, {
                    let path = path.clone();
                    move || run_path(&path)
                })
                .unwrap_or_else(|error| panic!("{error}"));
                match run_path_result {
                    Ok(_) | Err(_) => run_completed += 1,
                }

                let explicit_mir_result =
                    run_corpus_case_with_timeout("explicit MIR run", &path, {
                        let path = path.clone();
                        move || lower_path_to_mir(&path).and_then(|mir| run_mir(&mir))
                    })
                    .unwrap_or_else(|error| panic!("{error}"));
                match explicit_mir_result {
                    Ok(_) | Err(_) => explicit_mir_completed += 1,
                }
            }
        }

        assert!(runnable > 0, "expected runnable scratch programs");
        assert!(
            run_completed > 0 && explicit_mir_completed > 0,
            "expected runtime corpus to exercise both execution paths"
        );
    });
}

#[test]
fn maintained_example_tree_public_paths_do_not_panic() {
    let repo_root = repo_root();
    let examples_dir = repo_root.join("examples");

    let mut file_count = 0usize;
    let mut checked_ok = 0usize;
    let mut lowered_ok = 0usize;
    let mut emitted_ok = 0usize;
    let mut run_completed = 0usize;
    let mut explicit_mir_completed = 0usize;
    let package_examples_dir = examples_dir.join("packages");

    for path in collect_aura_files_recursive(&examples_dir) {
        if path.starts_with(&package_examples_dir) {
            continue;
        }
        file_count += 1;
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {}", path.display(), error));

        let analysis = analyze_path_source(&path, &source);
        assert!(
            analysis
                .diagnostics
                .iter()
                .all(|diag| !diag.message.contains("internal")),
            "analysis should not report internal diagnostics for {}: {:?}",
            path.display(),
            analysis.diagnostics
        );

        let _ = crate::analysis::complete_path_source(&path, &source, 0, 0, None);

        if check_path(&path).is_ok() {
            checked_ok += 1;
        }

        if let Ok(mir) = lower_path_to_mir(&path) {
            lowered_ok += 1;
            let emission = run_corpus_case_with_timeout(
                "maintained example direct object emission",
                &path,
                move || emit_host_native_object(&mir),
            )
            .unwrap_or_else(|error| panic!("{error}"));
            match emission {
                Ok(_) => emitted_ok += 1,
                Err(_) => {}
            }
        }

        let run_result = run_corpus_case_with_timeout("maintained example public run", &path, {
            let path = path.clone();
            move || run_path(&path)
        })
        .unwrap_or_else(|error| panic!("{error}"));
        match run_result {
            Ok(_) | Err(_) => run_completed += 1,
        }

        let explicit_mir_result =
            run_corpus_case_with_timeout("maintained example explicit MIR run", &path, {
                let path = path.clone();
                move || lower_path_to_mir(&path).and_then(|mir| run_mir(&mir))
            })
            .unwrap_or_else(|error| panic!("{error}"));
        match explicit_mir_result {
            Ok(_) | Err(_) => explicit_mir_completed += 1,
        }
    }

    assert!(
        file_count >= 80,
        "expected maintained example tree to stay broad"
    );
    assert!(
        checked_ok > 0,
        "expected some maintained examples to type-check"
    );
    assert!(
        lowered_ok > 0,
        "expected some maintained examples to lower to MIR"
    );
    assert!(
        emitted_ok > 0,
        "expected some maintained examples to emit native direct objects"
    );
    assert!(
        run_completed > 0 && explicit_mir_completed > 0,
        "expected maintained examples to exercise both runtime paths"
    );
}

#[test]
fn public_run_path_runs_queues_example_natively() {
    let source = include_str!("../../../examples/concurrency/task_group_start.au");
    let output = run_source(source).expect("queue example should run through the public run path");
    assert_eq!(output.stdout, "2\n4\n6\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn public_run_path_executes_file_io_example_with_path_context() {
    let fixture = repo_root().join("examples/io/read_text_file.au");
    let _guard = lock_io_example();
    let output =
        run_path(&fixture).expect("file io example should run through the public run path");
    assert_eq!(output.stdout, "true\ntrue\n");
    assert_eq!(output.value, zero_exit_value());
}

#[cfg(unix)]
#[test]
fn public_run_path_executes_unix_and_tls_example_with_path_context() {
    let fixture = repo_root().join("examples/io/unix_tls_roundtrip.au");
    let _guard = lock_io_example();
    let output =
        run_path(&fixture).expect("unix/tls example should run through the public run path");
    assert_eq!(output.stdout, "unix:ping\n9\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn mir_lowering_creates_blocks_for_control_flow() {
    let mir = lower_source_to_mir(CONTROL_FLOW_SOURCE).expect("control flow MIR should lower");
    let script = mir
        .top_level
        .expect("top-level script MIR should be present for control flow example");

    assert!(script.blocks.len() >= 4);
    assert!(script
        .blocks
        .iter()
        .any(|block| block.label.contains("while_cond")));
    assert!(script
        .blocks
        .iter()
        .any(|block| block.label.contains("if_then")));
}

#[test]
fn categorized_examples_type_check() {
    for (path, source) in EXAMPLE_CASES {
        check_source(source).unwrap_or_else(|error| {
            panic!("{} should type-check: {}", path, error);
        });
    }
}

#[test]
fn categorized_examples_run_with_expected_output() {
    let cases = [
            (
                "examples/basics/top_level_script.au",
                EXAMPLE_CASES[0].1,
                "156\n",
            ),
            (
                "examples/basics/main_function.au",
                EXAMPLE_CASES[1].1,
                "16\n",
            ),
            (
                "examples/basics/mutable_bindings.au",
                EXAMPLE_CASES[2].1,
                "5\n",
            ),
            (
                "examples/basics/default_arguments.au",
                EXAMPLE_CASES[3].1,
                "hello world\nhello aura\n6\n12\n",
            ),
            ("examples/basics/pass_keyword.au", EXAMPLE_CASES[4].1, "0\n"),
            (
                "examples/classes/point_distance.au",
                EXAMPLE_CASES[5].1,
                "5.0\n",
            ),
            (
                "examples/classes/default_fields.au",
                EXAMPLE_CASES[6].1,
                "localhost\n8080\n",
            ),
            (
                "examples/classes/methods.au",
                EXAMPLE_CASES[7].1,
                "4\n8\n0\n",
            ),
            (
                "examples/control_flow/if_elif_else.au",
                EXAMPLE_CASES[8].1,
                "high\n",
            ),
            (
                "examples/control_flow/for_range.au",
                EXAMPLE_CASES[9].1,
                "7\n",
            ),
            (
                "examples/control_flow/while_break_continue.au",
                EXAMPLE_CASES[10].1,
                "ok\n",
            ),
            (
                "examples/enums/result_match.au",
                EXAMPLE_CASES[11].1,
                "42\nbad\n0\n",
            ),
            (
                "examples/enums/result_option.au",
                EXAMPLE_CASES[12].1,
                "4\ndivision by zero\n7\n",
            ),
            (
                "examples/enums/explicit_type_args.au",
                EXAMPLE_CASES[13].1,
                "7\nbad\n",
            ),
            (
                "examples/generics/box_and_wrapper.au",
                EXAMPLE_CASES[14].1,
                "7\nok\n",
            ),
            (
                "examples/traits/greeter.au",
                EXAMPLE_CASES[15].1,
                "hello aura\nhello aura\n",
            ),
            (
                "examples/traits/multiple_bounds.au",
                EXAMPLE_CASES[16].1,
                "9\n",
            ),
            (
                "examples/numbers/float_sqrt.au",
                EXAMPLE_CASES[17].1,
                "9.0\n",
            ),
            (
                "examples/numbers/float32_values.au",
                EXAMPLE_CASES[18].1,
                "3.25\n2.0\n5.0\n",
            ),
            (
                "examples/numbers/numeric_casts.au",
                EXAMPLE_CASES[19].1,
                "7\n3.0\n1.25\n2.0\n",
            ),
            (
                "examples/strings/greeting.au",
                EXAMPLE_CASES[20].1,
                "hello, aura\n",
            ),
            (
                "examples/concurrency/task_group_queue_sum.au",
                EXAMPLE_CASES[21].1,
                "3\n",
            ),
            (
                "examples/concurrency/task_group_cancel.au",
                EXAMPLE_CASES[22].1,
                "0\n1\n",
            ),
            (
                "examples/concurrency/queue_get_timeout.au",
                EXAMPLE_CASES[23].1,
                "Option.None\n",
            ),
            (
                "examples/concurrency/sleep_builtin.au",
                EXAMPLE_CASES[24].1,
                "start\nend\n",
            ),
            (
                "examples/concurrency/send_result.au",
                EXAMPLE_CASES[25].1,
                "7\n",
            ),
            (
                "examples/concurrency/bounded_queue.au",
                EXAMPLE_CASES[26].1,
                "queued 1\nqueued 2\n3\n",
            ),
            (
                "examples/concurrency/task_group_start_soon.au",
                EXAMPLE_CASES[27].1,
                "9\n",
            ),
            (
                "examples/concurrency/queue_put_timeout.au",
                EXAMPLE_CASES[28].1,
                "sent\n4\n",
            ),
            (
                "examples/enums/wildcard_match.au",
                EXAMPLE_CASES[29].1,
                "2\n",
            ),
            (
                "examples/generics/generic_method_calls.au",
                EXAMPLE_CASES[30].1,
                "7\n",
            ),
            (
                "examples/generics/bounded_types.au",
                EXAMPLE_CASES[31].1,
                "aura\nempty\n",
            ),
            (
                "examples/traits/marker_trait.au",
                EXAMPLE_CASES[32].1,
                "1\n",
            ),
            (
                "examples/traits/specialized_generic_impl.au",
                EXAMPLE_CASES[33].1,
                "hello\n",
            ),
            (
                "examples/concurrency/minute_duration.au",
                EXAMPLE_CASES[34].1,
                "120000ms\n",
            ),
            (
                "examples/traits/generic_dispatch_multiple_types.au",
                EXAMPLE_CASES[35].1,
                "dog\ncat\n",
            ),
            (
                "examples/strings/string_methods.au",
                EXAMPLE_CASES[36].1,
                "13\ntrue\ntrue\ntrue\naura repo\n2\naura\nrepo\naura lang\naura repo\nAURA REPO\nrepo\nnone\naura\nnone\n9\n",
            ),
            (
                "examples/numbers/numeric_builtins.au",
                EXAMPLE_CASES[37].1,
                "7\n3.5\n2\n12\n9.0\n9.0\n",
            ),
            (
                "examples/collections/dict_basics.au",
                EXAMPLE_CASES[38].1,
                "3\ntrue\n1\n1\n5\n(aura, 5)\n(repo, 3)\n3\n3\n3\ntrue\n",
            ),
            (
                "examples/collections/set_basics.au",
                EXAMPLE_CASES[39].1,
                "3\ntrue\nfalse\ntrue\ntrue\n9\ntrue\ntrue\n1\n",
            ),
            (
                "examples/strings/string_parsing_and_formatting.au",
                EXAMPLE_CASES[40].1,
                "42\n-9000000000\n3.5\ntrue\naura-lang-tests\ntrue\n12\n4\n9\n3.0\n",
            ),
            (
                "examples/traits/generic_trait_bounds.au",
                EXAMPLE_CASES[41].1,
                "20\n",
            ),
            (
                "examples/traits/operator_traits.au",
                EXAMPLE_CASES[42].1,
                "6\n8\n-6\n-8\n",
            ),
            (
                "examples/traits/ordering_traits.au",
                EXAMPLE_CASES[43].1,
                "true\ntrue\ntrue\ntrue\n2\n",
            ),
            (
                "examples/basics/copy_return_selection.au",
                EXAMPLE_CASES[44].1,
                "7\n",
            ),
            (
                "examples/io/process_run.au",
                EXAMPLE_CASES[45].1,
                "aura process\n13\n0\nExitStatus.Exited(0)\n",
            ),
            (
                "examples/io/process_pipes.au",
                EXAMPLE_CASES[46].1,
                "ping\nExitStatus.Exited(0)\n",
            ),
        ];

    for (path, source, expected_stdout) in cases {
        let output = run_source(source).unwrap_or_else(|error| {
            panic!("{} should run: {}", path, error);
        });
        assert_eq!(
            output.stdout, expected_stdout,
            "unexpected stdout for {}",
            path
        );
    }
}

#[test]
fn additional_categorized_examples_type_check() {
    for (path, source, _) in ADDITIONAL_EXAMPLE_CASES {
        check_source(source).unwrap_or_else(|error| {
            panic!("{} should type-check: {}", path, error);
        });
    }
}

#[test]
fn additional_categorized_examples_run_with_expected_output() {
    let _guard = lock_io_example();
    for (path, source, expected_stdout) in ADDITIONAL_EXAMPLE_CASES {
        let output = run_source(source).unwrap_or_else(|error| {
            panic!("{} should run: {}", path, error);
        });
        assert_eq!(
            output.stdout, *expected_stdout,
            "unexpected stdout for {}",
            path
        );
    }
}

#[test]
fn runtime_member_surface_matrix_runs_consistently_through_public_and_explicit_mir_paths() {
    let source = r#"
def worker(value: int32) -> int32:
    return value + 1

def main() -> int32:
    text = "  Aura Repo  "
    trimmed = text.trim()
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
    print(clone_numbers.remove(9))
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

    let public_output = run_source(source).expect("runtime member matrix should run");
    let mir = lower_source_to_mir(source).expect("runtime member matrix should lower to MIR");
    let explicit_output = run_mir(&mir).expect("runtime member matrix should run via MIR");

    assert_eq!(explicit_output.value, public_output.value);
    assert_eq!(explicit_output.stdout, public_output.stdout);
}

#[test]
fn runtime_call_writeback_and_cleanup_surface_runs_consistently_through_public_and_explicit_mir_paths(
) {
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
    print(jobs.get())
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

    let public_output = run_source(source)
        .expect("writeback/cleanup matrix should run through the public run path");
    let mir =
        lower_source_to_mir(source).expect("writeback/cleanup matrix should lower to explicit MIR");
    let explicit_output =
        run_mir(&mir).expect("writeback/cleanup matrix should run via explicit MIR runtime");

    assert_eq!(explicit_output.value, public_output.value);
    assert_eq!(explicit_output.stdout, public_output.stdout);
}

#[test]
fn cancellation_wakes_sleep_tasks_promptly() {
    let blocked_sleep = crate::hosted_ci_timing_limit(StdDuration::from_millis(250));
    let source = format!(
        r#"
def sleeper(started: Queue[str], finished: Queue[str]) -> None:
    started.put("sleep")
    sleep({blocked_sleep_ms}ms)
    finished.put("sleep")

def wait_for_one(queue: Queue[str]):
    while true:
        match queue.get():
            case QueueReceive.Item(_):
                return
            case QueueReceive.Closed:
                return
            case QueueReceive.TimedOut:
                pass
            case QueueReceive.Cancelled:
                pass

def main() -> int32:
    started = Queue[str]()
    with TaskGroup() as group:
        finished = Queue[str]()
        group.start(sleeper, started, finished)
        wait_for_one(started)
        group.cancel()
    return 0
"#,
        blocked_sleep_ms = blocked_sleep.as_millis()
    );

    let start = Instant::now();
    let output = run_source(&source).expect("sleep cancellation source should run");
    let elapsed = start.elapsed();

    assert_eq!(output.value, Value::Int(IntegerValue::from_signed(0)));
    assert!(
        elapsed < crate::hosted_ci_timing_limit(StdDuration::from_millis(120)),
        "sleep cancellation should return promptly; elapsed {:?}",
        elapsed
    );
}

#[test]
fn cancellation_wakes_queue_wait_tasks_promptly() {
    let blocked_wait = crate::hosted_ci_timing_limit(StdDuration::from_millis(250));
    let source = format!(
        r#"
def waiter(started: Queue[str], jobs: Queue[int32], finished: Queue[str]) -> None:
    started.put("wait")
    while not cancelled():
        match jobs.get(timeout={blocked_wait_ms}ms):
            case QueueReceive.Item(_):
                pass
            case QueueReceive.TimedOut:
                pass
            case QueueReceive.Closed:
                pass
            case QueueReceive.Cancelled:
                pass
    finished.put("wait")

def wait_for_one(queue: Queue[str]):
    while true:
        match queue.get():
            case QueueReceive.Item(_):
                return
            case QueueReceive.Closed:
                return
            case QueueReceive.TimedOut:
                pass
            case QueueReceive.Cancelled:
                pass

def main() -> int32:
    started = Queue[str]()
    finished = Queue[str]()
    jobs = Queue[int32]()
    with TaskGroup() as group:
        group.start(waiter, started, jobs, finished)
        wait_for_one(started)
        group.cancel()
    wait_for_one(finished)
    return 0
"#,
        blocked_wait_ms = blocked_wait.as_millis()
    );

    let start = Instant::now();
    let output = run_source(&source).expect("queue-wait cancellation source should run");
    let elapsed = start.elapsed();

    assert_eq!(output.value, Value::Int(IntegerValue::from_signed(0)));
    assert!(
        elapsed < crate::hosted_ci_timing_limit(StdDuration::from_millis(120)),
        "queue wait cancellation should return promptly; elapsed {:?}",
        elapsed
    );
}

#[test]
fn bounded_queue_blocks_second_put_until_capacity_frees() {
    let temp = TempDir::new("aura-bounded-queue");
    let consumer_drained_path = temp.path().join("consumer-drained.txt");
    let second_put_started_path = temp.path().join("second-put-started.txt");
    let second_put_finished_path = temp.path().join("second-put-finished.txt");
    let release_consumer_path = temp.path().join("release-consumer.txt");
    let consumer_drained_literal = escape_aura_string(&consumer_drained_path.display().to_string());
    let second_put_started_literal =
        escape_aura_string(&second_put_started_path.display().to_string());
    let second_put_finished_literal =
        escape_aura_string(&second_put_finished_path.display().to_string());
    let release_consumer_literal = escape_aura_string(&release_consumer_path.display().to_string());
    let source = format!(
        r#"
import fs

def consumer(
    jobs: Queue[int32],
    after_get_path: str,
    release_path: str
) -> None:
    while not fs.exists(release_path):
        sleep(5ms)
    match jobs.get():
        case QueueReceive.Item(_):
            pass
        case QueueReceive.Closed:
            pass
        case QueueReceive.TimedOut:
            pass
        case QueueReceive.Cancelled:
            pass
    match fs.write_string(after_get_path, "drained"):
        case Result.Ok(_):
            pass
        case Result.Err(_):
            pass

def main() -> int32:
    jobs = Queue[int32](capacity=1)
    with TaskGroup() as group:
        group.start(
            consumer,
            jobs,
            "{consumer_drained_path}",
            "{release_consumer_path}"
        )
        match jobs.put(1):
            case Result.Ok(_):
                pass
            case Result.Err(_):
                return 1
        match fs.write_string("{second_put_started_path}", "before-second"):
            case Result.Ok(_):
                pass
            case Result.Err(_):
                return 2
        match jobs.put(2):
            case Result.Ok(_):
                pass
            case Result.Err(_):
                return 3
        match fs.write_string("{second_put_finished_path}", "finished"):
            case Result.Ok(_):
                pass
            case Result.Err(_):
                return 4
        print("second-put-finished")
    return 0
"#,
        consumer_drained_path = consumer_drained_literal,
        second_put_started_path = second_put_started_literal,
        second_put_finished_path = second_put_finished_literal,
        release_consumer_path = release_consumer_literal
    );

    let handle = thread::spawn(move || run_source(&source));
    let deadline = Instant::now() + StdDuration::from_secs(3);
    while Instant::now() < deadline && !second_put_started_path.exists() {
        thread::sleep(StdDuration::from_millis(5));
    }

    let second_put_started = second_put_started_path.exists();
    let second_put_finished_before_release = second_put_finished_path.exists();
    fs::write(&release_consumer_path, "release")
        .expect("the host should release the queue consumer");
    let output = handle
        .join()
        .expect("bounded queue runtime thread should join")
        .expect("bounded queue source should run");

    assert!(
        second_put_started,
        "bounded queue source never reached the second put"
    );
    assert!(
        !second_put_finished_before_release,
        "second put should remain blocked until the consumer is released"
    );
    assert!(
        consumer_drained_path.exists(),
        "the released consumer should drain the first queued value"
    );
    assert!(
        second_put_finished_path.exists(),
        "the second put should finish after the consumer frees capacity"
    );
    assert_eq!(output.value, Value::Int(IntegerValue::from_signed(0)));
    assert_eq!(output.stdout.trim(), "second-put-finished");
}

#[cfg(unix)]
#[test]
fn async_file_io_keeps_the_scheduler_running_while_a_fifo_read_waits() {
    let _guard = lock_io_example();
    let temp = TempDir::new("aura-async-file-io");
    let fifo_path = temp.path().join("events.fifo");
    let ready_path = temp.path().join("ready.txt");
    let fifo_literal = escape_aura_string(&fifo_path.display().to_string());
    let ready_literal = escape_aura_string(&ready_path.display().to_string());
    let read_timeout = crate::hosted_ci_timing_limit(StdDuration::from_secs(2));

    let status = Command::new("mkfifo")
        .arg(&fifo_path)
        .status()
        .expect("mkfifo should be available");
    assert!(status.success(), "mkfifo should succeed");

    let source = format!(
        r#"
import fs

def wait_for_text(path: str):
    match fs.read_to_string(path):
        case Result.Ok(text):
            print(text)
        case Result.Err(err):
            print(err)

def mark_ready(path: str):
    sleep(20ms)
    match fs.write_string(path, "ready"):
        case Result.Ok(_):
            pass
        case Result.Err(_):
            pass

def main() -> int32:
    with TaskGroup() as group:
        reader = group.start(wait_for_text, "{fifo_path}")
        group.start_soon(mark_ready, "{ready_path}")
        match reader.result(timeout={read_timeout_ms}ms):
            case TaskResult.Ready(_):
                pass
            case TaskResult.Error(_message):
                pass
            case TaskResult.Cancelled:
                pass
            case TaskResult.TimedOut:
                pass
    return 0
"#,
        fifo_path = fifo_literal,
        ready_path = ready_literal,
        read_timeout_ms = read_timeout.as_millis()
    );

    let writer_path = fifo_path.clone();
    let writer = thread::spawn(move || {
        thread::sleep(crate::hosted_ci_timing_limit(StdDuration::from_millis(500)));
        let mut fifo = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(writer_path)
            .expect("fifo writer should open the fifo");
        fifo.write_all(b"ping").expect("fifo writer should succeed");
    });

    let handle = thread::spawn(move || run_source(&source));
    let ready_deadline =
        Instant::now() + crate::hosted_ci_timing_limit(StdDuration::from_millis(350));
    let mut ready_seen = false;
    while Instant::now() < ready_deadline {
        if ready_path.exists() {
            ready_seen = true;
            break;
        }
        thread::sleep(StdDuration::from_millis(5));
    }

    writer.join().expect("fifo writer thread should join");
    let output = handle
        .join()
        .expect("async file I/O runtime thread should join")
        .expect("async file I/O source should run");

    assert!(
        ready_seen,
        "scheduler should keep running other tasks while a FIFO read waits"
    );
    assert_eq!(output.value, Value::Int(IntegerValue::from_signed(0)));
    assert_eq!(output.stdout.trim(), "ping");
}

#[test]
fn lightweight_tasks_scale_to_thousands_of_waiting_tasks() {
    let temp = TempDir::new("aura-lightweight-task-scale");
    let ready_path = temp.path().join("ready.txt");
    let ready_literal = escape_aura_string(&ready_path.display().to_string());
    let source = format!(
        r#"
import fs

def sleeper(started: Queue[int32]) -> None:
    started.put(1)
    while not cancelled():
        sleep(10s)

def wait_for_count(queue: Queue[int32], expected: int32):
    mut seen: int32 = 0
    while seen < expected:
        match queue.get():
            case QueueReceive.Item(_):
                seen = seen + 1
            case QueueReceive.Closed:
                pass
            case QueueReceive.TimedOut:
                pass
            case QueueReceive.Cancelled:
                pass

def main() -> int32:
    started = Queue[int32]()
    with TaskGroup() as group:
        for i in range(3000):
            group.start(sleeper, started)
        wait_for_count(started, 3000)
        match fs.write_string("{ready_path}", "ready"):
            case Result.Ok(_):
                sleep(500ms)
            case Result.Err(_):
                group.cancel()
                return 1
        group.cancel()
    return 0
"#,
        ready_path = ready_literal
    );

    let baseline_threads = current_process_thread_count();
    let source_handle = thread::spawn(move || run_source(&source));
    let deadline = Instant::now() + StdDuration::from_secs(15);
    while !ready_path.exists() {
        assert!(
            Instant::now() < deadline,
            "lightweight task stress never signalled readiness"
        );
        thread::sleep(StdDuration::from_millis(10));
    }
    let running_threads = current_process_thread_count();
    let output = source_handle
        .join()
        .expect("lightweight task stress worker should not panic")
        .expect("lightweight task stress source should run");
    assert_eq!(output.value, Value::Int(IntegerValue::from_signed(0)));
    assert!(
        running_threads <= baseline_threads + 32,
        "lightweight task stress should stay near the baseline thread count; baseline {}, running {}",
        baseline_threads,
        running_threads
    );
}

#[test]
fn additional_module_examples_run_with_expected_output() {
    let cases = [
        ("examples/modules/namespace_import_types.au", "4\ntrue\n1\n"),
        ("examples/modules/trait_impl_imports.au", "Ada\nAda\n"),
    ];

    for (relative_path, expected_stdout) in cases {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../")
            .join(relative_path);
        check_path(&path).unwrap_or_else(|error| {
            panic!("{} should type-check: {}", relative_path, error);
        });
        let output = run_path(&path).unwrap_or_else(|error| {
            panic!("{} should run: {}", relative_path, error);
        });
        assert_eq!(
            output.stdout, expected_stdout,
            "unexpected stdout for {}",
            relative_path
        );
    }
}

#[test]
fn module_example_runs_with_expected_output() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/modules/simple_import.au");
    let output = run_path(&path).expect("module example should run");
    assert_eq!(output.stdout, "10\n2\n");
    assert_eq!(output.value, zero_exit_value());
}

#[test]
fn lib_helper_paths_cover_relative_paths_missing_reads_and_import_qualification() {
    let relative = Path::new("examples/basics/main_function.au");
    assert_eq!(
        absolutize(relative),
        std::env::current_dir()
            .expect("cwd should be available")
            .join(relative)
    );

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be monotonic enough")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("aura-lib-coverage-{}", unique));
    fs::create_dir_all(&temp_dir).expect("temp dir should be created");

    let read_error = check_path(&temp_dir).expect_err("directories should not be readable");
    assert!(read_error.message.contains("failed to read"));

    let imported = check_source("public class Imported:\n    value: int32\n")
        .expect("imported module should type-check");
    let mut program =
        check_source("def main() -> None:\n    pass\n").expect("program should type-check");
    program.imported_modules.insert(
        "dep".to_string(),
        exported_namespace(&["dep".to_string()], &imported),
    );

    let qualified = qualify_export_type(
        &program,
        &crate::sema::Type::Named("Imported".to_string(), Vec::new()),
    );
    assert_eq!(
        qualified,
        crate::sema::Type::Named("dep.Imported".to_string(), Vec::new())
    );

    let qualified_ref = qualify_export_type_ref(
        &program,
        &TypeRef::named("Imported", Vec::new(), false, Span::new(1, 1)),
    );
    assert_eq!(named_ref_name(&qualified_ref), "dep.Imported");

    let unknown = qualify_export_type(
        &program,
        &crate::sema::Type::Named("Unknown".to_string(), Vec::new()),
    );
    assert_eq!(
        unknown,
        crate::sema::Type::Named("Unknown".to_string(), Vec::new())
    );
    assert_eq!(
        qualify_export_type(&program, &crate::sema::Type::Module("pkg.dep".to_string())),
        crate::sema::Type::Module("pkg.dep".to_string())
    );
    assert_eq!(
        qualify_export_type(&program, &crate::sema::Type::Unit),
        crate::sema::Type::Unit
    );

    let bounds = BTreeMap::from([(
        "T".to_string(),
        vec![TypeRef::named(
            "Imported",
            Vec::new(),
            false,
            Span::new(2, 3),
        )],
    )]);
    let qualified_bounds = super::qualify_export_bounds(&program, &bounds);
    assert_eq!(named_ref_name(&qualified_bounds["T"][0]), "dep.Imported");
}

#[test]
fn maintained_hello_world_example_runs() {
    let source = include_str!("../../../examples/basics/hello_world.au");
    assert_eq!(crate::run_source(source).unwrap().stdout, "Hello, world!\n");
}
