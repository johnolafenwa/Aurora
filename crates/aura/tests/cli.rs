use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use aura_compiler::ast::BinaryOp;
use aura_compiler::mir::{
    BasicBlock, CallTarget, Instruction, MirArg, MirClass, MirClassField, MirClosureCapture,
    MirFieldInit, MirFunction, MirLocalType, MirModule, MirParam, MirReceiverKind, Operand, Rvalue,
    Terminator,
};
use aura_compiler::sema::{ClosureCallKind, ClosureCapture, ClosureCaptureMode, Type};
use aura_compiler::Span;

#[cfg(unix)]
use rcgen::generate_simple_self_signed;

const FILESYSTEM_READ_CAP_BYTES: usize = 256 * 1024 * 1024;
const RETIRED_FILESYSTEM_READ_CAP_BYTES: usize = 64 * 1024 * 1024;

#[cfg(unix)]
fn serialize_bounded_blocking_pool_watchdog() -> std::sync::MutexGuard<'static, ()> {
    static WATCHDOG_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

    // Both tests deliberately saturate a single-runtime-worker blocking pool
    // across MIR, direct, and standalone processes. Running the two stress
    // harnesses together makes their watchdogs measure mutual host contention
    // instead of the admission/cancellation behavior under test.
    WATCHDOG_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn timing_millis_for_hosted_ci(local_millis: u64, hosted_ci: bool) -> u64 {
    if hosted_ci {
        local_millis.saturating_mul(4)
    } else {
        local_millis
    }
}

fn hosted_ci_timing_millis(local_millis: u64) -> u64 {
    timing_millis_for_hosted_ci(local_millis, std::env::var_os("GITHUB_ACTIONS").is_some())
}

#[test]
fn hosted_ci_safepoint_windows_scale_without_changing_local_windows() {
    assert_eq!(timing_millis_for_hosted_ci(100, false), 100);
    assert_eq!(timing_millis_for_hosted_ci(100, true), 400);
}

#[test]
fn check_reports_match_expression_and_literal_pattern_diagnostics() {
    let fixture = repo_root().join(
        "crates/aura-compiler/tests/fixtures/check-fail/match_expression_class_pattern_deferred.au",
    );
    let output = Command::new(aura_bin())
        .arg("check")
        .arg(&fixture)
        .output()
        .expect("failed to run aura check");

    assert!(
        !output.status.success(),
        "class patterns must remain rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[AU2999]"), "stderr was:\n{stderr}");
    assert!(
        stderr.contains(
            "class patterns are not supported; match an explicit enum/tag representation or use a wildcard and ordinary code"
        ),
        "stderr was:\n{stderr}"
    );
    assert!(stderr.contains(":6:14"), "stderr was:\n{stderr}");

    let duplicate_literal = repo_root()
        .join("crates/aura-compiler/tests/fixtures/check-fail/match_duplicate_literal.au");
    let output = Command::new(aura_bin())
        .arg("check")
        .arg(&duplicate_literal)
        .output()
        .expect("failed to run aura check");

    assert!(
        !output.status.success(),
        "duplicate literal patterns must remain rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[AU2999]"), "stderr was:\n{stderr}");
    assert!(
        stderr.contains("duplicate match arm for literal `1` (previously matched at 3:14)"),
        "stderr was:\n{stderr}"
    );
    assert!(stderr.contains(":5:14"), "stderr was:\n{stderr}");
}

#[cfg(unix)]
fn hold_native_runtime_build_locks(target_dir: &std::path::Path) -> Vec<fs::File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;

    // A normal source-checkout binary locks its selected Cargo target.
    // Coverage launches an instrumented `aura`, which deliberately builds and
    // locks an uninstrumented runtime in a separate repository target. Hold
    // both real production paths so this behavioral test is profile-agnostic.
    let mut target_dirs = vec![
        target_dir.to_path_buf(),
        repo_root().join("target/native-runtime-uninstrumented"),
    ];
    target_dirs.sort();
    target_dirs.dedup();

    target_dirs
        .into_iter()
        .map(|target_dir| {
            fs::create_dir_all(&target_dir).expect("native runtime target directory should exist");
            let lock_path = target_dir.join(".aura-native-runtime-build.lock");
            let held_lock = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(&lock_path)
                .expect("native runtime lock should be openable");
            fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))
                .expect("native runtime lock should be private");
            assert_eq!(
                unsafe { libc::flock(held_lock.as_raw_fd(), libc::LOCK_EX) },
                0,
                "the test should hold each real source-checkout build barrier"
            );
            held_lock
        })
        .collect()
}

fn aura_bin() -> &'static str {
    env!("CARGO_BIN_EXE_aura")
}

fn generated_binary(path: &PathBuf) -> Command {
    let mut command = Command::new(path);
    if std::env::var_os("LLVM_PROFILE_FILE").is_some() {
        #[cfg(unix)]
        command.env("LLVM_PROFILE_FILE", "/dev/null");
        #[cfg(windows)]
        command.env("LLVM_PROFILE_FILE", "NUL");
    }
    command
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .unwrap()
        .to_path_buf()
}

fn native_runtime_archive() -> PathBuf {
    let target_dir = if std::env::var_os("LLVM_PROFILE_FILE").is_some() {
        repo_root().join("target/native-runtime-uninstrumented")
    } else {
        match std::env::var_os("CARGO_TARGET_DIR") {
            Some(target) if std::path::Path::new(&target).is_absolute() => PathBuf::from(target),
            Some(target) => repo_root().join(target),
            None => repo_root().join("target"),
        }
    };
    target_dir
        .join(if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        })
        .join("libaura_compiler.a")
}

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

fn assert_default_backend_example_runs(example: &str, binary_name: &str, expected_stdout: &str) {
    let fixture = repo_root().join(example);
    let output_dir = TempDir::new("aura-build-auto-full");
    let output_path = output_dir.path().join(binary_name);

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg(&fixture)
        .output()
        .expect("failed to run aura build");

    assert!(
        build.status.success(),
        "default build should support {}, stderr was:\n{}",
        example,
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run built binary");

    assert!(
        run.status.success(),
        "built binary for {} should exit successfully, stderr was:\n{}",
        example,
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expected_stdout);
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
) -> Option<std::process::ExitStatus> {
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("failed to poll child process") {
            return Some(status);
        }
        if start.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn command_output_with_timeout(
    mut command: Command,
    timeout: std::time::Duration,
    context: &str,
) -> std::process::Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("{context}: failed to spawn command: {error}"));
    let mut stdout_pipe = child.stdout.take().expect("captured stdout should exist");
    let mut stderr_pipe = child.stderr.take().expect("captured stderr should exist");
    let stdout_reader = std::thread::spawn(move || {
        let mut stdout = Vec::new();
        stdout_pipe
            .read_to_end(&mut stdout)
            .expect("captured stdout should be readable");
        stdout
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut stderr = Vec::new();
        stderr_pipe
            .read_to_end(&mut stderr)
            .expect("captured stderr should be readable");
        stderr
    });

    let status = wait_with_timeout(&mut child, timeout);
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let stdout = stdout_reader
        .join()
        .expect("stdout reader should not panic");
    let stderr = stderr_reader
        .join()
        .expect("stderr reader should not panic");
    let Some(status) = status else {
        panic!(
            "{context}: command did not finish within {timeout:?}; stdout was:\n{}\nstderr was:\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    };

    std::process::Output {
        status,
        stdout,
        stderr,
    }
}

fn assert_direct_backend_example_runs(example: &str, binary_name: &str, expected_stdout: &str) {
    let fixture = repo_root().join(example);
    let output_dir = TempDir::new("aura-build-direct-full");
    let output_path = output_dir.path().join(binary_name);

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&fixture)
        .output()
        .expect("failed to run aura build --backend direct");

    assert!(
        build.status.success(),
        "direct backend should support {}, stderr was:\n{}",
        example,
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run direct-backend binary");

    assert!(
        run.status.success(),
        "direct-backend binary for {} should exit successfully, stderr was:\n{}",
        example,
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expected_stdout);
}

fn write_temp_source(prefix: &str, source: &str) -> (TempDir, PathBuf) {
    let temp = TempDir::new(prefix);
    let source_path = temp.path().join("main.au");
    fs::write(&source_path, source).expect("failed to write temporary Aura source");
    (temp, source_path)
}

#[test]
fn ast_json_preserves_named_and_loop_shapes_while_exposing_tuples() {
    let source = [
        "def named(items: list[int32]) -> int32:",
        "    for item in items:",
        "        pass",
        "    return 0",
        "",
        "def tupled(items: list[(int32, str)]) -> (int32, str):",
        "    for (number, text) in items:",
        "        return (number, text)",
        "    return (0, \"\")",
    ]
    .join("\n");

    let mut child = Command::new(aura_bin())
        .arg("ast-json")
        .arg("--stdin")
        .arg("/virtual/tuple_ast.au")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura ast-json");
    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write tuple AST source");
    let output = child
        .wait_with_output()
        .expect("failed to collect aura ast-json output");
    assert!(
        output.status.success(),
        "ast-json should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ast-json should return valid JSON");
    let named = &json["items"][0]["Function"];
    let named_type = &named["params"][0]["ty"];
    assert_eq!(named_type["name"], "list");
    assert_eq!(named_type["args"][0]["name"], "int32");
    assert!(
        named_type.get("kind").is_none(),
        "named type references must retain the pre-tuple JSON shape"
    );
    let simple_loop = &named["body"][0]["For"];
    assert_eq!(simple_loop["binding"], "item");
    assert!(
        simple_loop.get("target").is_none(),
        "simple loops must retain the pre-tuple `binding` field"
    );

    let tupled = &json["items"][1]["Function"];
    let tuple_parameter = &tupled["params"][0]["ty"]["args"][0];
    assert_eq!(tuple_parameter["elements"][0]["name"], "int32");
    assert_eq!(tuple_parameter["elements"][1]["name"], "str");
    assert_eq!(
        tupled["return_type"]["elements"].as_array().map(Vec::len),
        Some(2)
    );
    let tuple_loop = &tupled["body"][0]["For"];
    assert!(tuple_loop.get("binding").is_none());
    assert_eq!(
        tuple_loop["target"]["Tuple"]["elements"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
}

#[test]
fn generic_tuple_substitution_runs_in_mir_and_direct_backends() {
    let source = [
        "def swap[T, U](pair: own (T, U)) -> (U, T):",
        "    left, right = pair",
        "    return (right, left)",
        "",
        "def main() -> int32:",
        "    result = swap((7, \"seven\"))",
        "    label, number = result",
        "    print(label)",
        "    print(number)",
        "    return 0",
    ]
    .join("\n");

    assert_run_and_direct_source_stdout("aura-cli-generic-tuples", &source, "seven\n7\n");
}

fn build_and_run_direct_source(
    prefix: &str,
    source: &str,
) -> (std::process::Output, std::process::Output) {
    let (temp, source_path) = write_temp_source(prefix, source);
    let output_path = temp.path().join("out");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build --backend direct");

    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run direct-backend binary");

    (build, run)
}

fn build_and_run_default_source(
    prefix: &str,
    source: &str,
) -> (std::process::Output, std::process::Output) {
    let (temp, source_path) = write_temp_source(prefix, source);
    let output_path = temp.path().join("out");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build");

    assert!(
        build.status.success(),
        "default backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run default-backend binary");

    (build, run)
}

fn assert_run_and_direct_source_stdout(prefix: &str, source: &str, expected_stdout: &str) {
    let (temp, source_path) = write_temp_source(prefix, source);
    let run = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run aura run");
    assert!(
        run.status.success(),
        "aura run should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expected_stdout);

    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build --backend direct");
    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let direct = generated_binary(&output_path)
        .output()
        .expect("failed to run direct-backend binary");
    assert!(
        direct.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&direct.stdout), expected_stdout);
}

#[cfg(unix)]
fn adr0038_cfg_view_module(reverse_branch_storage: bool, include_dead_loan: bool) -> MirModule {
    let pair_type = Type::named("Pair");
    let mut update_blocks = vec![BasicBlock {
        label: "entry".to_string(),
        instructions: Vec::new(),
        terminator: Terminator::Branch {
            condition: Operand::Place("choose_left".to_string()),
            then_label: "left_begin".to_string(),
            else_label: "right_begin".to_string(),
        },
    }];
    let left_begin = BasicBlock {
        label: "left_begin".to_string(),
        instructions: vec![Instruction::BeginLoan {
            loan: "selected".to_string(),
            source: "pair.left".to_string(),
            mutable: true,
        }],
        terminator: Terminator::Goto("left_write".to_string()),
    };
    let right_begin = BasicBlock {
        label: "right_begin".to_string(),
        instructions: vec![Instruction::BeginLoan {
            loan: "selected".to_string(),
            source: "pair.right".to_string(),
            mutable: true,
        }],
        terminator: Terminator::Goto("right_write".to_string()),
    };
    let left_write = BasicBlock {
        label: "left_write".to_string(),
        instructions: vec![
            Instruction::WriteLoan {
                loan: "selected".to_string(),
                value: Rvalue::Use(Operand::Int(11)),
            },
            Instruction::EndLoan {
                loan: "selected".to_string(),
            },
        ],
        terminator: Terminator::Goto("exit".to_string()),
    };
    let right_write = BasicBlock {
        label: "right_write".to_string(),
        instructions: vec![
            Instruction::WriteLoan {
                loan: "selected".to_string(),
                value: Rvalue::Use(Operand::Int(22)),
            },
            Instruction::EndLoan {
                loan: "selected".to_string(),
            },
        ],
        terminator: Terminator::Goto("exit".to_string()),
    };
    if reverse_branch_storage {
        update_blocks.extend([right_begin, left_begin, right_write, left_write]);
    } else {
        update_blocks.extend([left_begin, right_begin, left_write, right_write]);
    }
    if include_dead_loan {
        update_blocks.insert(
            3,
            BasicBlock {
                label: "dead_loan_metadata".to_string(),
                instructions: vec![Instruction::BeginLoan {
                    loan: "selected".to_string(),
                    source: "pair.right".to_string(),
                    mutable: true,
                }],
                terminator: Terminator::Return(Operand::Unit),
            },
        );
    }
    update_blocks.push(BasicBlock {
        label: "exit".to_string(),
        instructions: Vec::new(),
        terminator: Terminator::Return(Operand::Unit),
    });

    let update = MirFunction {
        name: "update".to_string(),
        module_name: "<cfg-view-test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: vec![
            MirParam {
                name: "pair".to_string(),
                passing: MirReceiverKind::BorrowMut,
                ty: pair_type.clone(),
                default_function: None,
            },
            MirParam {
                name: "choose_left".to_string(),
                passing: MirReceiverKind::Borrow,
                ty: Type::named("bool"),
                default_function: None,
            },
        ],
        local_types: vec![
            MirLocalType {
                name: "pair".to_string(),
                ty: pair_type.clone(),
            },
            MirLocalType {
                name: "choose_left".to_string(),
                ty: Type::named("bool"),
            },
            MirLocalType {
                name: "selected".to_string(),
                ty: Type::named("int64"),
            },
        ],
        return_type: Type::Unit,
        entry: "entry".to_string(),
        blocks: update_blocks,
    };

    let call_update = |target: &str, choose_left| Instruction::Assign {
        target: target.to_string(),
        value: Rvalue::Call {
            callee: CallTarget::Name("update".to_string()),
            args: vec![
                MirArg {
                    name: None,
                    value: Operand::Place("pair".to_string()),
                    writeback_place: Some("pair".to_string()),
                },
                MirArg {
                    name: None,
                    value: Operand::Bool(choose_left),
                    writeback_place: None,
                },
            ],
        },
    };
    let main = MirFunction {
        name: "main".to_string(),
        module_name: "<cfg-view-test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: Vec::new(),
        local_types: vec![
            MirLocalType {
                name: "pair".to_string(),
                ty: pair_type.clone(),
            },
            MirLocalType {
                name: "left".to_string(),
                ty: Type::named("int64"),
            },
            MirLocalType {
                name: "right".to_string(),
                ty: Type::named("int64"),
            },
            MirLocalType {
                name: "call_left".to_string(),
                ty: Type::Unit,
            },
            MirLocalType {
                name: "call_right".to_string(),
                ty: Type::Unit,
            },
            MirLocalType {
                name: "print_left".to_string(),
                ty: Type::Unit,
            },
            MirLocalType {
                name: "print_right".to_string(),
                ty: Type::Unit,
            },
            MirLocalType {
                name: "sum".to_string(),
                ty: Type::named("int64"),
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
                            MirFieldInit {
                                name: "left".to_string(),
                                value: Operand::Int(1),
                            },
                            MirFieldInit {
                                name: "right".to_string(),
                                value: Operand::Int(2),
                            },
                        ],
                    },
                },
                call_update("call_left", true),
                call_update("call_right", false),
                Instruction::Assign {
                    target: "left".to_string(),
                    value: Rvalue::Member {
                        object: Operand::Place("pair".to_string()),
                        field: "left".to_string(),
                    },
                },
                Instruction::Assign {
                    target: "print_left".to_string(),
                    value: Rvalue::Call {
                        callee: CallTarget::Name("print".to_string()),
                        args: vec![MirArg {
                            name: None,
                            value: Operand::Place("left".to_string()),
                            writeback_place: None,
                        }],
                    },
                },
                Instruction::Assign {
                    target: "right".to_string(),
                    value: Rvalue::Member {
                        object: Operand::Place("pair".to_string()),
                        field: "right".to_string(),
                    },
                },
                Instruction::Assign {
                    target: "print_right".to_string(),
                    value: Rvalue::Call {
                        callee: CallTarget::Name("print".to_string()),
                        args: vec![MirArg {
                            name: None,
                            value: Operand::Place("right".to_string()),
                            writeback_place: None,
                        }],
                    },
                },
                Instruction::Assign {
                    target: "sum".to_string(),
                    value: Rvalue::Binary {
                        op: BinaryOp::Add,
                        left: Operand::Place("left".to_string()),
                        right: Operand::Place("right".to_string()),
                        span: Span::new(1, 1),
                    },
                },
            ],
            terminator: Terminator::Return(Operand::Int(0)),
        }],
    };

    MirModule {
        functions: vec![main, update],
        classes: vec![MirClass {
            name: "Pair".to_string(),
            type_params: Vec::new(),
            fields: vec![
                MirClassField {
                    name: "left".to_string(),
                    ty: Type::named("int64"),
                },
                MirClassField {
                    name: "right".to_string(),
                    ty: Type::named("int64"),
                },
            ],
            methods: Vec::new(),
        }],
        trait_impls: Vec::new(),
        constants: Vec::new(),
        top_level: None,
    }
}

#[cfg(unix)]
fn adr0038_mutable_closure_type() -> Type {
    Type::Closure {
        params: Box::new(Vec::new()),
        return_type: Box::new(Type::Unit),
        captures: Box::new(vec![ClosureCapture {
            name: "captured".to_string(),
            ty: Type::named("int64"),
            mode: ClosureCaptureMode::MutableView,
            span: Span::new(1, 1),
        }]),
        call_kind: ClosureCallKind::MutableRepeatable,
    }
}

#[cfg(unix)]
fn adr0038_set_capture_function() -> MirFunction {
    MirFunction {
        name: "set_capture".to_string(),
        module_name: "<cfg-closure-test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: vec![MirParam {
            name: "captured".to_string(),
            passing: MirReceiverKind::BorrowMut,
            ty: Type::named("int64"),
            default_function: None,
        }],
        local_types: vec![MirLocalType {
            name: "captured".to_string(),
            ty: Type::named("int64"),
        }],
        return_type: Type::Unit,
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: vec![Instruction::Assign {
                target: "captured".to_string(),
                value: Rvalue::Use(Operand::Int(9)),
            }],
            terminator: Terminator::Return(Operand::Unit),
        }],
    }
}

#[cfg(unix)]
fn adr0038_closure_main(blocks: Vec<BasicBlock>, local_types: Vec<MirLocalType>) -> MirFunction {
    MirFunction {
        name: "main".to_string(),
        module_name: "<cfg-closure-test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: Vec::new(),
        local_types,
        return_type: Type::named("int32"),
        entry: "entry".to_string(),
        blocks,
    }
}

#[cfg(unix)]
fn adr0038_pair_class() -> MirClass {
    MirClass {
        name: "Pair".to_string(),
        type_params: Vec::new(),
        fields: vec![
            MirClassField {
                name: "left".to_string(),
                ty: Type::named("int64"),
            },
            MirClassField {
                name: "right".to_string(),
                ty: Type::named("int64"),
            },
        ],
        methods: Vec::new(),
    }
}

#[cfg(unix)]
fn adr0038_closure_branch_module(flag: bool, reverse_branch_storage: bool) -> MirModule {
    let closure_type = adr0038_mutable_closure_type();
    let branch_block = |label: &str, source: &str| BasicBlock {
        label: label.to_string(),
        instructions: vec![
            Instruction::BeginLoan {
                loan: "captured".to_string(),
                source: source.to_string(),
                mutable: true,
            },
            Instruction::Assign {
                target: "callback".to_string(),
                value: Rvalue::Closure {
                    function: "set_capture".to_string(),
                    signature: closure_type.clone(),
                    captures: vec![MirClosureCapture {
                        name: "captured".to_string(),
                        value: Operand::Place("captured".to_string()),
                        ty: Type::named("int64"),
                        passing: MirReceiverKind::BorrowMut,
                        source_place: Some(source.to_string()),
                        resolve_source_at_capture: false,
                    }],
                    consuming: false,
                },
            },
            Instruction::EndLoan {
                loan: "captured".to_string(),
            },
        ],
        terminator: Terminator::Goto("join".to_string()),
    };
    let locals = [
        ("left", Type::named("int64")),
        ("right", Type::named("int64")),
        ("callback", closure_type.clone()),
        ("called", Type::Unit),
        ("printed_left", Type::Unit),
        ("printed_right", Type::Unit),
    ]
    .into_iter()
    .map(|(name, ty)| MirLocalType {
        name: name.to_string(),
        ty,
    })
    .collect();
    let mut blocks = vec![BasicBlock {
        label: "entry".to_string(),
        instructions: vec![
            Instruction::Assign {
                target: "left".to_string(),
                value: Rvalue::Use(Operand::Int(1)),
            },
            Instruction::Assign {
                target: "right".to_string(),
                value: Rvalue::Use(Operand::Int(2)),
            },
        ],
        terminator: Terminator::Branch {
            condition: Operand::Bool(flag),
            then_label: "left_branch".to_string(),
            else_label: "right_branch".to_string(),
        },
    }];
    if reverse_branch_storage {
        blocks.extend([
            branch_block("right_branch", "right"),
            branch_block("left_branch", "left"),
        ]);
    } else {
        blocks.extend([
            branch_block("left_branch", "left"),
            branch_block("right_branch", "right"),
        ]);
    }
    blocks.push(BasicBlock {
        label: "join".to_string(),
        instructions: vec![
            Instruction::Assign {
                target: "called".to_string(),
                value: Rvalue::Call {
                    callee: CallTarget::Value(Operand::Place("callback".to_string())),
                    args: Vec::new(),
                },
            },
            Instruction::Assign {
                target: "printed_left".to_string(),
                value: Rvalue::Call {
                    callee: CallTarget::Name("print".to_string()),
                    args: vec![MirArg {
                        name: None,
                        value: Operand::Place("left".to_string()),
                        writeback_place: None,
                    }],
                },
            },
            Instruction::Assign {
                target: "printed_right".to_string(),
                value: Rvalue::Call {
                    callee: CallTarget::Name("print".to_string()),
                    args: vec![MirArg {
                        name: None,
                        value: Operand::Place("right".to_string()),
                        writeback_place: None,
                    }],
                },
            },
        ],
        terminator: Terminator::Return(Operand::Int(0)),
    });
    let main = adr0038_closure_main(blocks, locals);
    MirModule {
        functions: vec![main, adr0038_set_capture_function()],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        constants: Vec::new(),
        top_level: None,
    }
}

#[cfg(unix)]
fn adr0038_choose_pair_field_function() -> MirFunction {
    MirFunction {
        name: "choose_pair_field".to_string(),
        module_name: "<cfg-closure-test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: vec![
            MirParam {
                name: "pair".to_string(),
                passing: MirReceiverKind::BorrowMut,
                ty: Type::named("Pair"),
                default_function: None,
            },
            MirParam {
                name: "left".to_string(),
                passing: MirReceiverKind::Borrow,
                ty: Type::named("bool"),
                default_function: None,
            },
        ],
        local_types: vec![
            MirLocalType {
                name: "pair".to_string(),
                ty: Type::named("Pair"),
            },
            MirLocalType {
                name: "left".to_string(),
                ty: Type::named("bool"),
            },
        ],
        return_type: Type::named("int64"),
        entry: "entry".to_string(),
        blocks: vec![
            BasicBlock {
                label: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Branch {
                    condition: Operand::Place("left".to_string()),
                    then_label: "left".to_string(),
                    else_label: "right".to_string(),
                },
            },
            BasicBlock {
                label: "left".to_string(),
                instructions: vec![Instruction::ReturnLoan {
                    loan: "pair.left".to_string(),
                    origin: "pair".to_string(),
                }],
                terminator: Terminator::Return(Operand::Place("pair.left".to_string())),
            },
            BasicBlock {
                label: "right".to_string(),
                instructions: vec![Instruction::ReturnLoan {
                    loan: "pair.right".to_string(),
                    origin: "pair".to_string(),
                }],
                terminator: Terminator::Return(Operand::Place("pair.right".to_string())),
            },
        ],
    }
}

#[cfg(unix)]
fn adr0038_call_choose_pair_field(left: bool) -> Vec<Instruction> {
    vec![
        Instruction::Assign {
            target: "call_result".to_string(),
            value: Rvalue::Call {
                callee: CallTarget::Name("choose_pair_field".to_string()),
                args: vec![
                    MirArg {
                        name: None,
                        value: Operand::Place("pair".to_string()),
                        writeback_place: Some("pair".to_string()),
                    },
                    MirArg {
                        name: None,
                        value: Operand::Bool(left),
                        writeback_place: None,
                    },
                ],
            },
        },
        Instruction::BeginReturnedLoan {
            loan: "selected".to_string(),
            origin: "pair".to_string(),
            projections: vec!["left".to_string(), "right".to_string()],
            mutable: true,
        },
    ]
}

#[cfg(unix)]
fn adr0038_selector_reuse_module() -> MirModule {
    let closure_type = adr0038_mutable_closure_type();
    let mut instructions = vec![Instruction::Assign {
        target: "pair".to_string(),
        value: Rvalue::Construct {
            class_name: "Pair".to_string(),
            fields: vec![
                MirFieldInit {
                    name: "left".to_string(),
                    value: Operand::Int(1),
                },
                MirFieldInit {
                    name: "right".to_string(),
                    value: Operand::Int(2),
                },
            ],
        },
    }];
    instructions.extend(adr0038_call_choose_pair_field(true));
    instructions.extend([
        Instruction::Assign {
            target: "callback".to_string(),
            value: Rvalue::Closure {
                function: "set_capture".to_string(),
                signature: closure_type.clone(),
                captures: vec![MirClosureCapture {
                    name: "captured".to_string(),
                    value: Operand::Place("selected".to_string()),
                    ty: Type::named("int64"),
                    passing: MirReceiverKind::BorrowMut,
                    source_place: Some("selected".to_string()),
                    resolve_source_at_capture: true,
                }],
                consuming: false,
            },
        },
        Instruction::EndLoan {
            loan: "selected".to_string(),
        },
    ]);
    instructions.extend(adr0038_call_choose_pair_field(false));
    instructions.extend([
        Instruction::EndLoan {
            loan: "selected".to_string(),
        },
        Instruction::Assign {
            target: "called".to_string(),
            value: Rvalue::Call {
                callee: CallTarget::Value(Operand::Place("callback".to_string())),
                args: Vec::new(),
            },
        },
        Instruction::Assign {
            target: "printed_left".to_string(),
            value: Rvalue::Call {
                callee: CallTarget::Name("print".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Place("pair.left".to_string()),
                    writeback_place: None,
                }],
            },
        },
        Instruction::Assign {
            target: "printed_right".to_string(),
            value: Rvalue::Call {
                callee: CallTarget::Name("print".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Place("pair.right".to_string()),
                    writeback_place: None,
                }],
            },
        },
    ]);
    let locals = [
        ("pair", Type::named("Pair")),
        ("call_result", Type::named("int64")),
        ("selected", Type::named("int64")),
        ("callback", closure_type),
        ("called", Type::Unit),
        ("printed_left", Type::Unit),
        ("printed_right", Type::Unit),
    ]
    .into_iter()
    .map(|(name, ty)| MirLocalType {
        name: name.to_string(),
        ty,
    })
    .collect();
    let main = adr0038_closure_main(
        vec![BasicBlock {
            label: "entry".to_string(),
            instructions,
            terminator: Terminator::Return(Operand::Int(0)),
        }],
        locals,
    );
    MirModule {
        functions: vec![
            main,
            adr0038_choose_pair_field_function(),
            adr0038_set_capture_function(),
        ],
        classes: vec![adr0038_pair_class()],
        trait_impls: Vec::new(),
        constants: Vec::new(),
        top_level: None,
    }
}

#[cfg(unix)]
fn direct_runtime_link_args_for_test(prefix: &str) -> Vec<String> {
    let archive = native_runtime_archive();
    let runtime_identity = archive
        .parent()
        .and_then(std::path::Path::parent)
        .expect("native runtime archive should live below the target root")
        .join("runtime-identity");
    if !archive.is_file() || !runtime_identity.is_file() {
        let bootstrap_cache = TempDir::new(&format!("{prefix}-bootstrap-cache"));
        let (_bootstrap_source, bootstrap_path) = write_temp_source(
            &format!("{prefix}-bootstrap-source"),
            "def main() -> int32:\n    return 0\n",
        );
        let bootstrap = Command::new(aura_bin())
            .env("AURA_CACHE_DIR", bootstrap_cache.path())
            .args(["run", "--backend", "direct"])
            .arg(&bootstrap_path)
            .output()
            .expect("failed to bootstrap direct runtime link metadata");
        assert!(
            bootstrap.status.success(),
            "direct runtime bootstrap failed, stderr was:\n{}",
            String::from_utf8_lossy(&bootstrap.stderr)
        );
    }
    let runtime_identity = fs::read_to_string(runtime_identity)
        .expect("direct runtime identity should be readable after bootstrap");
    serde_json::from_str(
        runtime_identity
            .lines()
            .nth(2)
            .expect("runtime identity should contain native link arguments"),
    )
    .expect("native link arguments should be valid JSON")
}

#[cfg(unix)]
fn run_linked_direct_mir(
    module: &MirModule,
    prefix: &str,
    link_args: &[String],
) -> std::process::Output {
    let output_dir = TempDir::new(prefix);
    let object_path = output_dir.path().join("program.o");
    let binary_path = output_dir.path().join("program");
    let object =
        aura_compiler::emit_host_native_object_with_metadata(module, "/virtual/cfg_view.au", "")
            .expect("manual CFG MIR should compile through the direct backend");
    fs::write(&object_path, object).expect("direct object should be writable");
    let mut linker = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()));
    linker
        .arg(&object_path)
        .arg(native_runtime_archive())
        .arg("-o")
        .arg(&binary_path);
    linker.args(link_args);
    let linked = linker.output().expect("failed to run direct object linker");
    assert!(
        linked.status.success(),
        "direct object link failed, stderr was:\n{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    generated_binary(&binary_path)
        .output()
        .expect("failed to run linked direct CFG-view object")
}

#[cfg(unix)]
#[test]
fn direct_cfg_view_identity_is_independent_of_block_storage_and_unreachable_metadata() {
    let link_args = direct_runtime_link_args_for_test("aura-direct-cfg-view");
    for (label, module) in [
        ("forward", adr0038_cfg_view_module(false, false)),
        ("reversed", adr0038_cfg_view_module(true, false)),
        ("unreachable", adr0038_cfg_view_module(false, true)),
    ] {
        let mir = aura_compiler::run_mir(&module)
            .unwrap_or_else(|error| panic!("{label} MIR execution failed: {error}"));
        assert_eq!(mir.stdout, "11\n22\n", "{label} MIR output drifted");

        let direct = run_linked_direct_mir(
            &module,
            &format!("aura-direct-cfg-view-{label}"),
            &link_args,
        );
        assert!(
            direct.status.success(),
            "{label} direct binary failed, stderr was:\n{}",
            String::from_utf8_lossy(&direct.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&direct.stdout),
            mir.stdout,
            "{label} direct output must match MIR regardless of block storage"
        );
    }
}

#[cfg(unix)]
#[test]
fn direct_closure_writebacks_follow_cfg_and_snapshot_returned_view_selectors() {
    let link_args = direct_runtime_link_args_for_test("aura-direct-cfg-closure");
    for (label, module, expected) in [
        (
            "branch-left",
            adr0038_closure_branch_module(true, false),
            "9\n2\n",
        ),
        (
            "branch-right",
            adr0038_closure_branch_module(false, false),
            "1\n9\n",
        ),
        (
            "branch-left-reversed",
            adr0038_closure_branch_module(true, true),
            "9\n2\n",
        ),
        (
            "branch-right-reversed",
            adr0038_closure_branch_module(false, true),
            "1\n9\n",
        ),
        ("selector-reuse", adr0038_selector_reuse_module(), "9\n2\n"),
    ] {
        let mir = aura_compiler::run_mir(&module)
            .unwrap_or_else(|error| panic!("{label} MIR execution failed: {error}"));
        assert_eq!(mir.stdout, expected, "{label} MIR output drifted");

        let direct = run_linked_direct_mir(
            &module,
            &format!("aura-direct-cfg-closure-{label}"),
            &link_args,
        );
        assert!(
            direct.status.success(),
            "{label} direct binary failed, stderr was:\n{}",
            String::from_utf8_lossy(&direct.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&direct.stdout),
            expected,
            "{label} direct writeback must follow the closure created on the executed CFG path"
        );
    }
}

#[test]
fn run_and_direct_backends_support_loop_local_mutable_closures() {
    let source = r#"
def add(total: mut int64, item: int64):
    total = total + item

def main():
    mut total = 0
    for item in [1, 2]:
        mut bump = lambda [mut total, item]: add(total, item)
        bump()
    print(total)
"#;
    assert_run_and_direct_source_stdout("aura-loop-local-mutable-closure-writeback", source, "3\n");
}

#[test]
fn run_and_direct_backends_preserve_dynamic_returned_view_closure_writeback() {
    let source = r#"
class Pair:
    left: int64
    right: int64

def choose(pair: mut Pair, left: bool) -> view mut int64 from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def forward(pair: mut Pair, left: bool) -> view mut int64 from pair:
    return view mut choose(pair, left)

def assign(value: mut int64, next: int64):
    value = next

def main():
    mut pair = Pair(left=1, right=2)
    view mut captured = forward(pair, false)
    mut update: def(int64) -> None = lambda [mut captured] next: assign(captured, next)
    update(41)
    print(pair)
"#;
    assert_run_and_direct_source_stdout(
        "aura-dynamic-returned-view-closure-writeback",
        source,
        "Pair(left=1, right=41)\n",
    );
}

#[test]
fn run_and_direct_backends_preserve_bounded_trait_dispatch_identity() {
    let source = r#"
trait Other:
    def get(self) -> view int64 from self

trait Project:
    def get(self) -> view int64 from self

trait OtherMut:
    def get_mut(mut self) -> view mut int64 from self

trait ProjectMut:
    def get_mut(mut self) -> view mut int64 from self

class Box:
    left: int64
    right: int64

impl Other for Box:
    def get(self) -> view int64 from self:
        return view self.right

impl Project for Box:
    def get(self) -> view int64 from self:
        return view self.left

impl OtherMut for Box:
    def get_mut(mut self) -> view mut int64 from self:
        return view mut self.right

impl ProjectMut for Box:
    def get_mut(mut self) -> view mut int64 from self:
        return view mut self.left

def forward[T: Project](value: T) -> view int64 from value:
    return view value.get()

def update[T: ProjectMut](value: mut T):
    view mut selected = value.get_mut()
    selected = 9

def main():
    box = Box(left=1, right=2)
    view selected = forward(box)
    print(selected)
    mut mutable_box = Box(left=3, right=4)
    update(mutable_box)
    print(mutable_box.left)
    print(mutable_box.right)
"#;
    assert_run_and_direct_source_stdout(
        "aura-bounded-trait-dispatch-identity",
        source,
        "1\n9\n4\n",
    );
}

#[test]
fn run_and_direct_backends_preserve_specialized_returned_view_trait_identity_in_both_impl_orders() {
    let declarations = r#"
trait Project:
    def get(self) -> view int64 from self

trait Other:
    def get(self) -> view int64 from self

class Box:
    left: int64
    right: int64
"#;
    let project_impl = r#"
impl Project for Box:
    def get(self) -> view int64 from self:
        return view self.left
"#;
    let other_impl = r#"
impl Other for Box:
    def get(self) -> view int64 from self:
        return view self.right
"#;
    let program = r#"
def forward[T: Project](item: T) -> view int64 from item:
    return view item.get()

def main():
    box = Box(left=1, right=2)
    view selected = forward[Box](box)
    print(selected)
"#;

    for (order, first_impl, second_impl) in [
        ("project-first", project_impl, other_impl),
        ("other-first", other_impl, project_impl),
    ] {
        let source = format!("{declarations}{first_impl}{second_impl}{program}");
        assert_run_and_direct_source_stdout(
            &format!("aura-specialized-returned-view-trait-{order}"),
            &source,
            "1\n",
        );
    }
}

#[test]
fn run_and_direct_backends_compose_returned_view_descendants() {
    let source = r#"
class ScalarPair:
    left: int64
    right: int64

def choose_mut(pair: mut ScalarPair, left: bool) -> view mut int64 from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def choose_shared(pair: ScalarPair, left: bool) -> view int64 from pair:
    if left:
        return view pair.left
    return view pair.right

def assign(value: mut int64, next: int64):
    value = next

class Cell:
    value: int64

class CellPair:
    left: Cell
    right: Cell

class TuplePair:
    left: (int64, int64)
    right: (int64, int64)

def left_cell(pair: mut CellPair) -> view mut Cell from pair:
    return view mut pair.left

def choose_cell(pair: mut CellPair, left: bool) -> view mut Cell from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def cell_value(cell: mut Cell) -> view mut int64 from cell:
    return view mut cell.value

def static_forward(pair: mut CellPair) -> view mut int64 from pair:
    return view mut left_cell(pair).value

def dynamic_forward(pair: mut CellPair, left: bool) -> view mut int64 from pair:
    return view mut choose_cell(pair, left).value

def nested_forward(pair: mut CellPair, left: bool) -> view mut int64 from pair:
    return view mut cell_value(choose_cell(pair, left))

def local_class_forward(pair: mut CellPair, left: bool) -> view mut int64 from pair:
    view mut selected = choose_cell(pair, left)
    return view mut selected.value

def choose_tuple(pair: mut TuplePair, left: bool) -> view mut (int64, int64) from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def local_tuple_forward(pair: mut TuplePair, left: bool) -> view mut int64 from pair:
    view mut selected = choose_tuple(pair, left)
    return view mut selected[1]

def main():
    mut mutable_pair = ScalarPair(left=1, right=2)
    view mut selected_mut = choose_mut(mutable_pair, false)
    view mut child_mut = selected_mut
    mut update: def(int64) -> None = lambda [mut child_mut] next: assign(child_mut, next)
    update(9)
    print(mutable_pair)

    shared_pair = ScalarPair(left=3, right=4)
    view selected_shared = choose_shared(shared_pair, true)
    view child_shared = selected_shared
    read: def() -> int64 = lambda [child_shared]: child_shared
    print(read())

    mut cells = CellPair(left=Cell(value=10), right=Cell(value=20))
    view mut static_value = static_forward(cells)
    static_value = 11
    view mut dynamic_value = dynamic_forward(cells, false)
    dynamic_value = 21
    view mut nested_value = nested_forward(cells, true)
    nested_value = 12
    print(cells)

    view mut local_class_value = local_class_forward(cells, false)
    local_class_value = 22
    print(cells)

    mut tuples = TuplePair(left=(30, 40), right=(50, 60))
    view mut local_tuple_value = local_tuple_forward(tuples, true)
    local_tuple_value = 41
    print(tuples)
"#;
    assert_run_and_direct_source_stdout(
        "aura-returned-view-descendant-composition",
        source,
        "ScalarPair(left=1, right=9)\n3\nCellPair(left=Cell(value=12), right=Cell(value=21))\nCellPair(left=Cell(value=12), right=Cell(value=22))\nTuplePair(left=(30, 41), right=(50, 60))\n",
    );
}

#[test]
fn run_and_direct_backends_publish_mutable_call_writebacks_before_trap_cleanup() {
    let cases = [
        (
            "closure-capture",
            r#"
class Resource:
    value: int64

    def close(mut self):
        print(self.value)

def mutate_then_trap(resource: mut Resource):
    resource.value = 9
    print(1 // 0)

def main():
    with resource = Resource(value=1):
        mut action: def() -> None = lambda [mut resource]: mutate_then_trap(resource)
        action()
"#,
        ),
        (
            "named-view",
            r#"
class Resource:
    value: int64

    def close(mut self):
        print(self.value)

def mutate_then_trap(resource: mut Resource):
    resource.value = 9
    print(1 // 0)

def main():
    with resource = Resource(value=1):
        view mut alias = resource
        mutate_then_trap(alias)
"#,
        ),
        (
            "immediate-returned-view",
            r#"
class Resource:
    value: int64

    def close(mut self):
        print(self.value)

def borrow_mut(resource: mut Resource) -> view mut Resource from resource:
    return view mut resource

def mutate_then_trap(resource: mut Resource):
    resource.value = 9
    print(1 // 0)

def main():
    with resource = Resource(value=1):
        mutate_then_trap(borrow_mut(resource))
"#,
        ),
    ];

    for (label, source) in cases {
        for output in run_and_direct_failure_outputs(
            &format!("aura-adr0038-trap-write-through-{label}"),
            source,
        ) {
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                "9\n",
                "{label} cleanup must observe every successful pre-trap mutation"
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("error[AU4004]: division by zero"),
                "{label} must preserve the body trap as primary, stderr was:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn run_and_direct_backends_support_immediate_mutable_returned_view_receivers() {
    let source = r#"
class Box:
    value: int64

    def set(mut self, next: int64):
        self.value = next

class Wrapper:
    box: Box

def borrow_box(box: mut Box) -> view mut Box from box:
    return view mut box

def borrow_wrapper(wrapper: mut Wrapper) -> view mut Wrapper from wrapper:
    return view mut wrapper

def borrow_values(values: mut list[int64]) -> view mut list[int64] from values:
    return view mut values

def main():
    mut box = Box(value=1)
    borrow_box(box).set(9)
    print(box.value)

    mut wrapper = Wrapper(box=Box(value=2))
    borrow_wrapper(wrapper).box.set(10)
    print(wrapper.box.value)

    mut values = [1, 2]
    borrow_values(values).append(3)
    print(values)
"#;

    assert_run_and_direct_source_stdout(
        "aura-adr0038-immediate-mutable-returned-view-receiver",
        source,
        "9\n10\n[1, 2, 3]\n",
    );
}

fn assert_run_and_direct_source_stdout_with_timeout(
    prefix: &str,
    source: &str,
    timeout: std::time::Duration,
    expected_stdout: &str,
) {
    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        prefix,
        source,
        timeout,
        expected_stdout,
        None,
    );
}

fn assert_run_and_direct_source_stdout_with_timeout_and_workers(
    prefix: &str,
    source: &str,
    timeout: std::time::Duration,
    expected_stdout: &str,
    worker_count: Option<usize>,
) {
    let (_temp, _source_path, mut run_child) =
        run_aura_source_with_timeout_and_workers(prefix, source, timeout, worker_count);
    let run_status = wait_with_timeout(&mut run_child, timeout).unwrap_or_else(|| {
        run_child
            .kill()
            .expect("failed to kill timed out aura run process");
        panic!("aura run timed out after {:?}", timeout);
    });
    let run = run_child
        .wait_with_output()
        .expect("failed to collect aura run output");
    assert!(
        run_status.success(),
        "aura run should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expected_stdout);

    let (_temp, _source_path, mut direct_child) = build_direct_source_with_timeout_and_workers(
        &format!("{prefix}-direct"),
        source,
        timeout,
        worker_count,
    );
    let direct_status = wait_with_timeout(&mut direct_child, timeout).unwrap_or_else(|| {
        direct_child
            .kill()
            .expect("failed to kill timed out direct-backend process");
        panic!("direct-backend run timed out after {:?}", timeout);
    });
    let direct = direct_child
        .wait_with_output()
        .expect("failed to collect direct-backend output");
    assert!(
        direct_status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&direct.stdout), expected_stdout);
}

fn assert_mir_and_direct_source_stdout_with_timeout_and_workers(
    prefix: &str,
    source: &str,
    timeout: std::time::Duration,
    expected_stdout: &str,
    worker_count: usize,
) {
    let (temp, source_path) = write_temp_source(prefix, source);

    let mut mir = Command::new(aura_bin());
    mir.env("AURA_WORKERS", worker_count.to_string())
        .args(["run", "--backend", "mir"])
        .arg(&source_path);
    let mir = command_output_with_timeout(mir, timeout, "forced-MIR fixture");
    assert!(
        mir.status.success(),
        "{prefix} MIR run should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&mir.stdout), expected_stdout);

    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build direct fixture");
    assert!(
        build.status.success(),
        "{prefix} direct fixture should build, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut direct = generated_binary(&output_path);
    direct.env("AURA_WORKERS", worker_count.to_string());
    let direct = command_output_with_timeout(direct, timeout, "direct fixture");
    assert!(
        direct.status.success(),
        "{prefix} direct run should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&direct.stdout), expected_stdout);
}

fn assert_run_and_direct_source_failure_with_timeout(
    prefix: &str,
    source: &str,
    timeout: std::time::Duration,
    expected_stdout: &str,
    expected_stderr_substring: &str,
) {
    let (_temp, _source_path, mut run_child) =
        run_aura_source_with_timeout(prefix, source, timeout);
    let run_status = wait_with_timeout(&mut run_child, timeout).unwrap_or_else(|| {
        run_child
            .kill()
            .expect("failed to kill timed out aura run process");
        panic!("aura run timed out after {:?}", timeout);
    });
    let run = run_child
        .wait_with_output()
        .expect("failed to collect aura run output");
    assert!(
        !run_status.success(),
        "aura run should fail, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expected_stdout);
    assert!(
        String::from_utf8_lossy(&run.stderr).contains(expected_stderr_substring),
        "aura run stderr should mention `{expected_stderr_substring}`, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let (_temp, _source_path, mut direct_child) =
        build_direct_source_with_timeout(&format!("{prefix}-direct"), source, timeout);
    let direct_status = wait_with_timeout(&mut direct_child, timeout).unwrap_or_else(|| {
        direct_child
            .kill()
            .expect("failed to kill timed out direct-backend process");
        panic!("direct-backend run timed out after {:?}", timeout);
    });
    let direct = direct_child
        .wait_with_output()
        .expect("failed to collect direct-backend output");
    assert!(
        !direct_status.success(),
        "direct-backend binary should fail, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&direct.stdout), expected_stdout);
    assert!(
        String::from_utf8_lossy(&direct.stderr).contains(expected_stderr_substring),
        "direct-backend stderr should mention `{expected_stderr_substring}`, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
}

fn run_aura_source_with_timeout(
    prefix: &str,
    source: &str,
    timeout: std::time::Duration,
) -> (TempDir, PathBuf, std::process::Child) {
    run_aura_source_with_timeout_and_workers(prefix, source, timeout, None)
}

fn run_aura_source_with_timeout_and_workers(
    prefix: &str,
    source: &str,
    timeout: std::time::Duration,
    worker_count: Option<usize>,
) -> (TempDir, PathBuf, std::process::Child) {
    let (temp, source_path) = write_temp_source(prefix, source);
    let mut command = Command::new(aura_bin());
    command.arg("run").arg(&source_path);
    if let Some(worker_count) = worker_count {
        command.env("AURA_WORKERS", worker_count.to_string());
    }
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("failed to spawn aura run: {error}"));
    assert!(
        timeout > std::time::Duration::ZERO,
        "timeout should be positive"
    );
    (temp, source_path, child)
}

fn build_direct_source_with_timeout(
    prefix: &str,
    source: &str,
    timeout: std::time::Duration,
) -> (TempDir, PathBuf, std::process::Child) {
    build_direct_source_with_timeout_and_workers(prefix, source, timeout, None)
}

fn build_direct_source_with_timeout_and_workers(
    prefix: &str,
    source: &str,
    timeout: std::time::Duration,
    worker_count: Option<usize>,
) -> (TempDir, PathBuf, std::process::Child) {
    let (temp, source_path) = write_temp_source(prefix, source);
    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build --backend direct");
    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let mut command = generated_binary(&output_path);
    if let Some(worker_count) = worker_count {
        command.env("AURA_WORKERS", worker_count.to_string());
    }
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("failed to spawn direct-backend binary: {error}"));
    assert!(
        timeout > std::time::Duration::ZERO,
        "timeout should be positive"
    );
    (temp, source_path, child)
}

#[test]
fn ast_exits_cleanly_when_stdout_pipe_closes() {
    let fixture = repo_root().join("examples/point.au");
    let mut child = Command::new(aura_bin())
        .arg("ast")
        .arg(fixture)
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura ast");

    drop(child.stdout.take());

    let status = child.wait().expect("failed to wait for aura ast");
    assert!(status.success(), "ast should exit cleanly on broken pipe");
}

#[test]
fn lsp_service_handles_multiple_requests_in_one_process() {
    let mut child = Command::new(aura_bin())
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start aura lsp service");
    let input = [
        serde_json::json!({
            "id": 1,
            "semantic_interface_version": aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION,
            "method": "analyze",
            "path": "/virtual/main.au",
            "source": "def main() -> int32:\n    return 0\n"
        }),
        serde_json::json!({
            "id": 2,
            "semantic_interface_version": aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION,
            "method": "complete",
            "path": "/virtual/main.au",
            "source": "def main() -> int32:\n    value: str = \"hi\"\n    value.\n    return 0\n",
            "line": 2,
            "character": 10,
            "trigger": "."
        }),
    ]
    .into_iter()
    .map(|request| request.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    child
        .stdin
        .take()
        .expect("lsp stdin should be piped")
        .write_all(format!("{input}\n").as_bytes())
        .expect("lsp requests should write");

    let output = child
        .wait_with_output()
        .expect("lsp service should exit after stdin closes");
    assert!(
        output.status.success(),
        "lsp service should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses = String::from_utf8(output.stdout)
        .expect("lsp responses should be utf-8")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("valid response JSON"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(
        responses[0]["semantic_interface_version"],
        aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION
    );
    assert!(responses[0]["result"]["diagnostics"].is_array());
    assert_eq!(responses[1]["id"], 2);
    assert_eq!(
        responses[1]["semantic_interface_version"],
        aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION
    );
    assert!(responses[1]["result"]
        .as_array()
        .expect("completion result should be an array")
        .iter()
        .any(|item| item["name"] == "len"));
}

#[test]
fn new_fmt_and_test_commands_cover_the_project_workflow() {
    let temp = TempDir::new("aura-project-workflow");
    let create = Command::new(aura_bin())
        .current_dir(temp.path())
        .args(["new", "agent-app"])
        .output()
        .expect("failed to run aura new");
    assert!(
        create.status.success(),
        "aura new should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&create.stderr)
    );
    let project = temp.path().join("agent-app");
    assert!(project.join("Aura.toml").is_file());
    assert!(project.join("src/main.au").is_file());
    assert!(project.join("tests/smoke.au").is_file());
    assert_eq!(
        fs::read_to_string(project.join(".gitignore")).expect("gitignore should read"),
        "target/\n"
    );

    fs::write(
        project.join("src/main.au"),
        "def main() -> int32:   \r\n    print(\"ready\")\t\r\n    return 0\r\n",
    )
    .expect("unformatted source should write");
    let check = Command::new(aura_bin())
        .current_dir(&project)
        .args(["fmt", "--check", "src/main.au"])
        .output()
        .expect("failed to run aura fmt --check");
    assert!(
        !check.status.success(),
        "unformatted source should fail --check"
    );

    let format = Command::new(aura_bin())
        .current_dir(&project)
        .args(["fmt", "src/main.au"])
        .output()
        .expect("failed to run aura fmt");
    assert!(format.status.success(), "aura fmt should succeed");
    assert_eq!(
        fs::read_to_string(project.join("src/main.au")).expect("formatted source should read"),
        "def main() -> int32:\n    print(\"ready\")\n    return 0\n"
    );

    fs::write(
        project.join("src/helpers.au"),
        "public def answer() -> int32:\n    return 42\n",
    )
    .expect("project helper source should write");
    fs::write(
        project.join("tests/smoke.au"),
        "from helpers import answer\n\ndef main() -> int32:\n    print(answer())\n    return 0\n",
    )
    .expect("test source should write");
    let tests = Command::new(aura_bin())
        .current_dir(&project)
        .arg("test")
        .output()
        .expect("failed to run aura test");
    assert!(
        tests.status.success(),
        "aura test should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&tests.stderr)
    );
    assert!(String::from_utf8_lossy(&tests.stdout).contains("1 passed; 0 failed"));

    fs::write(
        project.join("tests/slow.au"),
        "def main() -> int32:\n    sleep(1s)\n    return 0\n",
    )
    .expect("slow test source should write");
    let timed_out = Command::new(aura_bin())
        .current_dir(&project)
        .args(["test", "--timeout-ms", "10", "tests/slow.au"])
        .output()
        .expect("failed to run timed-out aura test");
    assert!(!timed_out.status.success(), "timed-out test should fail");
    assert!(String::from_utf8_lossy(&timed_out.stderr).contains("timed out after 10ms"));

    let recreate = Command::new(aura_bin())
        .current_dir(temp.path())
        .args(["new", "agent-app"])
        .output()
        .expect("failed to rerun aura new");
    assert!(
        !recreate.status.success(),
        "aura new must not overwrite a project"
    );
}

#[test]
fn fmt_preserves_triple_quoted_string_bytes() {
    let temp = TempDir::new("aura-fmt-triple-string");
    let source =
        "def main():\n    message = \"\"\"first  \n\tsecond\t\n\"\"\"\n    print(message)\n";
    let path = temp.path().join("triple.au");
    fs::write(&path, source).expect("triple-string fixture should write");
    let format = Command::new(aura_bin())
        .args([
            "fmt",
            path.to_str().expect("temporary path should be UTF-8"),
        ])
        .output()
        .expect("failed to run aura fmt");
    assert!(format.status.success(), "aura fmt should succeed");
    assert_eq!(fs::read_to_string(path).unwrap(), source);
}

#[test]
fn fmt_is_idempotent_for_adr_0022_capability_syntax() {
    let temp = TempDir::new("aura-capability-format");
    let source_path = temp.path().join("capabilities.au");
    fs::write(
        &source_path,
        concat!(
            "class Box:\r\n",
            "    value: str   \r\n",
            "    def read(self) -> str:\r\n",
            "        return self.value.clone()\r\n",
            "    def replace(mut self, value: own str):\r\n",
            "        self.value = value\r\n",
            "\r\n",
            "def inspect(value: str):\r\n",
            "    print(value)\r\n",
            "\r\n",
            "def main():\r\n",
            "    mut boxes = [Box(value=\"one\")]\r\n",
            "    for box in mut boxes:\r\n",
            "        box.replace(\"changed\")\r\n",
            "    match own boxes:\r\n",
            "        case _:\r\n",
            "            pass\t\r\n",
        ),
    )
    .expect("capability source should write");

    let first = Command::new(aura_bin())
        .args(["fmt"])
        .arg(&source_path)
        .output()
        .expect("failed to format ADR-0022 capability syntax");
    assert!(
        first.status.success(),
        "capability syntax should format successfully, stderr was:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let once = fs::read_to_string(&source_path).expect("formatted source should read");
    assert!(once.contains("def replace(mut self, value: own str):"));
    assert!(once.contains("for box in mut boxes:"));
    assert!(once.contains("match own boxes:"));
    assert!(!once.contains('\r'));
    assert!(!once.lines().any(|line| line.ends_with([' ', '\t'])));

    let check = Command::new(aura_bin())
        .args(["fmt", "--check"])
        .arg(&source_path)
        .output()
        .expect("failed to check formatted ADR-0022 capability syntax");
    assert!(
        check.status.success(),
        "a second formatter pass must be idempotent, stderr was:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        fs::read_to_string(&source_path).expect("idempotent source should read"),
        once
    );
}

#[test]
fn fmt_is_idempotent_for_lambda_expression_syntax() {
    let temp = TempDir::new("aura-lambda-format");
    let source_path = temp.path().join("lambdas.au");
    fs::write(
        &source_path,
        concat!(
            "def build():\r\n",
            "    identity = lambda value: value   \r\n",
            "    consume = lambda own value: value\t\r\n",
            "    combine = lambda left, mut right: left + right\r\n",
        ),
    )
    .expect("lambda source should write");

    let first = Command::new(aura_bin())
        .args(["fmt"])
        .arg(&source_path)
        .output()
        .expect("failed to format lambda syntax");
    assert!(
        first.status.success(),
        "lambda syntax should format successfully, stderr was:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let once = fs::read_to_string(&source_path).expect("formatted source should read");
    assert!(once.contains("identity = lambda value: value"));
    assert!(once.contains("consume = lambda own value: value"));
    assert!(once.contains("combine = lambda left, mut right: left + right"));
    assert!(!once.contains('\r'));
    assert!(!once.lines().any(|line| line.ends_with([' ', '\t'])));

    let check = Command::new(aura_bin())
        .args(["fmt", "--check"])
        .arg(&source_path)
        .output()
        .expect("failed to check formatted lambda syntax");
    assert!(
        check.status.success(),
        "a second formatter pass must be idempotent, stderr was:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        fs::read_to_string(&source_path).expect("idempotent source should read"),
        once
    );
}

#[test]
fn fmt_is_idempotent_for_extern_c_declarations() {
    let temp = TempDir::new("aura-ffi-format");
    let source_path = temp.path().join("ffi.au");
    fs::write(
        &source_path,
        concat!(
            "public extern \"C\" opaque class ProcessHandle   \r\n",
            "public extern \"C\" def getpid() -> int32\t\r\n",
            "extern \"C\" def write(fd: int32, data: str) -> int64\r\n",
        ),
    )
    .expect("FFI source should write");

    let first = Command::new(aura_bin())
        .args(["fmt"])
        .arg(&source_path)
        .output()
        .expect("failed to format extern C declarations");
    assert!(
        first.status.success(),
        "extern C syntax should format successfully, stderr was:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let once = fs::read_to_string(&source_path).expect("formatted source should read");
    assert!(once.contains("public extern \"C\" opaque class ProcessHandle"));
    assert!(once.contains("public extern \"C\" def getpid() -> int32"));
    assert!(once.contains("extern \"C\" def write(fd: int32, data: str) -> int64"));
    assert!(!once.contains('\r'));
    assert!(!once.lines().any(|line| line.ends_with([' ', '\t'])));

    let check = Command::new(aura_bin())
        .args(["fmt", "--check"])
        .arg(&source_path)
        .output()
        .expect("failed to check formatted extern C declarations");
    assert!(
        check.status.success(),
        "a second formatter pass must be idempotent, stderr was:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        fs::read_to_string(&source_path).expect("idempotent source should read"),
        once
    );
}

#[test]
fn run_and_built_programs_receive_arguments_and_environment() {
    let source = r#"import sys

def print_child_arguments():
    for argument in sys.args():
        print("child:" + argument)

def main() -> int32:
    for argument in sys.args():
        print("main:" + argument)
    with TaskGroup() as group:
        group.start_soon(print_child_arguments)
    match sys.env("AURA_CLI_TEST_VALUE"):
        case Option.Some(value):
            print(value)
        case Option.None:
            return 1
    return 0
"#;
    let (temp, source_path) = write_temp_source("aura-program-args", source);
    let interpreted = Command::new(aura_bin())
        .args(["run", source_path.to_str().expect("UTF-8 temp path"), "--"])
        .args(["alpha", "beta"])
        .env("AURA_CLI_TEST_VALUE", "from-env")
        .env("AURA_PROGRAM_ARGS_JSON", "[\"spoofed\"]")
        .output()
        .expect("failed to run aura program with arguments");
    assert!(
        interpreted.status.success(),
        "aura run should accept program arguments, stderr was:\n{}",
        String::from_utf8_lossy(&interpreted.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&interpreted.stdout),
        "main:alpha\nmain:beta\nchild:alpha\nchild:beta\nfrom-env\n"
    );

    let mut stdin_child = Command::new(aura_bin())
        .arg("run")
        .arg("--stdin")
        .arg(&source_path)
        .arg("--")
        .args(["alpha", "beta"])
        .env("AURA_CLI_TEST_VALUE", "from-env")
        .env("AURA_PROGRAM_ARGS_JSON", "[\"spoofed\"]")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run stdin Aura program with arguments");
    stdin_child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write argv test source to stdin");
    let stdin_interpreted = stdin_child
        .wait_with_output()
        .expect("failed to collect stdin argv test output");
    assert!(
        stdin_interpreted.status.success(),
        "stdin aura run should accept explicit program arguments, stderr was:\n{}",
        String::from_utf8_lossy(&stdin_interpreted.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&stdin_interpreted.stdout),
        "main:alpha\nmain:beta\nchild:alpha\nchild:beta\nfrom-env\n"
    );

    let output_path = temp.path().join("program");
    let build = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build argument-aware program");
    assert!(
        build.status.success(),
        "argument-aware direct build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let direct = generated_binary(&output_path)
        .args(["alpha", "beta"])
        .env("AURA_CLI_TEST_VALUE", "from-env")
        .env("AURA_PROGRAM_ARGS_JSON", "[\"spoofed\"]")
        .output()
        .expect("failed to run built program with arguments");
    assert!(
        direct.status.success(),
        "built program should accept arguments, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&direct.stdout),
        "main:alpha\nmain:beta\nchild:alpha\nchild:beta\nfrom-env\n"
    );
}

#[test]
fn mir_and_forced_direct_support_one_thousand_simultaneously_suspended_tasks() {
    let source = r#"def suspend(started: Queue[int32], release: Queue[int32]):
    started.put(1)
    match release.get():
        case QueueReceive.Item(_):
            pass
        case QueueReceive.Closed:
            pass
        case QueueReceive.TimedOut:
            pass
        case QueueReceive.Cancelled:
            pass

def main() -> int32:
    started = Queue[int32]()
    release = Queue[int32]()
    mut ready: int32 = 0

    with TaskGroup() as group:
        mut spawned: int32 = 0
        while spawned < 1000:
            group.start_soon(suspend, started, release)
            spawned += 1

        while ready < 1000:
            match started.get():
                case QueueReceive.Item(_):
                    ready += 1
                case QueueReceive.Closed:
                    return 2
                case QueueReceive.TimedOut:
                    pass
                case QueueReceive.Cancelled:
                    return 3

        release.close()

    print(ready)
    return 0
"#;

    assert_run_and_direct_source_stdout("aura-thousand-suspended-direct-tasks", source, "1000\n");
}

#[test]
fn mir_exits_cleanly_when_stdout_pipe_closes() {
    let fixture = repo_root().join("examples/control_flow/while_break_continue.au");
    let mut child = Command::new(aura_bin())
        .arg("mir")
        .arg(fixture)
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura mir");

    drop(child.stdout.take());

    let status = child.wait().expect("failed to wait for aura mir");
    assert!(status.success(), "mir should exit cleanly on broken pipe");
}

#[test]
fn task_group_scope_exit_cancels_blocked_children() {
    let source = r#"def wait_forever(q: Queue[int32]) -> None:
    match q.get():
        case QueueReceive.Item(value):
            print(value)
        case QueueReceive.Closed:
            print("closed")
        case QueueReceive.TimedOut:
            print("timed out")
        case QueueReceive.Cancelled:
            print("cancelled")

def main() -> int32:
    q = Queue[int32]()
    with TaskGroup() as group:
        group.start_soon(wait_forever, q)
    print("done")
    return 0
"#;

    let (_temp, _source_path, mut child) = run_aura_source_with_timeout(
        "aura-task-group-close",
        source,
        std::time::Duration::from_secs(15),
    );
    let status = wait_with_timeout(&mut child, std::time::Duration::from_secs(15))
        .expect("task-group scope exit should not hang indefinitely");
    let output = child
        .wait_with_output()
        .expect("failed to collect aura run output");
    assert!(
        status.success(),
        "task-group scope exit should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "cancelled\ndone\n");

    let (_temp, _source_path, mut direct_child) = build_direct_source_with_timeout(
        "aura-task-group-close-direct",
        source,
        std::time::Duration::from_secs(15),
    );
    let status = wait_with_timeout(&mut direct_child, std::time::Duration::from_secs(15))
        .expect("direct task-group scope exit should not hang indefinitely");
    let output = direct_child
        .wait_with_output()
        .expect("failed to collect direct-backend output");
    assert!(
        status.success(),
        "direct task-group scope exit should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "cancelled\ndone\n");
}

#[test]
fn task_group_join_keeps_reachable_queue_producers_alive_under_cpu_load() {
    let source = r#"import sys

def burn_cpu() -> None:
    started_at = sys.monotonic_time_ms()
    mut value: int64 = 1
    while sys.monotonic_time_ms() - started_at < 250:
        value = (value * 1664525 + 1013904223) % 2147483647

def produce(q: Queue[int32], base: int32) -> int32:
    mut sent: int32 = 0
    while sent < 1000:
        match own q.put(base + sent):
            case Result.Ok(_):
                sent += 1
            case Result.Err(_):
                return sent
    return sent

def consume(q: Queue[int32], totals: Queue[int32]) -> None:
    mut received: int32 = 0
    while received < 1000:
        match own q.get():
            case QueueReceive.Item(_):
                received += 1
            case QueueReceive.Closed:
                totals.put(received)
                return
            case QueueReceive.TimedOut:
                pass
            case QueueReceive.Cancelled:
                totals.put(received)
                return
    totals.put(received)

def main() -> int32:
    q = Queue[int32](capacity=64)
    totals = Queue[int32](capacity=4)

    with TaskGroup() as outer:
        mut burner: int32 = 0
        while burner < 12:
            outer.start_soon(burn_cpu)
            burner += 1

        outer.start_soon(consume, q, totals)
        outer.start_soon(consume, q, totals)
        outer.start_soon(consume, q, totals)
        outer.start_soon(consume, q, totals)

        with TaskGroup() as producers:
            producers.start_soon(produce, q, 0)
            producers.start_soon(produce, q, 1000)
            producers.start_soon(produce, q, 2000)
            producers.start_soon(produce, q, 3000)

        q.close()

        mut consumed: int32 = 0
        mut consumer: int32 = 0
        while consumer < 4:
            consumed += totals.get_or(-10000, timeout=10s)
            consumer += 1

        print(consumed)
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        "aura-task-group-reachable-queue-join",
        source,
        std::time::Duration::from_secs(30),
        "4000\n",
        Some(4),
    );
}

#[test]
fn queue_iteration_consumers_complete_with_more_cpu_burners_than_default_workers() {
    let default_workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let burner_count = default_workers.saturating_add(4);
    let source = format!(
        r#"import sys

def burn_cpu() -> None:
    started_at = sys.monotonic_time_ms()
    mut value: int64 = 1
    while sys.monotonic_time_ms() - started_at < 300:
        value = (value * 1664525 + 1013904223) % 2147483647

def produce(q: Queue[int64], base: int64) -> None:
    for offset in range(250):
        q.put(base + offset)

def consume(q: Queue[int64], totals: Queue[int32]) -> None:
    mut received: int32 = 0
    for value in q:
        if value >= 0:
            received += 1
    totals.put(received)

def main() -> int32:
    q = Queue[int64](capacity=64)
    totals = Queue[int32](capacity=4)

    with TaskGroup() as outer:
        mut burner: int32 = 0
        while burner < {burner_count}:
            outer.start_soon(burn_cpu)
            burner += 1

        outer.start_soon(consume, q, totals)
        outer.start_soon(consume, q, totals)
        outer.start_soon(consume, q, totals)
        outer.start_soon(consume, q, totals)

        with TaskGroup() as producers:
            producers.start_soon(produce, q, 0)
            producers.start_soon(produce, q, 1000)
            producers.start_soon(produce, q, 2000)
            producers.start_soon(produce, q, 3000)

        q.close()
        mut consumed: int32 = 0
        mut consumer: int32 = 0
        while consumer < 4:
            consumed += totals.get_or(-10000, timeout=10s)
            consumer += 1
        print(consumed)
    return 0
"#
    );

    assert_run_and_direct_source_stdout_with_timeout(
        "aura-queue-iteration-default-worker-contention",
        &source,
        std::time::Duration::from_secs(20),
        "1000\n",
    );
}

#[test]
fn task_group_join_still_cancels_a_queue_wait_without_a_reachable_waker() {
    let source = r#"def wait_on_private_queue() -> None:
    private = Queue[int32]()
    match private.get():
        case QueueReceive.Item(_):
            print("unexpected item")
        case QueueReceive.Closed:
            print("unexpected close")
        case QueueReceive.TimedOut:
            print("unexpected timeout")
        case QueueReceive.Cancelled:
            print("cancelled")

def main() -> int32:
    with TaskGroup() as group:
        group.start_soon(wait_on_private_queue)
    print("done")
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        "aura-task-group-unreachable-queue-join",
        source,
        std::time::Duration::from_secs(15),
        "cancelled\ndone\n",
        Some(4),
    );
}

#[test]
fn task_group_join_does_not_treat_the_joining_parent_as_a_queue_waker() {
    let source = r#"def fill_without_consumer(q: Queue[int32]) -> None:
    q.put(1)
    match own q.put(2):
        case Result.Ok(_):
            print("unexpected send")
        case Result.Err(_):
            print("cancelled")

def nested_parent(q: Queue[int32]) -> None:
    with TaskGroup() as inner:
        inner.start_soon(fill_without_consumer, q)
    print("nested done")

def main() -> int32:
    q = Queue[int32](capacity=1)
    with TaskGroup() as outer:
        outer.start_soon(nested_parent, q)
    print("done")
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        "aura-task-group-joining-parent-is-not-waker",
        source,
        std::time::Duration::from_secs(15),
        "cancelled\nnested done\ndone\n",
        Some(4),
    );
}

#[test]
fn task_group_join_tracks_queue_handles_received_after_task_start() {
    let source = r#"def delayed_consumer(
    handoff: Queue[Queue[int32]],
    ready: Queue[int32],
    totals: Queue[int32]
) -> None:
    match own handoff.get():
        case QueueReceive.Item(q):
            ready.put(1)
            sleep(100ms)
            mut received: int32 = 0
            while received < 2:
                match own q.get():
                    case QueueReceive.Item(_):
                        received += 1
                    case QueueReceive.Closed:
                        totals.put(received)
                        return
                    case QueueReceive.TimedOut:
                        pass
                    case QueueReceive.Cancelled:
                        totals.put(received)
                        return
            totals.put(received)
        case _:
            totals.put(-10000)

def send_second(q: Queue[int32]) -> None:
    q.put(2)

def main() -> int32:
    q = Queue[int32](capacity=1)
    q.put(1)
    handoff = Queue[Queue[int32]](capacity=1)
    ready = Queue[int32](capacity=1)
    totals = Queue[int32](capacity=1)

    with TaskGroup() as outer:
        outer.start_soon(delayed_consumer, handoff, ready, totals)
        handoff.put(q)
        ready.get()

        with TaskGroup() as producer:
            producer.start_soon(send_second, q)

        q.close()
        print(totals.get_or(-10000, timeout=5s))
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        "aura-task-group-dynamic-queue-waker",
        source,
        std::time::Duration::from_secs(20),
        "2\n",
        Some(4),
    );
}

#[test]
fn task_group_join_detects_cross_join_cycles_after_multiple_cleanup_probes() {
    let source = r#"def producer(q: Queue[int32], signal: Queue[int32]) -> None:
    sleep(20ms)
    q.put(1)
    signal.put(1)

def consumer(signal: Queue[int32]) -> None:
    sleep(20ms)
    signal.get()

def parent_a(q: Queue[int32], signal: Queue[int32]) -> None:
    with TaskGroup() as inner:
        inner.start_soon(producer, q, signal)

def parent_b(q: Queue[int32], signal: Queue[int32]) -> None:
    with TaskGroup() as inner:
        inner.start_soon(consumer, signal)
    q.get()

def main() -> int32:
    q = Queue[int32](capacity=1)
    q.put(0)
    signal = Queue[int32]()
    with TaskGroup() as outer:
        outer.start_soon(parent_a, q, signal)
        outer.start_soon(parent_b, q, signal)
    print("done")
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        "aura-task-group-cross-join-cycle",
        source,
        // The outer watchdog includes process scheduling while the complete
        // CLI suite launches many direct binaries in parallel. The Aura
        // sleeps above remain the semantic timing pins.
        std::time::Duration::from_secs(30),
        "done\n",
        Some(4),
    );
}

#[test]
fn queue_consumers_share_work_fairly_on_one_worker() {
    let source = r#"def consumer(q: Queue[int32]) -> int32:
    mut got: int32 = 0
    for value in q:
        got += 1
    return got

def main() -> int32:
    q = Queue[int32](capacity=16)
    with TaskGroup() as group:
        c1 = group.start(consumer, q)
        c2 = group.start(consumer, q)
        c3 = group.start(consumer, q)
        c4 = group.start(consumer, q)

        mut i: int32 = 0
        while i < 1000:
            match q.put(i):
                case Result.Ok(_):
                    pass
                case Result.Err(_):
                    return 1
            i += 1
        q.close()

        print(c1.result_or(-1, timeout=5s))
        print(c2.result_or(-1, timeout=5s))
        print(c3.result_or(-1, timeout=5s))
        print(c4.result_or(-1, timeout=5s))
    return 0
"#;

    let (_temp, source_path) = write_temp_source("aura-queue-fairness", source);
    let output = Command::new(aura_bin())
        .env("AURA_WORKERS", "1")
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run queue fairness source");

    assert!(
        output.status.success(),
        "queue fairness source should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let counts = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.parse::<i32>().expect("counts should be integers"))
        .collect::<Vec<_>>();
    assert_eq!(counts.len(), 4, "expected four consumer counts");
    assert_eq!(
        counts.iter().sum::<i32>(),
        1000,
        "counts should sum to all items"
    );
    let min = *counts.iter().min().expect("counts should not be empty");
    let max = *counts.iter().max().expect("counts should not be empty");
    assert!(
        max - min <= 1,
        "queue consumers should share work fairly, got {:?}",
        counts
    );
}

#[test]
fn cancelled_sleeping_children_resume_and_can_observe_cancellation() {
    let source = r#"def long_sleeper() -> int32:
    sleep(5s)
    print("after-sleep")
    if cancelled():
        print("observed-cancel")
        return 7
    return 99

def main() -> int32:
    with TaskGroup() as group:
        task = group.start(long_sleeper)
        sleep(20ms)
        group.cancel()
        match task.result(timeout=1s):
            case TaskResult.Ready(value):
                print(value)
            case TaskResult.Error(message):
                print(message)
            case TaskResult.TimedOut:
                print("timedout")
            case TaskResult.Cancelled:
                print("cancelled")
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-sleep-cancel-observed",
        source,
        "after-sleep\nobserved-cancel\n7\n",
    );
}

#[test]
fn scheduler_mixed_wakeups_complete_in_mir_and_direct_backends() {
    // Queue, task, timer, cancellation, and blocking-I/O wakeups are observable
    // here. Whether the scheduler reaches them by direct notification instead
    // of rescanning is intentionally proved by the reactor's instrumented unit
    // tests because both implementations have the same language-level result.
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/scheduler_mixed_wakeups.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/scheduler_mixed_wakeups.stdout");

    assert_run_and_direct_source_stdout_with_timeout(
        "aura-scheduler-mixed-wakeups",
        source,
        std::time::Duration::from_secs(20),
        expected,
    );
}

#[test]
fn yield_now_fairness_remains_observable_on_one_worker() {
    let source = include_str!("../../aura-compiler/tests/fixtures/run-pass/yield_now_fairness.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/yield_now_fairness.stdout");

    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        "aura-yield-now-one-worker",
        source,
        std::time::Duration::from_secs(20),
        expected,
        Some(1),
    );
}

#[test]
fn nested_scheduler_spawns_preserve_outcomes_cleanup_and_backend_parity() {
    // Nested child starts, result waits, the 256 KiB stack override, and
    // structured cleanup must all complete on both backends. The fixture
    // validates the event multiset rather than freezing a cross-worker order.
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/scheduler_nested_spawns.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/scheduler_nested_spawns.stdout");

    assert_run_and_direct_source_stdout_with_timeout(
        "aura-scheduler-nested-spawns",
        source,
        std::time::Duration::from_secs(20),
        expected,
    );
}

#[test]
fn explicit_four_worker_queue_and_task_handles_match_backends() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/multicore_queue_task_matrix.au");
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/multicore_queue_task_matrix.stdout"
    );
    let (temp, source_path) = write_temp_source("aura-four-worker-task-parity", source);

    let mut mir = Command::new(aura_bin());
    mir.env("AURA_WORKERS", "4")
        .args(["run", "--backend", "mir"])
        .arg(&source_path);
    let mir = command_output_with_timeout(
        mir,
        std::time::Duration::from_secs(20),
        "four-worker MIR task parity",
    );
    assert!(
        mir.status.success(),
        "four-worker MIR run should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&mir.stdout), expected);

    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build four-worker direct fixture");
    assert!(
        build.status.success(),
        "four-worker direct fixture should build, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut direct = generated_binary(&output_path);
    direct.env("AURA_WORKERS", "4");
    let direct = command_output_with_timeout(
        direct,
        std::time::Duration::from_secs(20),
        "four-worker direct task parity",
    );
    assert!(
        direct.status.success(),
        "four-worker direct run should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&direct.stdout), expected);
}

#[test]
fn explicit_four_worker_queue_stress_preserves_integrity_and_per_producer_fifo() {
    // Four consumers still contend across workers for admission to the next
    // receive. The one-slot ticket Queue serializes only observation/dequeue,
    // letting the fixture reconstruct order without fixing producer interleaving.
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/multicore_queue_stress.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/multicore_queue_stress.stdout");

    assert_mir_and_direct_source_stdout_with_timeout_and_workers(
        "aura-four-worker-queue-stress",
        source,
        std::time::Duration::from_secs(30),
        expected,
        4,
    );
}

#[test]
fn single_worker_queue_stress_preserves_integrity_without_promising_consumer_fairness() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/multicore_queue_stress.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/multicore_queue_stress.stdout");

    assert_mir_and_direct_source_stdout_with_timeout_and_workers(
        "aura-single-worker-queue-stress",
        source,
        std::time::Duration::from_secs(30),
        expected,
        1,
    );
}

#[test]
fn explicit_four_worker_cancellation_and_task_failures_remain_isolated() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/multicore_cancellation_failure_isolation.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/multicore_cancellation_failure_isolation.stdout"
    );

    assert_mir_and_direct_source_stdout_with_timeout_and_workers(
        "aura-four-worker-cancellation-failure-isolation",
        source,
        std::time::Duration::from_secs(30),
        expected,
        4,
    );
}

#[test]
fn four_worker_prints_are_complete_atomic_lines_on_both_backends() {
    let source = r#"def print_many(label: str) -> None:
    value32: float32 = 1.25
    for index in range(200):
        print(f"{label}:{index}:abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ")
        print(true)
        print(value32)
        print(2.5)

def main():
    with TaskGroup() as tasks:
        tasks.start_soon(print_many, "alpha")
        tasks.start_soon(print_many, "beta")
        tasks.start_soon(print_many, "gamma")
        tasks.start_soon(print_many, "delta")
"#;
    let (temp, source_path) = write_temp_source("aura-four-worker-atomic-print", source);

    let assert_complete_lines = |stdout: Vec<u8>, backend: &str| {
        let mut actual = String::from_utf8(stdout)
            .expect("atomic print output should be UTF-8")
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut expected = Vec::new();
        for label in ["alpha", "beta", "gamma", "delta"] {
            for index in 0..200 {
                expected.push(format!(
                    "{label}:{index}:abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
                ));
                expected.push("true".to_string());
                expected.push("1.25".to_string());
                expected.push("2.5".to_string());
            }
        }
        actual.sort();
        expected.sort();
        assert_eq!(
            actual, expected,
            "{backend} concurrent print calls must each publish one complete line"
        );
    };

    let mut mir = Command::new(aura_bin());
    mir.env("AURA_WORKERS", "4")
        .args(["run", "--backend", "mir"])
        .arg(&source_path);
    let mir = command_output_with_timeout(
        mir,
        std::time::Duration::from_secs(20),
        "four-worker MIR atomic print",
    );
    assert!(
        mir.status.success(),
        "four-worker MIR atomic-print fixture should run, stderr was:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );
    assert_complete_lines(mir.stdout, "MIR");

    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build atomic-print fixture");
    assert!(
        build.status.success(),
        "atomic-print fixture should build, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut command = generated_binary(&output_path);
    command.env("AURA_WORKERS", "4");
    let output = command_output_with_timeout(
        command,
        std::time::Duration::from_secs(20),
        "four-worker direct atomic print",
    );
    assert!(
        output.status.success(),
        "four-worker atomic-print fixture should run, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_complete_lines(output.stdout, "direct");
}

#[test]
fn invalid_worker_override_is_au4006_on_mir_and_direct_backends() {
    let source = r#"def child() -> int32:
    return 7

def main():
    with TaskGroup() as tasks:
        task = tasks.start(child)
        match task.result():
            case TaskResult.Ready(value):
                print(value)
            case TaskResult.Error(message):
                print(message)
            case TaskResult.TimedOut:
                print("timed-out")
            case TaskResult.Cancelled:
                print("cancelled")
"#;
    let (temp, source_path) = write_temp_source("aura-invalid-worker-override", source);
    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build worker-override diagnostic fixture");
    assert!(
        build.status.success(),
        "worker-override diagnostic fixture should build, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    for invalid in ["0", "two"] {
        let expected =
            format!("invalid AURA_WORKERS value `{invalid}`: expected a positive integer");

        let mir = Command::new(aura_bin())
            .env("AURA_WORKERS", invalid)
            .args(["run", "--backend", "mir"])
            .arg(&source_path)
            .output()
            .expect("failed to run invalid worker override through MIR");
        assert!(
            !mir.status.success(),
            "invalid worker override `{invalid}` unexpectedly succeeded through MIR"
        );
        let mir_stderr = String::from_utf8_lossy(&mir.stderr);
        assert!(mir_stderr.contains("error[AU4006]"), "{mir_stderr}");
        assert!(mir_stderr.contains(&expected), "{mir_stderr}");

        let direct = generated_binary(&output_path)
            .env("AURA_WORKERS", invalid)
            .output()
            .expect("failed to run invalid worker override through direct backend");
        assert!(
            !direct.status.success(),
            "invalid worker override `{invalid}` unexpectedly succeeded through direct backend"
        );
        let direct_stderr = String::from_utf8_lossy(&direct.stderr);
        assert!(direct_stderr.contains("error[AU4006]"), "{direct_stderr}");
        assert!(direct_stderr.contains(&expected), "{direct_stderr}");
    }
}

#[test]
fn old_product_environment_names_are_not_honored() {
    let old_prefix = ["AURO", "RA"].concat();
    let old_worker = format!("{old_prefix}_WORKERS");
    let old_blocking_workers = format!("{old_prefix}_BLOCKING_WORKERS");
    let old_blocking_capacity = format!("{old_prefix}_BLOCKING_QUEUE_CAPACITY");
    let old_cache = format!("{old_prefix}_CACHE_DIR");
    let (temp, source_path) =
        write_temp_source("aura-old-environment-names", "def main():\n    print(42)\n");
    let ignored_cache = temp.path().join("ignored-old-cache");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("isolated home should be creatable");

    for backend in ["mir", "direct"] {
        let output = Command::new(aura_bin())
            .env_remove("AURA_WORKERS")
            .env_remove("AURA_BLOCKING_WORKERS")
            .env_remove("AURA_BLOCKING_QUEUE_CAPACITY")
            .env_remove("AURA_CACHE_DIR")
            .env(&old_worker, "0")
            .env(&old_blocking_workers, "0")
            .env(&old_blocking_capacity, "0")
            .env(&old_cache, &ignored_cache)
            .env("HOME", &home)
            .args(["run", "--backend", backend])
            .arg(&source_path)
            .output()
            .expect("failed to run with old product environment names");
        assert!(
            output.status.success(),
            "old environment names must be ignored on {backend}, stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "42\n");
    }

    assert!(
        !ignored_cache.exists(),
        "the old cache override must not create or select its path"
    );
    assert!(
        home.join(".cache/aura/native").is_dir(),
        "direct execution should use the Aura default cache beneath HOME"
    );
}

#[test]
fn invalid_blocking_pool_configuration_is_au4006_before_user_code_on_every_runtime_path() {
    use std::ffi::OsString;

    let source = r#"import fs
import io

def write_marker() -> Result[None, io.Error]:
    with marker = try fs.create("USER_CODE_RAN"):
        return Result.Ok(None)

def main():
    print("USER_CODE_RAN")
    write_marker()
"#;
    let (temp, source_path) = write_temp_source("aura-invalid-blocking-pool-configuration", source);
    let standalone_path = temp.path().join("standalone");
    let build = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(&standalone_path)
        .arg(&source_path)
        .output()
        .expect("failed to build standalone blocking-pool configuration fixture");
    assert!(
        build.status.success(),
        "standalone fixture should build, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let overflow = (usize::MAX as u128 + 1).to_string();
    let mut invalid_values = vec![
        OsString::from(""),
        OsString::from("0"),
        OsString::from("+1"),
        OsString::from("-1"),
        OsString::from(" 1"),
        OsString::from("1 "),
        OsString::from("1.0"),
        OsString::from("١"),
        OsString::from(overflow),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        invalid_values.push(OsString::from_vec(b"invalid-\xff".to_vec()));
    }

    for setting in ["AURA_BLOCKING_WORKERS", "AURA_BLOCKING_QUEUE_CAPACITY"] {
        for invalid in &invalid_values {
            let rendered = invalid.to_string_lossy();
            let expected =
                format!("invalid {setting} value `{rendered}`: expected a positive integer");

            let mut mir = Command::new(aura_bin());
            mir.env_remove("AURA_BLOCKING_WORKERS")
                .env_remove("AURA_BLOCKING_QUEUE_CAPACITY")
                .env(setting, invalid)
                .current_dir(temp.path())
                .args(["run", "--backend", "mir"])
                .arg(&source_path);

            let mut direct = Command::new(aura_bin());
            direct
                .env_remove("AURA_BLOCKING_WORKERS")
                .env_remove("AURA_BLOCKING_QUEUE_CAPACITY")
                .env(setting, invalid)
                .current_dir(temp.path())
                .args(["run", "--backend", "direct"])
                .arg(&source_path);

            let mut standalone = generated_binary(&standalone_path);
            standalone
                .env_remove("AURA_BLOCKING_WORKERS")
                .env_remove("AURA_BLOCKING_QUEUE_CAPACITY")
                .env(setting, invalid)
                .current_dir(temp.path());

            for (path, output) in [
                (
                    "forced MIR",
                    mir.output()
                        .expect("failed to run invalid config through forced MIR"),
                ),
                (
                    "forced direct",
                    direct
                        .output()
                        .expect("failed to run invalid config through forced direct"),
                ),
                (
                    "standalone",
                    standalone
                        .output()
                        .expect("failed to run invalid config through standalone binary"),
                ),
            ] {
                assert!(
                    !output.status.success(),
                    "{path} unexpectedly accepted {setting}={rendered}"
                );
                assert!(
                    output.stdout.is_empty(),
                    "{path} ran user code for {setting}={rendered}; stdout was:\n{}",
                    String::from_utf8_lossy(&output.stdout)
                );
                assert!(
                    !temp.path().join("USER_CODE_RAN").exists(),
                    "{path} ran the user filesystem side effect for {setting}={rendered}"
                );
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(stderr.contains("error[AU4006]"), "{path}: {stderr}");
                assert!(stderr.contains(&expected), "{path}: {stderr}");
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn bounded_blocking_pool_admission_preserves_scheduler_progress_on_every_runtime_path() {
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::mpsc;

    let _watchdog_guard = serialize_bounded_blocking_pool_watchdog();

    struct FifoWriterGate {
        opened: mpsc::Receiver<Result<(), String>>,
        release: mpsc::Sender<()>,
        handle: std::thread::JoinHandle<Result<(), String>>,
    }

    fn spawn_fifo_writer(path: PathBuf, payload: &'static [u8]) -> FifoWriterGate {
        let (opened_sender, opened) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            let mut writer = loop {
                match fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)
                {
                    Ok(writer) => break writer,
                    Err(error)
                        if error.raw_os_error() == Some(libc::ENXIO)
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::park_timeout(std::time::Duration::from_millis(1));
                    }
                    Err(error) => {
                        let message =
                            format!("failed to open FIFO writer `{}`: {error}", path.display());
                        let _ = opened_sender.send(Err(message.clone()));
                        return Err(message);
                    }
                }
            };
            opened_sender.send(Ok(())).map_err(|error| {
                format!("failed to report open FIFO `{}`: {error}", path.display())
            })?;
            release_receiver
                .recv_timeout(std::time::Duration::from_secs(120))
                .map_err(|error| {
                    format!(
                        "FIFO writer `{}` was not released before its watchdog: {error}",
                        path.display()
                    )
                })?;
            writer.write_all(payload).map_err(|error| {
                format!(
                    "failed to release FIFO reader `{}`: {error}",
                    path.display()
                )
            })
        });
        FifoWriterGate {
            opened,
            release,
            handle,
        }
    }

    fn receive_line(
        label: &str,
        receiver: &mpsc::Receiver<Result<String, String>>,
        deadline: std::time::Instant,
    ) -> String {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => panic!("{label} stdout reader failed: {error}"),
            Err(error) => panic!("{label} did not emit its next handshake line: {error}"),
        }
    }

    fn expect_line(
        label: &str,
        receiver: &mpsc::Receiver<Result<String, String>>,
        deadline: std::time::Instant,
        lines: &mut Vec<String>,
        expected: &str,
    ) {
        let line = receive_line(label, receiver, deadline);
        assert_eq!(
            line,
            expected,
            "{label} emitted an unexpected handshake; output so far was:\n{}",
            lines.concat()
        );
        lines.push(line);
    }

    fn wait_for_fifo_reader(label: &str, gate: &FifoWriterGate, deadline: std::time::Instant) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match gate.opened.recv_timeout(remaining) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => panic!("{label}: {error}"),
            Err(error) => panic!("{label} did not enter its blocking FIFO read: {error}"),
        }
    }

    fn run_case(
        label: &str,
        mut command: Command,
        first_fifo: &std::path::Path,
        second_fifo: &std::path::Path,
        expected_stdout: &str,
    ) -> String {
        let first_gate = spawn_fifo_writer(first_fifo.to_path_buf(), b"gate-one");
        let second_gate = spawn_fifo_writer(second_fifo.to_path_buf(), b"gate-two");

        command
            .env("AURA_WORKERS", "1")
            .env("AURA_BLOCKING_WORKERS", "2")
            .env("AURA_BLOCKING_QUEUE_CAPACITY", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .unwrap_or_else(|error| panic!("{label} failed to start: {error}"));
        let stdout = child.stdout.take().expect("captured stdout should exist");
        let stderr = child.stderr.take().expect("captured stderr should exist");

        let (line_sender, line_receiver) = mpsc::channel();
        let stdout_reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut captured = String::new();
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        captured.push_str(&line);
                        if line_sender.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = line_sender.send(Err(error.to_string()));
                        break;
                    }
                }
            }
            captured
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut captured = Vec::new();
            let _ = BufReader::new(stderr).read_to_end(&mut captured);
            captured
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut lines = Vec::new();
        expect_line(
            label,
            &line_receiver,
            deadline,
            &mut lines,
            "gate-one-entered\n",
        );
        expect_line(
            label,
            &line_receiver,
            deadline,
            &mut lines,
            "gate-two-entered\n",
        );
        wait_for_fifo_reader(label, &first_gate, deadline);
        wait_for_fifo_reader(label, &second_gate, deadline);

        expect_line(
            label,
            &line_receiver,
            deadline,
            &mut lines,
            "ordinary-one-entered\n",
        );
        expect_line(
            label,
            &line_receiver,
            deadline,
            &mut lines,
            "ordinary-two-entered\n",
        );
        expect_line(
            label,
            &line_receiver,
            deadline,
            &mut lines,
            "scheduler-live\n",
        );

        first_gate
            .release
            .send(())
            .expect("first FIFO writer should still be waiting for release");
        expect_line(
            label,
            &line_receiver,
            deadline,
            &mut lines,
            "ordinary-one\n",
        );
        expect_line(
            label,
            &line_receiver,
            deadline,
            &mut lines,
            "ordinary-two\n",
        );
        expect_line(label, &line_receiver, deadline, &mut lines, "gate-one\n");

        second_gate
            .release
            .send(())
            .expect("second FIFO writer should still be waiting for release");
        expect_line(label, &line_receiver, deadline, &mut lines, "gate-two\n");

        let status = wait_with_timeout(
            &mut child,
            deadline.saturating_duration_since(std::time::Instant::now()),
        );
        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let stdout = stdout_reader
            .join()
            .expect("stdout reader should not panic");
        let stderr = stderr_reader
            .join()
            .expect("stderr reader should not panic");
        first_gate
            .handle
            .join()
            .expect("first FIFO writer should not panic")
            .expect("first FIFO writer should complete");
        second_gate
            .handle
            .join()
            .expect("second FIFO writer should not panic")
            .expect("second FIFO writer should complete");

        let Some(status) = status else {
            panic!(
                "{label} did not exit before its watchdog; stdout was:\n{stdout}\nstderr was:\n{}",
                String::from_utf8_lossy(&stderr)
            );
        };
        assert!(
            status.success(),
            "{label} failed; stdout was:\n{stdout}\nstderr was:\n{}",
            String::from_utf8_lossy(&stderr)
        );
        assert_eq!(stdout, expected_stdout, "{label} stdout changed");
        stdout
    }

    let temp = TempDir::new("aura-blocking-pool-product-saturation");
    let first_fifo = temp.path().join("gate-one.fifo");
    let second_fifo = temp.path().join("gate-two.fifo");
    for fifo in [&first_fifo, &second_fifo] {
        let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())
            .expect("temporary FIFO path should not contain NUL");
        let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
        assert_eq!(
            result,
            0,
            "failed to create FIFO `{}`: {}",
            fifo.display(),
            std::io::Error::last_os_error()
        );
    }

    let ordinary_one = temp.path().join("ordinary-one.txt");
    let ordinary_two = temp.path().join("ordinary-two.txt");
    fs::write(&ordinary_one, "ordinary-one").expect("first ordinary file should be writable");
    fs::write(&ordinary_two, "ordinary-two").expect("second ordinary file should be writable");

    let path_literal = |path: &PathBuf| {
        serde_json::to_string(
            path.to_str()
                .expect("temporary product-regression path should be UTF-8"),
        )
        .expect("temporary path should encode as an Aura string")
    };
    let source = format!(
        r#"import fs

def read_gate_one(path: str) -> str:
    print("gate-one-entered")
    match own fs.read_to_string(path):
        case Result.Ok(text):
            return text
        case Result.Err(_):
            return "gate-one-error"

def read_gate_two(path: str) -> str:
    print("gate-two-entered")
    match own fs.read_to_string(path):
        case Result.Ok(text):
            return text
        case Result.Err(_):
            return "gate-two-error"

def read_ordinary_one(path: str) -> str:
    print("ordinary-one-entered")
    match own fs.read_to_string(path):
        case Result.Ok(text):
            return text
        case Result.Err(_):
            return "ordinary-one-error"

def read_ordinary_two(path: str) -> str:
    print("ordinary-two-entered")
    match own fs.read_to_string(path):
        case Result.Ok(text):
            return text
        case Result.Err(_):
            return "ordinary-two-error"

def prove_scheduler_is_live() -> None:
    sleep(1ms)
    print("scheduler-live")

def print_task(task: own Task[str]) -> None:
    match own task.result():
        case TaskResult.Ready(text):
            print(text)
        case TaskResult.Error(_):
            print("task-error")
        case TaskResult.TimedOut:
            print("task-timed-out")
        case TaskResult.Cancelled:
            print("task-cancelled")

def main() -> int32:
    with TaskGroup() as group:
        gate_one = group.start(read_gate_one, {first_fifo})
        gate_two = group.start(read_gate_two, {second_fifo})
        ordinary_one = group.start(read_ordinary_one, {ordinary_one})
        ordinary_two = group.start(read_ordinary_two, {ordinary_two})
        group.start_soon(prove_scheduler_is_live)
        print_task(ordinary_one)
        print_task(ordinary_two)
        print_task(gate_one)
        print_task(gate_two)
    return 0
"#,
        first_fifo = path_literal(&first_fifo),
        second_fifo = path_literal(&second_fifo),
        ordinary_one = path_literal(&ordinary_one),
        ordinary_two = path_literal(&ordinary_two),
    );
    let source_path = temp.path().join("main.au");
    fs::write(&source_path, source).expect("product-regression source should be writable");

    let standalone_path = temp.path().join("standalone");
    let build = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(&standalone_path)
        .arg(&source_path)
        .output()
        .expect("failed to build standalone bounded-pool product regression");
    assert!(
        build.status.success(),
        "standalone bounded-pool product regression should build, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let expected = concat!(
        "gate-one-entered\n",
        "gate-two-entered\n",
        "ordinary-one-entered\n",
        "ordinary-two-entered\n",
        "scheduler-live\n",
        "ordinary-one\n",
        "ordinary-two\n",
        "gate-one\n",
        "gate-two\n",
    );

    let mut mir = Command::new(aura_bin());
    mir.current_dir(temp.path())
        .args(["run", "--backend", "mir"])
        .arg(&source_path);
    let mir_stdout = run_case(
        "forced MIR bounded-pool saturation",
        mir,
        &first_fifo,
        &second_fifo,
        expected,
    );

    let mut direct = Command::new(aura_bin());
    direct
        .env("AURA_CACHE_DIR", temp.path().join("direct-cache"))
        .current_dir(temp.path())
        .args(["run", "--backend", "direct"])
        .arg(&source_path);
    let direct_stdout = run_case(
        "forced direct bounded-pool saturation",
        direct,
        &first_fifo,
        &second_fifo,
        expected,
    );

    let mut standalone = generated_binary(&standalone_path);
    standalone.current_dir(temp.path());
    let standalone_stdout = run_case(
        "standalone bounded-pool saturation",
        standalone,
        &first_fifo,
        &second_fifo,
        expected,
    );

    assert_eq!(mir_stdout, direct_stdout);
    assert_eq!(mir_stdout, standalone_stdout);
}

#[cfg(unix)]
#[test]
fn bounded_blocking_pool_timeout_and_cancellation_preserve_acceptance_boundary_parity() {
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};

    let _watchdog_guard = serialize_bounded_blocking_pool_watchdog();

    struct WriterGate {
        opened: mpsc::Receiver<Result<(), String>>,
        release: mpsc::Sender<()>,
        handle: std::thread::JoinHandle<Result<(), String>>,
    }

    struct ForbiddenReaderProbe {
        observed: mpsc::Receiver<Result<(), String>>,
        stop: Arc<AtomicBool>,
        handle: std::thread::JoinHandle<Result<(), String>>,
    }

    fn spawn_writer_gate(path: PathBuf, payload: &'static [u8]) -> WriterGate {
        let (opened_sender, opened) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            let mut writer = loop {
                match fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)
                {
                    Ok(writer) => break writer,
                    Err(error)
                        if error.raw_os_error() == Some(libc::ENXIO)
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::park_timeout(std::time::Duration::from_millis(1));
                    }
                    Err(error) => {
                        let message =
                            format!("failed to open FIFO writer `{}`: {error}", path.display());
                        let _ = opened_sender.send(Err(message.clone()));
                        return Err(message);
                    }
                }
            };
            opened_sender.send(Ok(())).map_err(|error| {
                format!(
                    "failed to report FIFO acceptance `{}`: {error}",
                    path.display()
                )
            })?;
            release_receiver
                .recv_timeout(std::time::Duration::from_secs(120))
                .map_err(|error| {
                    format!(
                        "FIFO writer `{}` was not released before its watchdog: {error}",
                        path.display()
                    )
                })?;
            writer
                .write_all(payload)
                .map_err(|error| format!("failed to write FIFO `{}`: {error}", path.display()))
        });
        WriterGate {
            opened,
            release,
            handle,
        }
    }

    fn spawn_forbidden_reader_probe(path: PathBuf) -> ForbiddenReaderProbe {
        let (observed_sender, observed) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let handle = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)
                {
                    Ok(mut writer) => {
                        let result = writer.write_all(b"forbidden").map_err(|error| {
                            format!(
                                "failed to release forbidden FIFO reader `{}`: {error}",
                                path.display()
                            )
                        });
                        let report = result.as_ref().map(|_| ()).map_err(Clone::clone);
                        let _ = observed_sender.send(report);
                        return result;
                    }
                    Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                        std::thread::park_timeout(std::time::Duration::from_millis(1));
                    }
                    Err(error) => {
                        let message =
                            format!("failed to probe FIFO reader `{}`: {error}", path.display());
                        let _ = observed_sender.send(Err(message.clone()));
                        return Err(message);
                    }
                }
            }
            Ok(())
        });
        ForbiddenReaderProbe {
            observed,
            stop,
            handle,
        }
    }

    fn wait_opened(label: &str, gate: &WriterGate, deadline: std::time::Instant) {
        match gate
            .opened
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => panic!("{label}: {error}"),
            Err(error) => panic!("{label} did not reach its accepted FIFO operation: {error}"),
        }
    }

    fn release(label: &str, gate: &WriterGate) {
        gate.release
            .send(())
            .unwrap_or_else(|error| panic!("{label} release failed: {error}"));
    }

    fn expect_line(
        label: &str,
        receiver: &mpsc::Receiver<Result<String, String>>,
        deadline: std::time::Instant,
        output: &mut String,
        expected: &str,
    ) {
        let line = match receiver
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => panic!("{label} stdout reader failed: {error}"),
            Err(error) => panic!(
                "{label} did not emit `{expected}` before its watchdog: {error}; output was:\n{output}"
            ),
        };
        assert_eq!(
            line, expected,
            "{label} emitted an unexpected line; output so far was:\n{output}"
        );
        output.push_str(&line);
    }

    fn finish_gate(label: &str, gate: WriterGate) {
        gate.handle
            .join()
            .unwrap_or_else(|_| panic!("{label} writer panicked"))
            .unwrap_or_else(|error| panic!("{label}: {error}"));
    }

    fn finish_forbidden_probe(label: &str, probe: ForbiddenReaderProbe) {
        assert!(
            matches!(probe.observed.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "{label} host operation executed despite timing out or being cancelled before admission"
        );
        probe.stop.store(true, Ordering::Release);
        probe
            .handle
            .join()
            .unwrap_or_else(|_| panic!("{label} probe panicked"))
            .unwrap_or_else(|error| panic!("{label}: {error}"));
        assert!(
            matches!(
                probe.observed.try_recv(),
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected)
            ),
            "{label} host operation executed before the zero-execution probe stopped"
        );
    }

    fn run_case(label: &str, mut command: Command, paths: &[PathBuf], expected: &str) -> String {
        let pre_timeout_active = spawn_writer_gate(paths[0].clone(), b"pre-timeout-active");
        let pre_timeout_pending = spawn_writer_gate(paths[1].clone(), b"pre-timeout-pending");
        let pre_timeout_forbidden = spawn_forbidden_reader_probe(paths[2].clone());
        let accepted_timeout = spawn_writer_gate(paths[3].clone(), b"late-timeout-value");
        let pre_cancel_active = spawn_writer_gate(paths[4].clone(), b"pre-cancel-active");
        let pre_cancel_pending = spawn_writer_gate(paths[5].clone(), b"pre-cancel-pending");
        let pre_cancel_forbidden = spawn_forbidden_reader_probe(paths[6].clone());
        let accepted_cancel = spawn_writer_gate(paths[7].clone(), b"late-cancel-value");

        command
            .env("AURA_WORKERS", "1")
            .env("AURA_BLOCKING_WORKERS", "1")
            .env("AURA_BLOCKING_QUEUE_CAPACITY", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .unwrap_or_else(|error| panic!("{label} failed to start: {error}"));
        let stdout = child.stdout.take().expect("captured stdout should exist");
        let stderr = child.stderr.take().expect("captured stderr should exist");
        let (line_sender, line_receiver) = mpsc::channel();
        let stdout_reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut captured = String::new();
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        captured.push_str(&line);
                        if line_sender.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = line_sender.send(Err(error.to_string()));
                        break;
                    }
                }
            }
            captured
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut captured = Vec::new();
            let _ = BufReader::new(stderr).read_to_end(&mut captured);
            captured
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut output = String::new();
        for expected_line in [
            "phase-pre-timeout\n",
            "pre-timeout-active-entered\n",
            "pre-timeout-pending-entered\n",
            "pre-timeout-target-entered\n",
        ] {
            expect_line(label, &line_receiver, deadline, &mut output, expected_line);
        }
        wait_opened(label, &pre_timeout_active, deadline);
        expect_line(label, &line_receiver, deadline, &mut output, "timed-out\n");
        assert!(matches!(
            pre_timeout_forbidden.observed.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        release(label, &pre_timeout_active);
        wait_opened(label, &pre_timeout_pending, deadline);
        release(label, &pre_timeout_pending);
        for expected_line in ["pre-timeout-active\n", "pre-timeout-pending\n"] {
            expect_line(label, &line_receiver, deadline, &mut output, expected_line);
        }

        for expected_line in ["phase-accepted-timeout\n", "accepted-timeout-entered\n"] {
            expect_line(label, &line_receiver, deadline, &mut output, expected_line);
        }
        wait_opened(label, &accepted_timeout, deadline);
        expect_line(label, &line_receiver, deadline, &mut output, "timed-out\n");
        release(label, &accepted_timeout);
        expect_line(
            label,
            &line_receiver,
            deadline,
            &mut output,
            "timeout-sentinel\n",
        );

        for expected_line in [
            "phase-pre-cancel\n",
            "pre-cancel-active-entered\n",
            "pre-cancel-pending-entered\n",
            "pre-cancel-target-entered\n",
            "cancelled\n",
        ] {
            expect_line(label, &line_receiver, deadline, &mut output, expected_line);
        }
        wait_opened(label, &pre_cancel_active, deadline);
        assert!(matches!(
            pre_cancel_forbidden.observed.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        release(label, &pre_cancel_active);
        wait_opened(label, &pre_cancel_pending, deadline);
        release(label, &pre_cancel_pending);
        for expected_line in ["pre-cancel-active\n", "pre-cancel-pending\n"] {
            expect_line(label, &line_receiver, deadline, &mut output, expected_line);
        }

        for expected_line in ["phase-accepted-cancel\n", "accepted-cancel-entered\n"] {
            expect_line(label, &line_receiver, deadline, &mut output, expected_line);
        }
        wait_opened(label, &accepted_cancel, deadline);
        expect_line(label, &line_receiver, deadline, &mut output, "cancelled\n");
        release(label, &accepted_cancel);
        expect_line(
            label,
            &line_receiver,
            deadline,
            &mut output,
            "cancel-sentinel\n",
        );
        expect_line(label, &line_receiver, deadline, &mut output, "done\n");

        let status = wait_with_timeout(
            &mut child,
            deadline.saturating_duration_since(std::time::Instant::now()),
        );
        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let captured_stdout = stdout_reader
            .join()
            .expect("stdout reader should not panic");
        let captured_stderr = stderr_reader
            .join()
            .expect("stderr reader should not panic");

        finish_gate(label, pre_timeout_active);
        finish_gate(label, pre_timeout_pending);
        finish_forbidden_probe("pre-admission timeout", pre_timeout_forbidden);
        finish_gate(label, accepted_timeout);
        finish_gate(label, pre_cancel_active);
        finish_gate(label, pre_cancel_pending);
        finish_forbidden_probe("pre-admission cancellation", pre_cancel_forbidden);
        finish_gate(label, accepted_cancel);

        let Some(status) = status else {
            panic!(
                "{label} did not exit before its watchdog; stdout was:\n{captured_stdout}\nstderr was:\n{}",
                String::from_utf8_lossy(&captured_stderr)
            );
        };
        assert!(
            status.success(),
            "{label} failed; stdout was:\n{captured_stdout}\nstderr was:\n{}",
            String::from_utf8_lossy(&captured_stderr)
        );
        assert_eq!(captured_stdout, output);
        assert_eq!(captured_stdout, expected, "{label} output changed");
        captured_stdout
    }

    let temp = TempDir::new("aura-blocking-pool-acceptance-boundaries");
    let fifo_names = [
        "pre-timeout-active.fifo",
        "pre-timeout-pending.fifo",
        "pre-timeout-forbidden.fifo",
        "accepted-timeout.fifo",
        "pre-cancel-active.fifo",
        "pre-cancel-pending.fifo",
        "pre-cancel-forbidden.fifo",
        "accepted-cancel.fifo",
    ];
    let fifo_paths = fifo_names
        .iter()
        .map(|name| temp.path().join(name))
        .collect::<Vec<_>>();
    for fifo in &fifo_paths {
        let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())
            .expect("temporary FIFO path should not contain NUL");
        let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
        assert_eq!(
            result,
            0,
            "failed to create FIFO `{}`: {}",
            fifo.display(),
            std::io::Error::last_os_error()
        );
    }

    let timeout_sentinel = temp.path().join("timeout-sentinel.txt");
    let cancel_sentinel = temp.path().join("cancel-sentinel.txt");
    fs::write(&timeout_sentinel, "timeout-sentinel").expect("timeout sentinel should be writable");
    fs::write(&cancel_sentinel, "cancel-sentinel").expect("cancel sentinel should be writable");
    let path_literal = |path: &std::path::Path| {
        serde_json::to_string(
            path.to_str()
                .expect("temporary product-regression path should be UTF-8"),
        )
        .expect("temporary path should encode as an Aura string")
    };
    let source = format!(
        r#"import fs
import io
import net

def read_gate(path: str, entered: str) -> str:
    print(entered)
    match own fs.read_to_string(path):
        case Result.Ok(text):
            return text
        case Result.Err(_):
            return "gate-error"

def timed_tls(path: str, entered: str) -> str:
    print(entered)
    match own net.tls_connect_timeout("127.0.0.1:1", "localhost", path, 100ms):
        case Result.Ok(_):
            return "unexpected-ok"
        case Result.Err(io.Error.TimedOut):
            return "timed-out"
        case Result.Err(_):
            return "unexpected-error"

def cancellable_tls(path: str, entered: str) -> str:
    print(entered)
    match own net.tls_connect_timeout("127.0.0.1:1", "localhost", path, 30s):
        case Result.Ok(_):
            return "unexpected-ok"
        case Result.Err(io.Error.Cancelled):
            return "cancelled"
        case Result.Err(_):
            return "unexpected-error"

def print_task(task: own Task[str]) -> None:
    match own task.result():
        case TaskResult.Ready(text):
            print(text)
        case TaskResult.Error(_):
            print("task-error")
        case TaskResult.TimedOut:
            print("task-timed-out")
        case TaskResult.Cancelled:
            print("cancelled")

def print_file(path: str) -> None:
    match own fs.read_to_string(path):
        case Result.Ok(text):
            print(text)
        case Result.Err(_):
            print("sentinel-error")

def main() -> int32:
    print("phase-pre-timeout")
    with TaskGroup() as group:
        active = group.start(read_gate, {pre_timeout_active}, "pre-timeout-active-entered")
        pending = group.start(read_gate, {pre_timeout_pending}, "pre-timeout-pending-entered")
        target = group.start(timed_tls, {pre_timeout_forbidden}, "pre-timeout-target-entered")
        print_task(target)
        print_task(active)
        print_task(pending)

    print("phase-accepted-timeout")
    print(timed_tls({accepted_timeout}, "accepted-timeout-entered"))
    print_file({timeout_sentinel})

    print("phase-pre-cancel")
    with TaskGroup() as outer:
        active = outer.start(read_gate, {pre_cancel_active}, "pre-cancel-active-entered")
        pending = outer.start(read_gate, {pre_cancel_pending}, "pre-cancel-pending-entered")
        with TaskGroup() as cancelled_group:
            target = cancelled_group.start(cancellable_tls, {pre_cancel_forbidden}, "pre-cancel-target-entered")
            yield_now()
            cancelled_group.cancel()
            print_task(target)
        print_task(active)
        print_task(pending)

    print("phase-accepted-cancel")
    with TaskGroup() as cancelled_group:
        target = cancelled_group.start(cancellable_tls, {accepted_cancel}, "accepted-cancel-entered")
        yield_now()
        cancelled_group.cancel()
        print_task(target)
    print_file({cancel_sentinel})
    print("done")
    return 0
"#,
        pre_timeout_active = path_literal(&fifo_paths[0]),
        pre_timeout_pending = path_literal(&fifo_paths[1]),
        pre_timeout_forbidden = path_literal(&fifo_paths[2]),
        accepted_timeout = path_literal(&fifo_paths[3]),
        pre_cancel_active = path_literal(&fifo_paths[4]),
        pre_cancel_pending = path_literal(&fifo_paths[5]),
        pre_cancel_forbidden = path_literal(&fifo_paths[6]),
        accepted_cancel = path_literal(&fifo_paths[7]),
        timeout_sentinel = path_literal(&timeout_sentinel),
        cancel_sentinel = path_literal(&cancel_sentinel),
    );
    let source_path = temp.path().join("main.au");
    fs::write(&source_path, source).expect("acceptance-boundary source should be writable");

    let standalone_path = temp.path().join("standalone");
    let build = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(&standalone_path)
        .arg(&source_path)
        .output()
        .expect("failed to build standalone acceptance-boundary regression");
    assert!(
        build.status.success(),
        "standalone acceptance-boundary regression should build, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let expected = concat!(
        "phase-pre-timeout\n",
        "pre-timeout-active-entered\n",
        "pre-timeout-pending-entered\n",
        "pre-timeout-target-entered\n",
        "timed-out\n",
        "pre-timeout-active\n",
        "pre-timeout-pending\n",
        "phase-accepted-timeout\n",
        "accepted-timeout-entered\n",
        "timed-out\n",
        "timeout-sentinel\n",
        "phase-pre-cancel\n",
        "pre-cancel-active-entered\n",
        "pre-cancel-pending-entered\n",
        "pre-cancel-target-entered\n",
        "cancelled\n",
        "pre-cancel-active\n",
        "pre-cancel-pending\n",
        "phase-accepted-cancel\n",
        "accepted-cancel-entered\n",
        "cancelled\n",
        "cancel-sentinel\n",
        "done\n",
    );

    let mut mir = Command::new(aura_bin());
    mir.current_dir(temp.path())
        .args(["run", "--backend", "mir"])
        .arg(&source_path);
    let mir_stdout = run_case(
        "forced MIR acceptance boundaries",
        mir,
        &fifo_paths,
        expected,
    );

    let mut direct = Command::new(aura_bin());
    direct
        .env("AURA_CACHE_DIR", temp.path().join("direct-cache"))
        .current_dir(temp.path())
        .args(["run", "--backend", "direct"])
        .arg(&source_path);
    let direct_stdout = run_case(
        "forced direct acceptance boundaries",
        direct,
        &fifo_paths,
        expected,
    );

    let mut standalone = generated_binary(&standalone_path);
    standalone.current_dir(temp.path());
    let standalone_stdout = run_case(
        "standalone acceptance boundaries",
        standalone,
        &fifo_paths,
        expected,
    );

    assert_eq!(mir_stdout, direct_stdout);
    assert_eq!(mir_stdout, standalone_stdout);
}

#[test]
fn large_http_responses_complete_without_timing_out() {
    let temp = TempDir::new("aura-http-large-response");
    let body_path = temp.path().join("body.txt");
    fs::write(&body_path, "x".repeat(2_000_000)).expect("failed to write HTTP response body");
    let source = format!(
        r#"import fs
import io
import net

def serve(path: own str, addresses: Queue[str]) -> Result[None, io.Error]:
    with server = try net.http_listen("127.0.0.1:0"):
        addresses.put(try server.local_addr())
        req = try server.accept(timeout=5s)
        body = try fs.read_to_string(path)
        try req.respond_text(200, body, {{}})
        return Result.Ok(None)

def run() -> Result[None, io.Error]:
    with TaskGroup() as group:
        addresses = Queue[str](capacity=1)
        group.start_soon(serve, "{body_path}", addresses)
        match addresses.get(timeout=5s):
            case QueueReceive.Item(address):
                resp = try net.http_request_text_timeout("GET", "http://" + address + "/big", "x", {{}}, 5s)
                with r = resp:
                    print(r.status())
                    text = try r.text()
                    print(text.len())
            case QueueReceive.Closed:
                return Result.Err(io.Error.Other(message="HTTP address queue closed"))
            case QueueReceive.TimedOut:
                return Result.Err(io.Error.TimedOut)
            case QueueReceive.Cancelled:
                return Result.Err(io.Error.Cancelled)
        return Result.Ok(None)

def main() -> int32:
    match run():
        case Result.Ok(_):
            return 0
        case Result.Err(err):
            print(err)
            return 1
"#,
        body_path = body_path.display()
    );
    assert_run_and_direct_source_stdout("aura-http-raised-response-cap", &source, "200\n2000000\n");
}

#[test]
fn http_declared_response_above_fixed_cap_is_typed_on_both_backends() {
    let source = r#"import io
import net

def serve(addresses: Queue[str]) -> Result[None, io.Error]:
    with listener = try net.http_listen("127.0.0.1:0"):
        addresses.put(try listener.local_addr())
        request = try listener.accept(timeout=5s)
        try request.respond_text(200, "", {"Content-Length": "16777217"})
        return Result.Ok(None)

def run() -> Result[None, io.Error]:
    with TaskGroup() as group:
        addresses = Queue[str](capacity=1)
        group.start_soon(serve, addresses)
        match addresses.get(timeout=5s):
            case QueueReceive.Item(address):
                response = net.http_request_text_timeout("GET", "http://" + address + "/oversized", "", {}, 5s)
                match response:
                    case Result.Err(io.Error.InvalidData):
                        print("http-too-large")
                    case Result.Err(error):
                        print(error)
                    case Result.Ok(_):
                        print("unexpected-success")
            case QueueReceive.Closed:
                return Result.Err(io.Error.Other(message="HTTP address queue closed"))
            case QueueReceive.TimedOut:
                return Result.Err(io.Error.TimedOut)
            case QueueReceive.Cancelled:
                return Result.Err(io.Error.Cancelled)
    return Result.Ok(None)

def main() -> int32:
    match run():
        case Result.Ok(_):
            return 0
        case Result.Err(error):
            print(error)
            return 1
"#;

    assert_run_and_direct_source_stdout("aura-http-fixed-response-cap", source, "http-too-large\n");
}

#[test]
fn check_rejects_huge_left_associative_expression_chains_without_crashing() {
    let mut expr = String::from("1");
    for _ in 0..5000 {
        expr.push_str(" + 1");
    }
    let source = format!("def main() -> int32:\n    value = {expr}\n    return value\n");
    let (_temp, source_path) = write_temp_source("aura-huge-chain", &source);

    let output = Command::new(aura_bin())
        .arg("check")
        .arg(&source_path)
        .output()
        .expect("failed to run aura check");

    assert!(
        !output.status.success(),
        "huge left-associative chains should fail gracefully"
    );
    assert_ne!(
        output.status.code(),
        None,
        "aura check should not die by signal on huge expression chains"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expression chain")
            || String::from_utf8_lossy(&output.stderr).contains("expression nesting"),
        "expected a structural diagnostic, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn compile_commands_emit_the_shared_structured_diagnostic_schema() {
    let (temp, source_path) = write_temp_source(
        "aura-structured-diagnostics",
        "def main():\n    print(missing)\n    print(also_missing)\n",
    );
    let output_path = temp.path().join("out");

    let commands = [
        vec![
            "check".to_string(),
            "--format".to_string(),
            "json".to_string(),
        ],
        vec![
            "run".to_string(),
            "--format".to_string(),
            "json".to_string(),
        ],
        vec![
            "build".to_string(),
            "--format".to_string(),
            "json".to_string(),
            "-o".to_string(),
            output_path.display().to_string(),
        ],
    ];

    for mut arguments in commands {
        let command_name = arguments[0].clone();
        arguments.push(source_path.display().to_string());
        let output = Command::new(aura_bin())
            .args(&arguments)
            .output()
            .unwrap_or_else(|error| panic!("failed to run aura {command_name}: {error}"));
        assert!(
            !output.status.success(),
            "{command_name} should reject the source"
        );
        let report: serde_json::Value =
            serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
                panic!(
                    "{command_name} should emit one JSON document: {error}; stderr was {}",
                    String::from_utf8_lossy(&output.stderr)
                )
            });
        assert_eq!(report["schema_version"], 1, "{command_name}");
        assert_eq!(report["diagnostics"].as_array().unwrap().len(), 1);
        let diagnostic = &report["diagnostics"][0];
        assert_eq!(diagnostic["code"], "AU2001", "{command_name}");
        assert_eq!(diagnostic["severity"], "error", "{command_name}");
        assert_eq!(diagnostic["message"], "unknown name `missing`");
        assert!(diagnostic["primary_span"]["path"]
            .as_str()
            .unwrap()
            .ends_with("/main.au"));
        assert_eq!(diagnostic["primary_span"]["start"]["line"], 2);
        assert!(diagnostic["secondary_spans"].is_array());
        assert!(diagnostic["notes"].is_array());
        assert!(diagnostic["help"].is_array());
        assert!(diagnostic["edits"].is_array());
        assert_eq!(diagnostic["call_frames"].as_array().map(Vec::len), Some(0));
        assert_eq!(
            diagnostic["task_ancestry"].as_array().map(Vec::len),
            Some(0)
        );
    }
}

#[cfg(unix)]
struct NativeCacheFixture {
    cache: TempDir,
    _install: TempDir,
    installed_aura: PathBuf,
    _source: TempDir,
    source_path: PathBuf,
    entry: PathBuf,
}

#[cfg(unix)]
impl NativeCacheFixture {
    fn new(prefix: &str) -> Self {
        Self::new_with_program(
            prefix,
            "def main() -> int32:\n    print(\"cached\")\n    return 0\n",
            Some(0),
            "cached\n",
        )
    }

    fn new_with_program(
        prefix: &str,
        program: &str,
        expected_status: Option<i32>,
        expected_stdout: &str,
    ) -> Self {
        let cache = TempDir::new(&format!("{prefix}-cache"));
        let (source, source_path) = write_temp_source(&format!("{prefix}-source"), program);

        let run = |cache_path: &std::path::Path| {
            Command::new(aura_bin())
                .env("AURA_CACHE_DIR", cache_path)
                .arg("run")
                .arg("--backend")
                .arg("direct")
                .arg(&source_path)
                .output()
                .expect("failed to populate the native cache")
        };

        let cold = run(cache.path());
        assert_eq!(
            cold.status.code(),
            expected_status,
            "native-cache cold run had the wrong status, stderr was:\n{}",
            String::from_utf8_lossy(&cold.stderr),
        );
        assert_eq!(String::from_utf8_lossy(&cold.stdout), expected_stdout);

        // Timed cache-member checks must measure cache inspection, not
        // unrelated source-checkout tests contending on the shared Cargo
        // runtime lock. Copy a valid runtime plus its stable link arguments
        // into an installed immutable layout, which needs no workspace-runtime
        // lease. Populate the fixture's program entry through that installed
        // binary so concurrent Cargo activity cannot make the entry key refer
        // to different runtime bytes from the later timed checks.
        let install = TempDir::new(&format!("{prefix}-install"));
        let bin_dir = install.path().join("bin");
        let runtime_dir = install.path().join("lib").join("aura");
        fs::create_dir_all(&bin_dir).expect("installed bin directory should be creatable");
        fs::create_dir_all(&runtime_dir).expect("installed runtime directory should be creatable");
        let installed_aura = bin_dir.join("aura");
        fs::copy(aura_bin(), &installed_aura).expect("aura executable should be installable");
        fs::copy(
            native_runtime_archive(),
            runtime_dir.join("libaura_compiler.a"),
        )
        .expect("native runtime archive should be installable");
        let runtime_memo = fs::read_to_string(cache.path().join("runtime-identity"))
            .expect("cold run should record native link arguments");
        let native_link_args = runtime_memo
            .lines()
            .nth(2)
            .expect("runtime memo should contain native link arguments");
        fs::write(
            runtime_dir.join("native-link-args.json"),
            format!("{native_link_args}\n"),
        )
        .expect("installed native-link manifest should be writable");
        fs::remove_dir_all(cache.path().join("programs"))
            .expect("workspace bootstrap entry should be removable");
        let installed_cold = Command::new(&installed_aura)
            .env("AURA_CACHE_DIR", cache.path())
            .arg("run")
            .arg("--backend")
            .arg("direct")
            .arg(&source_path)
            .output()
            .expect("failed to populate the installed native cache");
        assert_eq!(
            installed_cold.status.code(),
            expected_status,
            "installed native-cache cold run had the wrong status, stderr was:\n{}",
            String::from_utf8_lossy(&installed_cold.stderr),
        );
        assert_eq!(
            String::from_utf8_lossy(&installed_cold.stdout),
            expected_stdout
        );

        let mut entries = fs::read_dir(cache.path().join("programs"))
            .expect("installed program cache should exist")
            .map(|entry| entry.expect("cache entry should be readable").path())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(
            entries.len(),
            1,
            "fixture should publish exactly one installed program entry, found {entries:?}"
        );

        Self {
            cache,
            _install: install,
            installed_aura,
            _source: source,
            source_path,
            entry: entries.remove(0),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.installed_aura);
        command
            .env("AURA_CACHE_DIR", self.cache.path())
            .arg("run")
            .arg("--backend")
            .arg("direct")
            .arg(&self.source_path);
        command
    }

    fn program(&self) -> PathBuf {
        self.entry.join("program")
    }

    fn digest(&self) -> PathBuf {
        self.entry.join("program.sha256")
    }
}

#[cfg(unix)]
fn replace_file_with_fifo(path: &std::path::Path) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    fs::remove_file(path).expect("regular cache member should be removable");
    let path = CString::new(path.as_os_str().as_bytes())
        .expect("temporary cache path should not contain a nul byte");
    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(
        result,
        0,
        "failed to create cache-member FIFO: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(unix)]
#[test]
fn native_cache_creates_every_new_path_component_private_under_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;

    let root = TempDir::new("aura-native-cache-private-components");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))
        .expect("pre-existing cache parent should be private");
    let cache = root.path().join("new-parent").join("new-cache");
    let (_source, source_path) = write_temp_source(
        "aura-native-cache-private-components-run",
        "def main() -> int32:\n    return 0\n",
    );

    let mut command = Command::new(aura_bin());
    command
        .env("AURA_CACHE_DIR", &cache)
        .arg("run")
        .arg("--backend")
        .arg("direct")
        .arg(&source_path);
    // Change the mask in the child immediately before exec so this test does
    // not mutate process-global state while other Rust tests are running.
    unsafe {
        command.pre_exec(|| {
            libc::umask(0o000);
            Ok(())
        });
    }
    let output = command
        .output()
        .expect("failed to run aura with a permissive umask");
    assert!(
        output.status.success(),
        "direct run should succeed with a private cache, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    for path in [root.path().join("new-parent"), cache] {
        assert_eq!(
            fs::metadata(&path)
                .unwrap_or_else(|error| panic!("{} should exist: {error}", path.display()))
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "new cache component `{}` must never be group- or world-accessible",
            path.display()
        );
    }
}

#[test]
fn native_run_cache_verifies_artifacts_rebuilds_invalid_entries_and_keys_on_the_program() {
    let cache = TempDir::new("aura-native-cache");
    let source = "def main() -> int32:\n    print(\"cached\")\n    return 0\n";
    let (_temp, source_path) = write_temp_source("aura-native-cache-run", source);

    let run = |path: &std::path::Path| {
        Command::new(aura_bin())
            .env("AURA_CACHE_DIR", cache.path())
            .arg("run")
            .arg("--backend")
            .arg("direct")
            .arg(path.display().to_string())
            .output()
            .expect("failed to run aura run --backend direct")
    };

    let cold = run(&source_path);
    assert!(
        cold.status.success(),
        "cold run failed, stderr was:\n{}",
        String::from_utf8_lossy(&cold.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&cold.stdout), "cached\n");
    assert!(
        String::from_utf8_lossy(&cold.stderr).contains("aura: building native program..."),
        "a cold direct run must explain the native-program build, stderr was:\n{}",
        String::from_utf8_lossy(&cold.stderr)
    );

    let entries = |label: &str| {
        let mut found = fs::read_dir(cache.path().join("programs"))
            .unwrap_or_else(|error| panic!("{label}: cache directory should exist: {error}"))
            .map(|entry| entry.expect("cache entry should be readable").file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        found.sort();
        found
    };

    let after_cold = entries("cold");
    assert_eq!(
        after_cold.len(),
        1,
        "one cached binary, found {after_cold:?}"
    );
    // A published entry is never a staged temporary.
    assert!(
        !after_cold[0].starts_with('.'),
        "cache published a staged name: {after_cold:?}"
    );
    let cached_entry = cache.path().join("programs").join(&after_cold[0]);
    let cached_binary = cached_entry.join("program");
    let cached_digest = cached_entry.join("program.sha256");
    assert!(
        cached_binary.is_file(),
        "the cache entry must contain the native program"
    );
    assert!(
        cached_digest.is_file(),
        "the cache entry must record the program's own content hash"
    );

    let cached_contents = fs::read(&cached_binary).expect("cached binary should be readable");
    let expected_digest = aura_compiler::sha256_hex(&cached_contents);
    assert_eq!(
        fs::read_to_string(&cached_digest)
            .expect("cached digest should be readable")
            .trim(),
        expected_digest,
        "the stored digest must describe the cached program bytes"
    );
    let verified_binary_modified = fs::metadata(&cached_binary)
        .expect("cached binary metadata should be readable")
        .modified()
        .expect("cached binary modification time should be readable");
    let verified_digest = fs::read(&cached_digest).expect("cached digest should be readable");
    std::thread::sleep(std::time::Duration::from_millis(20));
    // A valid hit must return before compiler/linker selection. Poisoning CC
    // makes this a behavioral proof of reuse: an implementation that silently
    // rebuilds on every invocation fails instead of passing because an
    // existing content-addressed entry masks the attempted republish.
    let missing_cc = cache.path().join("missing-cc");
    let warm = Command::new(aura_bin())
        .env("AURA_CACHE_DIR", cache.path())
        .env("CC", &missing_cc)
        .arg("run")
        .arg("--backend")
        .arg("direct")
        .arg(source_path.display().to_string())
        .output()
        .expect("failed to run a verified native cache hit");
    assert!(
        warm.status.success(),
        "a verified cache hit must not invoke the poisoned compiler, stderr was:\n{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&warm.stdout), "cached\n");
    assert_eq!(
        entries("warm"),
        after_cold,
        "a warm run must reuse the cached binary rather than publish another"
    );
    assert_eq!(
        fs::metadata(&cached_binary)
            .expect("verified cached binary should remain readable")
            .modified()
            .expect("verified cached binary modification time should remain readable"),
        verified_binary_modified,
        "a verified cache hit must launch without rebuilding or rewriting"
    );
    assert_eq!(
        fs::read(&cached_digest).expect("verified digest should remain readable"),
        verified_digest,
        "a verified cache hit must not rewrite its digest"
    );

    // A syntactically valid identity that is bound to another content key is
    // still corrupt metadata. The mismatched entry must be removed, rebuilt,
    // and retained as the next warm hit rather than restored after quarantine.
    let cached_entry_id = cached_entry.join("entry-id");
    let wrong_key = if after_cold[0] == "0".repeat(64) {
        "1".repeat(64)
    } else {
        "0".repeat(64)
    };
    fs::write(
        &cached_entry_id,
        format!("{wrong_key}:{}\n", "2".repeat(64)),
    )
    .expect("cached entry identity should be replaceable");
    let after_entry_id_mismatch = run(&source_path);
    assert!(
        after_entry_id_mismatch.status.success(),
        "entry-id mismatch should rebuild, stderr was:\n{}",
        String::from_utf8_lossy(&after_entry_id_mismatch.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&after_entry_id_mismatch.stdout),
        "cached\n"
    );
    assert!(
        fs::read_to_string(&cached_entry_id)
            .expect("entry-id mismatch rebuild should publish an identity")
            .starts_with(&format!("{}:", after_cold[0])),
        "rebuilt entry identity must be bound to its content key"
    );
    let retained_identity = Command::new(aura_bin())
        .env("AURA_CACHE_DIR", cache.path())
        .env("CC", &missing_cc)
        .arg("run")
        .arg("--backend")
        .arg("direct")
        .arg(source_path.display().to_string())
        .output()
        .expect("failed to run after rebuilding mismatched entry identity");
    assert!(
        retained_identity.status.success(),
        "the rebuilt entry must be retained as a warm hit, stderr was:\n{}",
        String::from_utf8_lossy(&retained_identity.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&retained_identity.stdout),
        "cached\n"
    );

    // A native-shaped artifact with the wrong recorded digest must rebuild.
    // This separately proves that hit verification consults the sidecar rather
    // than accepting the executable header alone.
    fs::write(&cached_digest, format!("{}\n", "0".repeat(64)))
        .expect("cached digest should be replaceable");
    let after_digest_mismatch = run(&source_path);
    assert!(
        after_digest_mismatch.status.success(),
        "digest mismatch should rebuild, stderr was:\n{}",
        String::from_utf8_lossy(&after_digest_mismatch.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&after_digest_mismatch.stdout),
        "cached\n"
    );
    let rebuilt_contents =
        fs::read(&cached_binary).expect("digest-mismatch rebuild should publish a binary");
    assert_eq!(
        fs::read_to_string(&cached_digest)
            .expect("digest-mismatch rebuild should publish a digest")
            .trim(),
        aura_compiler::sha256_hex(&rebuilt_contents),
        "a digest mismatch must be replaced by a self-verifying entry"
    );

    // A truncated artifact must fail content verification and rebuild. In
    // particular, it must never reach the macOS ENOEXEC shell fallback.
    fs::write(&cached_binary, []).expect("cached binary should be truncatable");
    let after_truncate = run(&source_path);
    assert!(
        after_truncate.status.success(),
        "truncated cache entry should rebuild, stderr was:\n{}",
        String::from_utf8_lossy(&after_truncate.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&after_truncate.stdout), "cached\n");
    assert!(
        fs::metadata(&cached_binary)
            .expect("rebuilt cached binary should exist")
            .len()
            > 0,
        "the truncated artifact must be replaced"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&cached_binary)
            .expect("cached binary permissions should be readable")
            .permissions();
        permissions.set_mode(permissions.mode() & !0o111);
        fs::set_permissions(&cached_binary, permissions)
            .expect("cached execute permissions should be removable");
        let after_unlaunchable = run(&source_path);
        assert!(
            after_unlaunchable.status.success(),
            "unlaunchable verified entry should rebuild, stderr was:\n{}",
            String::from_utf8_lossy(&after_unlaunchable.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&after_unlaunchable.stdout),
            "cached\n"
        );
        assert_ne!(
            fs::metadata(&cached_binary)
                .expect("unlaunchable entry should be replaced")
                .permissions()
                .mode()
                & 0o111,
            0,
            "the rebuilt cache entry must be executable"
        );
    }

    // A digest-matching file with a plausible native magic can still be a
    // malformed executable. It must reach a no-shell-fallback launch probe,
    // fail as cache state, and rebuild rather than becoming a program result.
    let wrong_bytes: &[u8] = if cfg!(target_os = "macos") {
        b"\xcf\xfa\xed\xfeexit 0\n"
    } else if cfg!(target_os = "linux") {
        b"\x7fELFexit 0\n"
    } else {
        b"native-format-invalid\n"
    };
    fs::write(&cached_binary, wrong_bytes).expect("cached binary should be replaceable");
    fs::write(
        &cached_digest,
        format!("{}\n", aura_compiler::sha256_hex(wrong_bytes)),
    )
    .expect("matching wrong-bytes digest should be writable");
    let after_wrong_bytes = run(&source_path);
    assert!(
        after_wrong_bytes.status.success(),
        "malformed native-shaped cache entry should rebuild, stderr was:\n{}",
        String::from_utf8_lossy(&after_wrong_bytes.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&after_wrong_bytes.stdout),
        "cached\n",
        "digest-matching malformed native bytes must not become a program result"
    );
    assert!(
        fs::metadata(&cached_binary)
            .expect("wrong-shape artifact should be replaced")
            .len()
            > wrong_bytes.len() as u64,
        "the wrong-shape artifact must be replaced by a native binary"
    );

    // Changing the program changes the content key, so the cache gains a
    // second entry rather than launching the stale binary.
    let (_changed_temp, changed_path) = write_temp_source(
        "aura-native-cache-changed",
        "def main() -> int32:\n    print(\"changed\")\n    return 0\n",
    );
    let changed = run(&changed_path);
    assert!(changed.status.success());
    assert_eq!(String::from_utf8_lossy(&changed.stdout), "changed\n");
    let changed_stderr = String::from_utf8_lossy(&changed.stderr);
    assert!(
        changed_stderr.contains("aura: building native program..."),
        "a cold per-program cache miss must describe the program artifact build: {changed_stderr}"
    );
    assert!(
        !changed_stderr.contains("rebuilding native runtime"),
        "a cold per-program cache miss must not claim the shared runtime is being rebuilt: {changed_stderr}"
    );
    let after_change = entries("changed");
    assert_eq!(
        after_change.len(),
        2,
        "a changed program should key to a new entry, found {after_change:?}"
    );

    // The runtime archive identity is memoized beside the programs, not among
    // them, so its bookkeeping can never be mistaken for a content key.
    assert!(
        cache.path().join("runtime-identity").is_file(),
        "the runtime identity memo should be recorded at the cache root"
    );
}

#[cfg(unix)]
#[test]
fn native_run_cache_serializes_concurrent_cold_runs_into_one_build_and_verified_hits() {
    use std::os::fd::AsRawFd;
    use std::sync::mpsc;

    let cache = TempDir::new("aura-native-cache-concurrent");
    let (_source, source_path) = write_temp_source(
        "aura-native-cache-concurrent-run",
        "def main() -> int32:\n    print(\"concurrent\")\n    return 0\n",
    );

    // Bootstrap the exact content key and its lock path, then remove only the
    // program entry. Holding that key gives every child a deterministic cold
    // miss and a real establishment barrier.
    let bootstrap = Command::new(aura_bin())
        .env("AURA_CACHE_DIR", cache.path())
        .arg("run")
        .arg("--backend")
        .arg("direct")
        .arg(&source_path)
        .output()
        .expect("failed to bootstrap concurrent native cache key");
    assert!(
        bootstrap.status.success(),
        "concurrent-key bootstrap failed, stderr was:\n{}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );
    let mut bootstrapped_entries = fs::read_dir(cache.path().join("programs"))
        .expect("bootstrapped program cache should exist")
        .map(|entry| entry.expect("bootstrap entry should be readable").path())
        .collect::<Vec<_>>();
    assert_eq!(bootstrapped_entries.len(), 1);
    let bootstrapped_entry = bootstrapped_entries.remove(0);
    let key = bootstrapped_entry
        .file_name()
        .expect("bootstrap entry should have a key")
        .to_string_lossy()
        .into_owned();
    fs::remove_dir_all(&bootstrapped_entry)
        .expect("bootstrap program entry should be removable for the cold barrier");
    let lock_path = cache.path().join("locks").join(format!("{key}.lock"));
    let held_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("the exact bootstrapped key lock should exist");
    assert_eq!(
        unsafe { libc::flock(held_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "the parent should hold the exact cache-key barrier"
    );

    let mut children = (0..4)
        .map(|_| {
            Command::new(aura_bin())
                .env("AURA_CACHE_DIR", cache.path())
                .arg("run")
                .arg("--backend")
                .arg("direct")
                .arg(&source_path)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("failed to spawn concurrent direct run")
        })
        .collect::<Vec<_>>();

    // Read each child's first stderr line concurrently. It must be flushed
    // while this process still owns the key lock, before the child can build.
    let mut first_line_receivers = Vec::new();
    let mut stderr_readers = Vec::new();
    for child in &mut children {
        let stderr = child
            .stderr
            .take()
            .expect("concurrent stderr should be captured");
        let (sender, receiver) = mpsc::channel();
        first_line_receivers.push(receiver);
        stderr_readers.push(std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut first_line = String::new();
            let result = reader.read_line(&mut first_line);
            let _ = sender.send((result, first_line));
            let mut rest = Vec::new();
            let _ = reader.read_to_end(&mut rest);
            rest
        }));
    }
    let mut first_lines = Vec::new();
    let mut barrier_error = None;
    for (index, receiver) in first_line_receivers.into_iter().enumerate() {
        match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok((Ok(_), line)) if line == "aura: waiting for a concurrent build...\n" => {
                first_lines.push(line)
            }
            Ok((Ok(_), line)) => {
                barrier_error = Some(format!(
                    "concurrent direct run {index} reported the wrong pre-block line: {line:?}"
                ));
                break;
            }
            Ok((Err(error), _)) => {
                barrier_error = Some(format!(
                    "concurrent direct run {index} stderr read failed: {error}"
                ));
                break;
            }
            Err(error) => {
                barrier_error = Some(format!(
                    "concurrent direct run {index} did not flush its wait line before blocking: {error}"
                ));
                break;
            }
        }
    }
    drop(held_lock);
    if let Some(error) = barrier_error {
        for child in &mut children {
            let _ = child.kill();
            let _ = child.wait();
        }
        panic!("{error}");
    }

    let mut outputs = Vec::new();
    for (index, (child, stderr_reader)) in children.iter_mut().zip(stderr_readers).enumerate() {
        let status =
            wait_with_timeout(child, std::time::Duration::from_secs(120)).unwrap_or_else(|| {
                let _ = child.kill();
                let _ = child.wait();
                panic!("concurrent direct run {index} did not finish within 120 seconds")
            });
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        child
            .stdout
            .take()
            .expect("concurrent stdout should be captured")
            .read_to_end(&mut stdout)
            .expect("concurrent stdout should be readable");
        stderr.extend_from_slice(first_lines[index].as_bytes());
        stderr.extend(
            stderr_reader
                .join()
                .expect("concurrent stderr reader should finish"),
        );
        outputs.push(std::process::Output {
            status,
            stdout,
            stderr,
        });
    }

    for (index, output) in outputs.iter().enumerate() {
        assert!(
            output.status.success(),
            "concurrent direct run {index} failed; stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "concurrent\n",
            "concurrent direct run {index} produced the wrong result"
        );
    }

    let rebuilds = outputs
        .iter()
        .filter(|output| {
            String::from_utf8_lossy(&output.stderr).contains("aura: building native program...")
        })
        .count();
    assert_eq!(
        rebuilds,
        1,
        "four concurrent cold runs must perform exactly one build; stderr was:\n{}",
        outputs
            .iter()
            .map(|output| String::from_utf8_lossy(&output.stderr))
            .collect::<Vec<_>>()
            .join("\n---\n")
    );
    for output in &outputs {
        assert_eq!(
            String::from_utf8_lossy(&output.stderr)
                .matches("aura: waiting for a concurrent build...")
                .count(),
            1,
            "each run must deduplicate its wait notice"
        );
    }

    let entries = fs::read_dir(cache.path().join("programs"))
        .expect("concurrent program cache should exist")
        .map(|entry| {
            entry
                .expect("concurrent cache entry should be readable")
                .path()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        entries.len(),
        1,
        "concurrent runs must publish exactly one verified cache entry, found {entries:?}"
    );

    let warm = Command::new(aura_bin())
        .env("AURA_CACHE_DIR", cache.path())
        .env("CC", cache.path().join("missing-cc"))
        .env("CARGO", cache.path().join("missing-cargo"))
        .arg("run")
        .arg("--backend")
        .arg("direct")
        .arg(&source_path)
        .output()
        .expect("failed to run poisoned-toolchain verified hit");
    assert!(
        warm.status.success(),
        "the established entry must be a verified hit with CC and CARGO unavailable, stderr was:\n{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&warm.stdout), "concurrent\n");
    assert!(
        !String::from_utf8_lossy(&warm.stderr).contains("building native program"),
        "a poisoned-toolchain warm hit must not rebuild"
    );
}

#[cfg(unix)]
#[test]
fn native_run_cache_unrelated_warm_hit_does_not_wait_for_another_key() {
    use std::os::fd::AsRawFd;

    let fixture = NativeCacheFixture::new("aura-native-cache-per-key");
    let cache = &fixture.cache;
    let first_path = &fixture.source_path;
    let (_second_source, second_path) = write_temp_source(
        "aura-native-cache-per-key-second",
        "def main() -> int32:\n    print(\"second\")\n    return 0\n",
    );
    let run = |path: &std::path::Path| {
        Command::new(&fixture.installed_aura)
            .env("AURA_CACHE_DIR", cache.path())
            .arg("run")
            .arg("--backend")
            .arg("direct")
            .arg(path)
            .output()
            .expect("failed to populate per-key native cache")
    };
    let first_key = fixture
        .entry
        .file_name()
        .expect("first program should publish a cache entry")
        .to_string_lossy()
        .into_owned();

    let second = run(&second_path);
    assert!(
        second.status.success(),
        "second per-key cold run failed, stderr was:\n{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&second.stdout), "second\n");
    let mut keys = fs::read_dir(cache.path().join("programs"))
        .expect("per-key program cache should exist")
        .map(|entry| {
            entry
                .expect("per-key cache entry should be readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys.len(), 2, "two programs should produce two cache keys");

    let warm = |label: &str, path: &std::path::Path| {
        let mut warm_command = Command::new(&fixture.installed_aura);
        warm_command
            .env("AURA_CACHE_DIR", cache.path())
            .env("CC", cache.path().join("missing-cc"))
            .env("CARGO", cache.path().join("missing-cargo"))
            .arg("run")
            .arg("--backend")
            .arg("direct")
            .arg(path);
        // The held lock makes any wrong-key wait permanent. Allow compile/link
        // contention elsewhere in the default-parallel suite without weakening
        // that bounded non-waiting assertion.
        command_output_with_timeout(warm_command, std::time::Duration::from_secs(30), label)
    };

    // Installed runtime inputs are immutable and therefore require no
    // target-global runtime lease. Holding one exact program-key writer now
    // isolates the property under test: verified hits for both that same key
    // and an unrelated key must return through the optimistic read path.
    let lock_path = cache.path().join("locks").join(format!("{first_key}.lock"));
    let held_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("the first cache-key lock should exist");
    assert_eq!(
        unsafe { libc::flock(held_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "the test should hold one otherwise-idle cache-key lock"
    );

    for (label, path, expected) in [
        ("same-key warm hit", first_path.as_path(), "cached\n"),
        ("unrelated warm hit", &second_path, "second\n"),
    ] {
        let warm = warm(label, path);
        assert!(
            warm.status.success(),
            "{label} must not wait or rebuild, stderr was:\n{}",
            String::from_utf8_lossy(&warm.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&warm.stdout), expected);
        let stderr = String::from_utf8_lossy(&warm.stderr);
        assert!(
            !stderr.contains("aura: waiting for a concurrent build..."),
            "{label} must not wait on the held cache-key writer: {stderr}"
        );
        assert!(
            !stderr.contains("aura: building native program..."),
            "{label} must not rebuild through the poisoned toolchain: {stderr}"
        );
    }
    drop(held_lock);
}

#[test]
fn direct_run_json_failure_remains_one_document_when_a_rebuild_is_needed() {
    let cache = TempDir::new("aura-native-json-rebuild");
    let (_source, source_path) = write_temp_source(
        "aura-native-json-rebuild-source",
        "def main() -> int32:\n    return 0\n",
    );
    let output = Command::new(aura_bin())
        .env("AURA_CACHE_DIR", cache.path())
        .env("CC", cache.path().join("missing-cc"))
        .arg("run")
        .arg("--format")
        .arg("json")
        .arg("--backend")
        .arg("direct")
        .arg(&source_path)
        .output()
        .expect("failed to run JSON-mode direct rebuild failure");
    assert!(
        !output.status.success(),
        "a missing linker must fail the forced direct backend"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
            panic!(
                "JSON-mode stderr must remain exactly one document: {error}; stderr was:\n{}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["diagnostics"].as_array().map(Vec::len), Some(1));
    assert!(
        report["diagnostics"][0]["notes"]
            .as_array()
            .is_some_and(|notes| notes
                .iter()
                .any(|note| note == "aura: building native program...")),
        "JSON mode must preserve the exact rebuild notice as structured progress: {report}"
    );
}

fn parse_single_json_stderr(output: &std::process::Output, context: &str) -> serde_json::Value {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.lines().count(),
        1,
        "{context} must emit exactly one JSON document, stderr was:\n{stderr}"
    );
    serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
        panic!("{context} must emit valid JSON: {error}; stderr was:\n{stderr}")
    })
}

#[cfg(unix)]
#[test]
fn direct_run_json_transports_runtime_traps_on_cold_warm_and_auto_paths() {
    let cache = TempDir::new("aura-native-json-runtime-trap");
    let (_source, source_path) = write_temp_source(
        "aura-native-json-runtime-trap-source",
        "def explode() -> int32:\n    values: list[int32] = [1, 2]\n    return values[9]\n\ndef main() -> int32:\n    return explode()\n",
    );

    let run = |backend: &str| {
        Command::new(aura_bin())
            .env("AURA_CACHE_DIR", cache.path())
            .args(["run", "--format", "json", "--backend", backend])
            .arg(&source_path)
            .output()
            .unwrap_or_else(|error| panic!("failed to run {backend} JSON trap: {error}"))
    };

    let cold = run("direct");
    assert_eq!(
        cold.status.code(),
        Some(1),
        "cold direct runtime trap should exit 1"
    );
    let cold_report = parse_single_json_stderr(&cold, "cold direct runtime trap");
    assert_eq!(cold_report["schema_version"], 1);
    assert!(
        cold_report["diagnostics"]
            .as_array()
            .is_some_and(|items| items.len() == 1),
        "cold direct trap must carry one diagnostic: {cold_report}"
    );
    assert_eq!(cold_report["diagnostics"][0]["code"], "AU4003");
    assert!(
        cold_report["diagnostics"][0]["call_frames"]
            .as_array()
            .is_some_and(|frames| !frames.is_empty()),
        "cold direct trap must carry typed call frames: {cold_report}"
    );
    assert!(
        cold_report["diagnostics"][0]["notes"]
            .as_array()
            .is_some_and(|notes| notes
                .iter()
                .any(|note| note == "aura: building native program...")),
        "cold direct trap must retain rebuild progress in non-frame notes: {cold_report}"
    );

    let warm = run("direct");
    assert_eq!(
        warm.status.code(),
        Some(1),
        "warm direct runtime trap should exit 1"
    );
    let warm_report = parse_single_json_stderr(&warm, "warm direct runtime trap");
    assert_eq!(
        warm_report["diagnostics"][0]["call_frames"], cold_report["diagnostics"][0]["call_frames"],
        "cold and verified-hit launches must transport identical frames"
    );
    assert!(
        warm_report.get("fallback").is_none(),
        "forced direct runtime traps cannot carry fallback metadata: {warm_report}"
    );

    let automatic = run("auto");
    assert_eq!(
        automatic.status.code(),
        Some(1),
        "automatic direct runtime trap should exit 1"
    );
    let automatic_report = parse_single_json_stderr(&automatic, "automatic direct runtime trap");
    assert_eq!(
        automatic_report["diagnostics"][0]["call_frames"],
        warm_report["diagnostics"][0]["call_frames"]
    );
    assert!(
        automatic_report.get("fallback").is_none(),
        "an Aura program trap is not a backend failure and must not fall back: {automatic_report}"
    );
}

#[cfg(unix)]
#[test]
fn assertion_introspection_json_matches_mir_and_direct_backends() {
    let cache = TempDir::new("aura-assertion-json-parity");
    let (_source, source_path) = write_temp_source(
        "aura-assertion-json-parity-source",
        "def main():\n    item = 4\n    values = [1, 2, 3]\n    assert item in values\n",
    );

    let run = |backend: Option<&str>| {
        let mut command = Command::new(aura_bin());
        command
            .env("AURA_CACHE_DIR", cache.path())
            .args(["run", "--format", "json"]);
        if let Some(backend) = backend {
            command.args(["--backend", backend]);
        }
        command
            .arg(&source_path)
            .output()
            .unwrap_or_else(|error| panic!("failed to run assertion JSON backend: {error}"))
    };

    let mir = run(None);
    let direct = run(Some("direct"));
    assert_eq!(mir.status.code(), Some(1));
    assert_eq!(direct.status.code(), Some(1));
    let mir_report = parse_single_json_stderr(&mir, "MIR assertion JSON");
    let direct_report = parse_single_json_stderr(&direct, "direct assertion JSON");
    let expected = serde_json::json!([
        {"label": "item", "type": "int64", "value": "4", "truncated": false},
        {
            "label": "collection",
            "type": "list[int64]",
            "value": "[1, 2, 3]",
            "truncated": false
        }
    ]);
    assert_eq!(mir_report["diagnostics"][0]["assertion_operands"], expected);
    assert_eq!(
        direct_report["diagnostics"][0]["assertion_operands"],
        mir_report["diagnostics"][0]["assertion_operands"]
    );
    assert_eq!(
        direct_report["diagnostics"][0]["primary_span"],
        mir_report["diagnostics"][0]["primary_span"]
    );
}

#[cfg(unix)]
#[test]
fn direct_run_json_distinguishes_normal_nonzero_status_from_a_runtime_trap() {
    let cache = TempDir::new("aura-native-json-normal-nonzero");
    let (_source, source_path) = write_temp_source(
        "aura-native-json-normal-nonzero-source",
        "def main() -> int32:\n    return 1\n",
    );
    let run = || {
        Command::new(aura_bin())
            .env("AURA_CACHE_DIR", cache.path())
            .args(["run", "--format", "json", "--backend", "direct"])
            .arg(&source_path)
            .output()
            .expect("failed to run normal nonzero direct program")
    };

    let cold = run();
    assert_eq!(cold.status.code(), Some(1));
    let cold_report = parse_single_json_stderr(&cold, "cold normal nonzero direct run");
    assert!(
        cold_report.get("diagnostics").is_none(),
        "ordinary status 1 must not be classified as a diagnostic: {cold_report}"
    );
    assert!(
        cold_report["progress"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "the cold run should retain its native-build progress: {cold_report}"
    );

    let warm = run();
    assert_eq!(warm.status.code(), Some(1));
    assert!(
        warm.stderr.is_empty(),
        "a warm normal status 1 needs neither a diagnostic nor a progress document: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
}

#[cfg(unix)]
#[test]
fn direct_json_channel_is_private_and_does_not_wait_for_a_grandchild() {
    let cache = TempDir::new("aura-native-json-grandchild");
    let source = r#"import process
import sys

def internal_env_visible() -> bool:
    match sys.env("AURA_INTERNAL_DIAGNOSTIC_FD"):
        case Option.Some(_):
            return true
        case Option.None:
            pass
    match sys.env("AURA_INTERNAL_DIAGNOSTIC_SIGNAL_FD"):
        case Option.Some(_):
            return true
        case Option.None:
            return false

def run() -> Result[int32, process.Error]:
    if internal_env_visible():
        return Result.Ok(11)
    environment = try process.run(["/usr/bin/env"], stdout=process.pipe(), stderr=process.pipe(), timeout=2s)
    child_environment = environment.stdout()
    if child_environment.contains("AURA_INTERNAL_DIAGNOSTIC_FD") or child_environment.contains("AURA_INTERNAL_DIAGNOSTIC_SIGNAL_FD"):
        return Result.Ok(12)
    try process.run(["/bin/sh", "-c", "sleep 30 &"], stdout=process.null(), stderr=process.null(), timeout=2s)
    values: list[int32] = [1, 2]
    return Result.Ok(values[9])

def main() -> int32:
    match own run():
        case Result.Ok(code):
            return code
        case Result.Err(_):
            return 13
"#;
    let (_source, source_path) = write_temp_source("aura-native-json-grandchild-source", source);

    // Populate the program cache without installing the private JSON channel.
    let cold = Command::new(aura_bin())
        .env("AURA_CACHE_DIR", cache.path())
        .args(["run", "--backend", "direct"])
        .arg(&source_path)
        .output()
        .expect("failed to populate grandchild regression cache");
    assert_eq!(
        cold.status.code(),
        Some(1),
        "the human-mode population run should reach the body trap: {}",
        String::from_utf8_lossy(&cold.stderr)
    );

    let mut command = Command::new(aura_bin());
    command
        .env("AURA_CACHE_DIR", cache.path())
        .args(["run", "--format", "json", "--backend", "direct"])
        .arg(&source_path);
    // Keep the harness deadline well below the inherited-grandchild lifetime,
    // while leaving enough headroom for a heavily loaded local or hosted VM.
    let output = command_output_with_timeout(
        command,
        std::time::Duration::from_secs(15),
        "direct JSON grandchild fd-isolation run",
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "the JSON run must reach the trap, not an environment-leak status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_single_json_stderr(&output, "direct JSON grandchild fd-isolation run");
    assert_eq!(report["diagnostics"][0]["code"], "AU4003");
}

#[cfg(unix)]
#[test]
fn forced_direct_json_preserves_the_original_lowering_diagnostic() {
    let (_source, source_path) = write_temp_source(
        "aura-native-json-lowering-diagnostic",
        "def main() -> int32:\n    return missing\n",
    );
    let output = Command::new(aura_bin())
        .args(["run", "--format", "json", "--backend", "direct"])
        .arg(&source_path)
        .output()
        .expect("failed to run forced-direct lowering failure");
    assert_eq!(output.status.code(), Some(1));
    let report = parse_single_json_stderr(&output, "forced-direct lowering failure");
    let diagnostic = &report["diagnostics"][0];
    assert_eq!(diagnostic["code"], "AU2001");
    assert_eq!(diagnostic["message"], "unknown name `missing`");
    assert_eq!(diagnostic["primary_span"]["start"]["line"], 2);
    assert!(
        diagnostic["primary_span"]["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("/main.au")),
        "the original lowering path must survive native backend selection: {report}"
    );
}

#[cfg(unix)]
#[test]
fn direct_run_json_buffers_wait_progress_into_one_document() {
    use std::os::fd::AsRawFd;

    let fixture = NativeCacheFixture::new_with_program(
        "aura-native-json-wait",
        "def explode() -> int32:\n    values: list[int32] = [1, 2]\n    return values[9]\n\ndef main() -> int32:\n    return explode()\n",
        Some(1),
        "",
    );
    let key = fixture
        .entry
        .file_name()
        .expect("the populated entry should have a content key")
        .to_string_lossy()
        .into_owned();
    fs::remove_dir_all(&fixture.entry)
        .expect("the populated entry should be removable to force a cache miss");

    // Hold the exact content-key lock, not a neighboring or synthetic lock.
    // JSON progress is intentionally buffered to preserve the one-document
    // stderr contract, so the blocked child must not emit a partial document.
    let lock_path = fixture
        .cache
        .path()
        .join("locks")
        .join(format!("{key}.lock"));
    let held_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("the populated entry's exact content-key lock should exist");
    assert_eq!(
        unsafe { libc::flock(held_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "the test should hold the exact content-key barrier"
    );

    let mut child = Command::new(&fixture.installed_aura)
        .env("AURA_CACHE_DIR", fixture.cache.path())
        .arg("run")
        .arg("--format")
        .arg("json")
        .arg("--backend")
        .arg("direct")
        .arg(&fixture.source_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn JSON-mode direct run behind the cache-key barrier");

    // The bounded poll is the only observable pre-release assertion available
    // for deliberately buffered JSON output. The final exact wait message
    // below proves that the child reached this held lock during the window.
    if let Some(status) = wait_with_timeout(&mut child, std::time::Duration::from_secs(3)) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        child
            .stdout
            .take()
            .expect("JSON wait stdout should be captured")
            .read_to_end(&mut stdout)
            .expect("JSON wait stdout should be readable");
        child
            .stderr
            .take()
            .expect("JSON wait stderr should be captured")
            .read_to_end(&mut stderr)
            .expect("JSON wait stderr should be readable");
        panic!(
            "JSON-mode direct run completed before the held content-key lock was released \
             (status {status}); stdout was:\n{}stderr was:\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    }

    drop(held_lock);
    let status =
        wait_with_timeout(&mut child, std::time::Duration::from_secs(60)).unwrap_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("JSON-mode direct run did not finish after releasing the content-key lock")
        });
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .stdout
        .take()
        .expect("JSON wait stdout should be captured")
        .read_to_end(&mut stdout)
        .expect("JSON wait stdout should be readable");
    child
        .stderr
        .take()
        .expect("JSON wait stderr should be captured")
        .read_to_end(&mut stderr)
        .expect("JSON wait stderr should be readable");

    assert_eq!(
        status.code(),
        Some(1),
        "JSON-mode direct trap should be reported after the lock release; stderr was:\n{}",
        String::from_utf8_lossy(&stderr),
    );
    assert!(stdout.is_empty(), "the trapping program should not print");
    let stderr_text = String::from_utf8(stderr).expect("JSON-mode stderr should be UTF-8");
    assert_eq!(
        stderr_text.lines().count(),
        1,
        "JSON-mode stderr must contain exactly one JSON document: {stderr_text:?}"
    );
    let report: serde_json::Value = serde_json::from_str(&stderr_text).unwrap_or_else(|error| {
        panic!(
            "JSON-mode wait stderr must be exactly one JSON document: {error}; stderr was:\n\
                 {stderr_text}"
        )
    });
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["diagnostics"][0]["code"], "AU4003");
    assert!(
        report["diagnostics"][0]["call_frames"]
            .as_array()
            .is_some_and(|frames| !frames.is_empty()),
        "the waiting launch must preserve the native trap frames: {report}"
    );
    let progress = report["diagnostics"][0]["notes"]
        .as_array()
        .unwrap_or_else(|| panic!("JSON trap should contain buffered progress notes: {report}"));
    assert_eq!(
        progress
            .iter()
            .filter(|message| *message == "aura: waiting for a concurrent build...")
            .count(),
        1,
        "the buffered report must preserve exactly one exact wait notice: {report}"
    );
}

#[cfg(unix)]
#[test]
fn auto_run_json_fallback_preserves_native_progress_in_one_document() {
    let fixture = NativeCacheFixture::new("aura-native-json-auto-fallback");
    fs::remove_dir_all(&fixture.entry)
        .expect("the warm entry should be removable to force a direct build");
    let output = Command::new(&fixture.installed_aura)
        .env("AURA_CACHE_DIR", fixture.cache.path())
        .env("CC", fixture.cache.path().join("missing-cc"))
        .arg("run")
        .arg("--format")
        .arg("json")
        .arg("--backend")
        .arg("auto")
        .arg(&fixture.source_path)
        .output()
        .expect("failed to run JSON-mode automatic backend fallback");
    assert!(
        output.status.success(),
        "the MIR fallback should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "cached\n");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
            panic!(
                "JSON-mode fallback stderr must remain exactly one document: {error}; stderr was:\n{}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
    assert_eq!(report["schema_version"], 1);
    assert!(
        report["progress"]
            .as_array()
            .is_some_and(|progress| progress
                .iter()
                .any(|message| message == "aura: building native program...")),
        "the automatic fallback must retain the exact direct rebuild notice: {report}"
    );
    assert_eq!(report["fallback"]["from"], "direct");
    assert_eq!(report["fallback"]["to"], "mir");
    assert!(
        report["fallback"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("failed to run native linker")),
        "the structured fallback must retain the direct failure reason: {report}"
    );
}

#[cfg(unix)]
#[test]
fn installed_direct_run_keeps_native_cache_optional_for_build_locking() {
    let bootstrap_cache = TempDir::new("aura-installed-no-cache-bootstrap");
    let (_source, source_path) = write_temp_source(
        "aura-installed-no-cache-source",
        "def main() -> int32:\n    print(\"uncached\")\n    return 0\n",
    );
    let bootstrap = Command::new(aura_bin())
        .env("AURA_CACHE_DIR", bootstrap_cache.path())
        .arg("run")
        .arg("--backend")
        .arg("direct")
        .arg(&source_path)
        .output()
        .expect("failed to establish installable runtime artifacts");
    assert!(
        bootstrap.status.success(),
        "runtime bootstrap failed, stderr was:\n{}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );

    let prefix = TempDir::new("aura-installed-no-cache-prefix");
    let bin_dir = prefix.path().join("bin");
    let runtime_dir = prefix.path().join("lib").join("aura");
    fs::create_dir_all(&bin_dir).expect("installed bin directory should be creatable");
    fs::create_dir_all(&runtime_dir).expect("installed runtime directory should be creatable");
    let installed_aura = bin_dir.join("aura");
    fs::copy(aura_bin(), &installed_aura).expect("aura executable should be installable");
    fs::copy(
        native_runtime_archive(),
        runtime_dir.join("libaura_compiler.a"),
    )
    .expect("native runtime archive should be installable");
    let runtime_memo = fs::read_to_string(bootstrap_cache.path().join("runtime-identity"))
        .expect("bootstrap should record native link arguments");
    let native_link_args = runtime_memo
        .lines()
        .nth(2)
        .expect("runtime memo should contain native link arguments");
    fs::write(
        runtime_dir.join("native-link-args.json"),
        format!("{native_link_args}\n"),
    )
    .expect("installed native-link manifest should be writable");

    let output = Command::new(&installed_aura)
        .env("AURA_CACHE_DIR", "")
        .env_remove("HOME")
        .arg("run")
        .arg("--backend")
        .arg("direct")
        .arg(&source_path)
        .output()
        .expect("failed to run installed aura without a native cache");
    assert!(
        output.status.success(),
        "installed direct execution must not require a cache merely to lock, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "uncached\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("aura: building native program..."),
        "an uncached installed build should still report its long operation"
    );
}

#[cfg(unix)]
#[test]
fn native_run_cache_rejects_symlink_and_fifo_members_without_blocking_or_leaking() {
    use std::os::unix::fs::symlink;

    let fixture = NativeCacheFixture::new("aura-native-cache-non-regular");
    // Each invalid member triggers a real native rebuild. The no-blocking
    // assertion must tolerate compiler/linker contention from the surrounding
    // default-parallel CLI suite while still bounding any accidental FIFO open.
    let timeout = std::time::Duration::from_secs(30);
    let missing_cc = fixture.cache.path().join("missing-cc");
    let launch_temp = fixture.cache.path().join("launch-temp");
    fs::create_dir(&launch_temp).expect("controlled launch temp should be creatable");

    // A verified warm hit is staged privately, and every launch artifact must
    // be removed again after the child exits.
    let mut warm_command = fixture.command();
    warm_command
        .env("CC", &missing_cc)
        .env("TMPDIR", &launch_temp);
    let warm = command_output_with_timeout(warm_command, timeout, "verified native cache hit");
    assert!(
        warm.status.success(),
        "verified hit failed with {:?}; stdout was:\n{}\nstderr was:\n{}",
        warm.status,
        String::from_utf8_lossy(&warm.stdout),
        String::from_utf8_lossy(&warm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&warm.stdout), "cached\n");
    let leaked_launch_artifacts = fs::read_dir(&launch_temp)
        .expect("controlled launch temp should remain readable")
        .map(|entry| {
            entry
                .expect("launch-temp entry should be readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with("aura-verified-native-"))
        .collect::<Vec<_>>();
    assert!(
        leaked_launch_artifacts.is_empty(),
        "successful verified hit leaked private launch artifacts: {leaked_launch_artifacts:?}"
    );

    // A symlinked program must not be followed even when its target is a real
    // native executable and the sidecar matches that target.
    let external_program = fixture.cache.path().join("external-program");
    fs::copy("/bin/echo", &external_program).expect("external native executable should copy");
    let external_contents =
        fs::read(&external_program).expect("external native executable should be readable");
    fs::remove_file(fixture.program()).expect("cached program should be removable");
    symlink(&external_program, fixture.program()).expect("program symlink should be creatable");
    fs::write(
        fixture.digest(),
        format!("{}\n", aura_compiler::sha256_hex(&external_contents)),
    )
    .expect("program-symlink digest should be writable");
    let program_symlink = command_output_with_timeout(
        fixture.command(),
        timeout,
        "native cache entry with a program symlink",
    );
    assert!(
        program_symlink.status.success(),
        "program symlink should cause a rebuild, stderr was:\n{}",
        String::from_utf8_lossy(&program_symlink.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&program_symlink.stdout),
        "cached\n",
        "the symlink target must never become the program result"
    );
    assert!(
        fs::symlink_metadata(fixture.program())
            .expect("rebuilt program should exist")
            .file_type()
            .is_file(),
        "the program symlink should be replaced by a regular cached binary"
    );
    assert!(
        external_program.is_file(),
        "rejecting a symlink must not remove its external target"
    );

    // A symlinked digest is invalid cache structure even when it names the
    // correct digest. Rejecting it pins no-follow behavior for both members.
    let external_digest = fixture.cache.path().join("external-digest");
    let current_program =
        fs::read(fixture.program()).expect("rebuilt cached program should be readable");
    fs::write(
        &external_digest,
        format!("{}\n", aura_compiler::sha256_hex(&current_program)),
    )
    .expect("external digest should be writable");
    fs::remove_file(fixture.digest()).expect("cached digest should be removable");
    symlink(&external_digest, fixture.digest()).expect("digest symlink should be creatable");
    let digest_symlink = command_output_with_timeout(
        fixture.command(),
        timeout,
        "native cache entry with a digest symlink",
    );
    assert!(
        digest_symlink.status.success(),
        "digest symlink should cause a rebuild, stderr was:\n{}",
        String::from_utf8_lossy(&digest_symlink.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&digest_symlink.stdout), "cached\n");
    assert!(
        fs::symlink_metadata(fixture.digest())
            .expect("rebuilt digest should exist")
            .file_type()
            .is_file(),
        "the digest symlink should be replaced by a regular sidecar"
    );
    assert!(
        external_digest.is_file(),
        "rejecting a digest symlink must not remove its external target"
    );

    // FIFOs are especially important: opening either member for an
    // unconditional read blocks forever when there is no writer. Metadata
    // validation must reject the node before any read is attempted.
    for (label, member) in [
        ("program FIFO", fixture.program()),
        ("digest FIFO", fixture.digest()),
    ] {
        replace_file_with_fifo(&member);
        let rebuilt = command_output_with_timeout(fixture.command(), timeout, label);
        assert!(
            rebuilt.status.success(),
            "{label} should cause a rebuild, stderr was:\n{}",
            String::from_utf8_lossy(&rebuilt.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&rebuilt.stdout),
            "cached\n",
            "{label} must never become a program result"
        );
        assert!(
            fs::symlink_metadata(&member)
                .unwrap_or_else(|error| panic!("{label} should be replaced: {error}"))
                .file_type()
                .is_file(),
            "{label} should be replaced by a regular cache member"
        );
    }
}

#[cfg(unix)]
#[test]
fn native_run_cache_preserves_verified_entry_when_private_launch_staging_fails() {
    use std::os::unix::fs::MetadataExt;

    let fixture = NativeCacheFixture::new("aura-native-cache-launch-environment");
    let program_before =
        fs::read(fixture.program()).expect("cached program should be readable before launch");
    let digest_before =
        fs::read(fixture.digest()).expect("cached digest should be readable before launch");
    let entry_metadata =
        fs::metadata(&fixture.entry).expect("cache entry metadata should be readable");
    let program_metadata =
        fs::metadata(fixture.program()).expect("cached program metadata should be readable");
    let digest_metadata =
        fs::metadata(fixture.digest()).expect("cached digest metadata should be readable");

    // Rust's Unix temp-dir selection honors TMPDIR. Pointing it at a regular
    // file makes private launch staging fail for environmental reasons after
    // the shared cache bytes have already verified. That must not be confused
    // with evidence that the valid cache entry itself is corrupt.
    let unusable_tmp = fixture.cache.path().join("tmp-is-a-file");
    fs::write(&unusable_tmp, "not a directory").expect("unusable TMPDIR marker should be writable");
    let mut command = fixture.command();
    command
        .env("TMPDIR", &unusable_tmp)
        .env("CC", fixture.cache.path().join("missing-cc"));
    let output = command_output_with_timeout(
        command,
        std::time::Duration::from_secs(10),
        "verified launch with unusable TMPDIR",
    );
    assert!(
        !output.status.success(),
        "a regular-file TMPDIR should exercise the private-staging failure path; stdout was:\n{}\nstderr was:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("failed to create private verified-native directory"),
        "expected an environmental private-staging diagnostic, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        fs::read(fixture.program()).expect("environmental failure must preserve cached program"),
        program_before,
        "environmental launch failure must not rewrite cached program bytes"
    );
    assert_eq!(
        fs::read(fixture.digest()).expect("environmental failure must preserve cached digest"),
        digest_before,
        "environmental launch failure must not rewrite the verified sidecar"
    );
    let entry_after =
        fs::metadata(&fixture.entry).expect("environmental failure must preserve cache entry");
    let program_after =
        fs::metadata(fixture.program()).expect("environmental failure must preserve program");
    let digest_after =
        fs::metadata(fixture.digest()).expect("environmental failure must preserve digest");
    assert_eq!(
        (entry_after.dev(), entry_after.ino()),
        (entry_metadata.dev(), entry_metadata.ino()),
        "environmental launch failure must not replace the cache entry"
    );
    assert_eq!(
        (program_after.dev(), program_after.ino()),
        (program_metadata.dev(), program_metadata.ino()),
        "environmental launch failure must not replace the cached program"
    );
    assert_eq!(
        (digest_after.dev(), digest_after.ino()),
        (digest_metadata.dev(), digest_metadata.ino()),
        "environmental launch failure must not replace the digest sidecar"
    );
}

#[test]
fn run_backend_selector_matches_across_mir_direct_and_auto() {
    let source = "import sys\n\ndef main() -> int32:\n    print(\"selector\")\n    for arg in sys.args():\n        print(arg)\n    return 3\n";
    let (_temp, source_path) = write_temp_source("aura-run-backend-selector", source);
    let expected = "selector\nalpha\nbeta\n";

    for backend in ["mir", "direct", "auto"] {
        let output = Command::new(aura_bin())
            .arg("run")
            .arg("--backend")
            .arg(backend)
            .arg(source_path.display().to_string())
            .arg("--")
            .arg("alpha")
            .arg("beta")
            .output()
            .unwrap_or_else(|error| panic!("failed to run aura run --backend {backend}: {error}"));

        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "{backend} stdout, stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.code(), Some(3), "{backend} exit code");
    }

    // The default is still the MIR runtime, and it agrees with every explicit
    // selector.
    let default_run = Command::new(aura_bin())
        .arg("run")
        .arg(source_path.display().to_string())
        .arg("--")
        .arg("alpha")
        .arg("beta")
        .output()
        .expect("failed to run aura run with the default backend");
    assert_eq!(String::from_utf8_lossy(&default_run.stdout), expected);
    assert_eq!(default_run.status.code(), Some(3));

    let rejected = Command::new(aura_bin())
        .arg("run")
        .arg("--backend")
        .arg("interpreter")
        .arg(source_path.display().to_string())
        .output()
        .expect("failed to run aura run with an unknown backend");
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("--backend mir|direct|auto"));
}

#[test]
fn s1_float32_power_and_python_surface_match_mir_and_direct_backends() {
    let source = r#"
scale: float32 = 2.0

def classify(value: int64) -> str:
    match value:
        case 1 | 2 if value > 1:
            return "two"
        case _:
            return "other"

def main() -> int32:
    powered: float32 = scale ** (3.0 as float32)
    pair: (float32, float32) = divmod(powered, 3.0 as float32)

    mut values: list[int64] = list[int64].with_capacity(4)
    values.append(4)
    values.append(2)
    values.append(2)
    first = values.pop(0)
    values.remove(2)
    values.reserve(8)

    mut labels: set[str] = set[str].with_capacity(2)
    labels.add("aura")
    labels.discard("missing")
    labels.reserve(4)

    mut scores: dict[str, int64] = dict[str, int64].with_capacity(2)
    scores["aura"] = 3
    scores.reserve(4)

    print(f"{powered:.2f}|{pair}|{round(2.5 as float32)}|{classify(2)}")
    print(f"{first}|{values.index(2)}|{values.count(2)}|{'aura' in labels}|{scores['aura']}")
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-s1-direct-python-surface",
        source,
        "8.00|(2.0, 2.0)|2|two\n4|0|1|true|3\n",
    );
}

#[test]
fn s1_narrow_operators_format_sort_and_guarded_patterns_match_backends() {
    let source = r#"
base: int8 = 12

def identity(value: int64) -> int64:
    return value

def describe(value: int8) -> str:
    match value:
        case 0 | 15 if value > (10 as int8):
            return f"{value:04x}"
        case _:
            return "other"

def main() -> int32:
    right: int8 = 3
    assert base > right
    print(base // right)
    print(base % right)
    print((2 as int8) ** right)
    print(base & right)
    print(base | right)
    print(base ^ right)
    print(base << right)
    print(base >> right)

    mut values: list[int64] = [3, 1, 2]
    values.sort(key=identity, reverse=true)
    print(values)

    ratio: float32 = 1.25
    print(f"{base:04x}|{ratio:.2f}|{describe(15 as int8)}")
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-s1-direct-narrow-operators",
        source,
        "4\n0\n8\n0\n15\n15\n96\n1\n[3, 2, 1]\n000c|1.25|000f\n",
    );
}

#[test]
fn s1_string_to_bytes_matches_mir_and_direct_backends() {
    let source = r#"
def main() -> int32:
    print("Aura".to_bytes())
    print("café".to_bytes())
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-s1-direct-string-to-bytes",
        source,
        "[65, 117, 114, 97]\n[99, 97, 102, 101, 204, 129]\n",
    );
}

#[test]
fn s1_discarded_collection_capacity_failures_match_mir_and_direct_backends() {
    for (name, call) in [
        ("list", "list[int64].with_capacity(-1)"),
        ("set", "set[str].with_capacity(-1)"),
        ("dict", "dict[str, int64].with_capacity(-1)"),
    ] {
        let source = format!("def main():\n    {call}\n    print(\"unreachable\")\n");
        assert_run_and_direct_source_failure_with_timeout(
            &format!("aura-s1-discarded-{name}-capacity"),
            &source,
            std::time::Duration::from_secs(30),
            "",
            "error[AU4003]: collection capacity cannot be negative",
        );
    }
}

#[test]
fn run_backends_match_eager_comprehension_behavior() {
    let source = include_str!("../../aura-compiler/tests/fixtures/run-pass/comprehensions.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/comprehensions.stdout");
    let (_temp, source_path) = write_temp_source("aura-comprehension-parity", source);

    for backend in ["mir", "direct"] {
        let output = Command::new(aura_bin())
            .args(["run", "--backend", backend])
            .arg(&source_path)
            .output()
            .unwrap_or_else(|error| {
                panic!("failed to run comprehension fixture with {backend}: {error}")
            });
        assert!(
            output.status.success(),
            "{backend} comprehension run failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "{backend} comprehension output diverged"
        );
    }
}

#[test]
fn run_backends_match_full_comprehension_runtime_matrix() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/comprehension_runtime_matrix.au");
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/comprehension_runtime_matrix.stdout"
    );
    let (_temp, source_path) = write_temp_source("aura-comprehension-runtime-matrix", source);

    for backend in ["mir", "direct"] {
        let output = Command::new(aura_bin())
            .args(["run", "--backend", backend])
            .arg(&source_path)
            .output()
            .unwrap_or_else(|error| {
                panic!("failed to run comprehension runtime matrix with {backend}: {error}")
            });
        assert!(
            output.status.success(),
            "{backend} comprehension runtime matrix failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "{backend} comprehension runtime matrix output diverged"
        );
    }
}

#[test]
fn run_backends_lower_comprehensions_in_function_and_field_defaults() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/comprehension_defaults.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/comprehension_defaults.stdout");
    let (_temp, source_path) = write_temp_source("aura-comprehension-defaults", source);

    for backend in ["mir", "direct"] {
        let output = Command::new(aura_bin())
            .args(["run", "--backend", backend])
            .arg(&source_path)
            .output()
            .unwrap_or_else(|error| {
                panic!("failed to run comprehension defaults with {backend}: {error}")
            });
        assert!(
            output.status.success(),
            "{backend} comprehension defaults run failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "{backend} comprehension defaults output diverged"
        );
    }
}

#[test]
fn owned_slice_matrix_matches_forced_mir_and_direct_backends() {
    let root = repo_root();
    let fixture = "crates/aura-compiler/tests/fixtures/run-pass/owned_list_string_slices.au";
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/owned_list_string_slices.stdout");

    let mir = Command::new(aura_bin())
        .current_dir(&root)
        .args(["run", "--backend", "mir", fixture])
        .output()
        .expect("failed to run owned-slice matrix on the forced MIR backend");
    assert!(
        mir.status.success(),
        "owned-slice matrix should run on MIR, stderr was:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );

    let output_dir = TempDir::new("aura-owned-slice-direct");
    let output_path = output_dir.path().join("out");
    let direct_build = Command::new(aura_bin())
        .current_dir(&root)
        .args(["build", "--backend", "direct", "-o"])
        .arg(&output_path)
        .arg(fixture)
        .output()
        .expect("failed to build owned-slice matrix on the direct backend");
    assert!(
        direct_build.status.success(),
        "owned-slice matrix should build on direct, stderr was:\n{}",
        String::from_utf8_lossy(&direct_build.stderr)
    );

    let direct = generated_binary(&output_path)
        .current_dir(&root)
        .output()
        .expect("failed to run direct owned-slice matrix");
    assert!(
        direct.status.success(),
        "owned-slice matrix should run on direct, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    assert_eq!(
        mir.stdout, direct.stdout,
        "owned list/str slices must produce byte-identical stdout on MIR and direct"
    );
    assert_eq!(String::from_utf8_lossy(&mir.stdout), expected);
}

#[test]
fn owned_slice_au4003_traps_match_forced_mir_and_direct_backends() {
    let root = repo_root();
    let cases = [
        (
            "list reversed bounds",
            "crates/aura-compiler/tests/fixtures/run-fail/list_slice_reversed_bounds.au",
            include_str!(
                "../../aura-compiler/tests/fixtures/run-fail/list_slice_reversed_bounds.diag"
            ),
        ),
        (
            "str normalized start out of bounds",
            "crates/aura-compiler/tests/fixtures/run-fail/string_slice_start_out_of_bounds.au",
            include_str!(
                "../../aura-compiler/tests/fixtures/run-fail/string_slice_start_out_of_bounds.diag"
            ),
        ),
    ];

    for (label, fixture, expected_stderr) in cases {
        let mir = Command::new(aura_bin())
            .current_dir(&root)
            .args(["run", "--backend", "mir", fixture])
            .output()
            .unwrap_or_else(|error| panic!("failed to run {label} trap on MIR: {error}"));
        assert_eq!(
            mir.status.code(),
            Some(1),
            "{label} should exit 1 on MIR; stderr was:\n{}",
            String::from_utf8_lossy(&mir.stderr)
        );

        let output_dir = TempDir::new("aura-owned-slice-trap-direct");
        let output_path = output_dir.path().join("out");
        let direct_build = Command::new(aura_bin())
            .current_dir(&root)
            .args(["build", "--backend", "direct", "-o"])
            .arg(&output_path)
            .arg(fixture)
            .output()
            .unwrap_or_else(|error| panic!("failed to build {label} trap on direct: {error}"));
        assert!(
            direct_build.status.success(),
            "{label} should build on direct; stderr was:\n{}",
            String::from_utf8_lossy(&direct_build.stderr)
        );

        let direct = generated_binary(&output_path)
            .current_dir(&root)
            .output()
            .unwrap_or_else(|error| panic!("failed to run {label} direct trap: {error}"));
        assert_eq!(
            direct.status.code(),
            Some(1),
            "{label} should exit 1 on direct; stderr was:\n{}",
            String::from_utf8_lossy(&direct.stderr)
        );
        assert!(
            mir.stdout.is_empty(),
            "{label} should not print before trapping"
        );
        assert_eq!(
            mir.stdout, direct.stdout,
            "{label} stdout must match on MIR and direct"
        );
        assert_eq!(
            mir.stderr, direct.stderr,
            "{label} AU4003 code, message, source span, and call frame must be byte-identical"
        );
        assert_eq!(
            String::from_utf8_lossy(&mir.stderr),
            expected_stderr,
            "{label} must retain its exact AU4003 diagnostic oracle"
        );
    }
}

#[test]
fn numeric_array_matrix_matches_forced_mir_and_direct_backends() {
    let source = include_str!("../../aura-compiler/tests/fixtures/run-pass/array_runtime.au");
    let expected = include_str!("../../aura-compiler/tests/fixtures/run-pass/array_runtime.stdout");
    assert_run_and_direct_source_stdout("aura-numeric-array-matrix", source, expected);

    let rank_one_source = [
        "def main() -> int32:",
        "    source: list[int32] = [4, 5, 6]",
        "    mut values = Array[int32].from_list(source, [3])",
        "    print(values[-1])",
        "    values[0] = 9",
        "    print(values[0])",
        "    return 0",
    ]
    .join("\n");
    assert_run_and_direct_source_stdout(
        "aura-numeric-array-rank-one-index",
        &rank_one_source,
        "6\n9\n",
    );
}

#[test]
fn numeric_array_operator_modes_match_forced_mir_and_direct_backends() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/array_operator_modes.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/array_operator_modes.stdout");
    assert_run_and_direct_source_stdout("aura-numeric-array-operator-modes", source, expected);
}

#[test]
fn numeric_array_composed_member_results_match_forced_mir_and_direct_backends() {
    let source = r#"
def increment(value: int32) -> int32:
    return value + 1

def main() -> int32:
    print(Array[int32].zeros([2]).shape())
    print(Array[int32].full([2], 3).len())
    print(Array[int32].full([2], 4).clone())
    print(Array[int32].full([2], 5)[0:1])
    print(Array[int32].full([2], 6).get([0]))
    print(Array[int32].full([2], 7)[0])
    print(Array[int32].full([2], 8).map(increment).shape())
    print(Array[int32].full([2], 9).sum())
    print(Array[int32].full([2], 10).min())
    print(Array[int32].full([2], 11).max())
    print(Array[int32].full([2], 12).mean())
    print(Array[int32].full([2], 2147483647).wrapping_add(1))
    print(Array[int32].full([2], 2147483647).saturating_add(1))
    return 0
"#;
    let expected = concat!(
        "[2]\n",
        "2\n",
        "Array[int32](shape=[2], values=[4, 4])\n",
        "Array[int32](shape=[1], values=[5])\n",
        "Option.Some(6)\n",
        "7\n",
        "[2]\n",
        "18\n",
        "10\n",
        "11\n",
        "12.0\n",
        "Array[int32](shape=[2], values=[-2147483648, -2147483648])\n",
        "Array[int32](shape=[2], values=[2147483647, 2147483647])\n",
    );
    assert_run_and_direct_source_stdout(
        "aura-numeric-array-composed-member-results",
        source,
        expected,
    );
}

#[test]
fn numeric_array_all_dtypes_match_forced_mir_and_direct_backends() {
    let source = r#"
def main() -> int32:
    i32_zeros = Array[int32].zeros([2])
    i32_full = Array[int32].full([2], 3)
    i32_source: list[int32] = [1, 2]
    i32_values = Array[int32].from_list(i32_source, [2])
    print(i32_zeros)
    print(i32_full)
    print(i32_values)
    print(i32_values + i32_full)
    print(i32_values * 2)
    print(i32_values.sum())
    print(i32_values.min())
    print(i32_values.max())
    print(i32_values.mean())

    i64_zeros = Array[int64].zeros([2])
    i64_full = Array[int64].full([2], 2)
    i64_source: list[int64] = [5000000000, 6000000000]
    i64_values = Array[int64].from_list(i64_source, [2])
    print(i64_zeros)
    print(i64_full)
    print(i64_values)
    print(i64_values + i64_full)
    print(7000000000 - i64_values)
    print(i64_values.sum())
    print(i64_values.min())
    print(i64_values.max())
    print(i64_values.mean())
    mut i64_clone = i64_values.clone()
    i64_clone[0] = 9
    print(i64_values[0])
    print(i64_clone[0])

    f32_zeros = Array[float32].zeros([2])
    f32_full = Array[float32].full([2], 0.5)
    f32_source: list[float32] = [1.5, 2.5]
    f32_values = Array[float32].from_list(f32_source, [2])
    print(f32_zeros)
    print(f32_full)
    print(f32_values)
    print(f32_values + f32_full)
    print(2.0 * f32_values)
    print(f32_values.sum())
    print(f32_values.min())
    print(f32_values.max())
    print(f32_values.mean())

    f64_zeros = Array[float64].zeros([2])
    f64_full = Array[float64].full([2], 2.0)
    f64_source: list[float64] = [4.0, 8.0]
    f64_values = Array[float64].from_list(f64_source, [2])
    print(f64_zeros)
    print(f64_full)
    print(f64_values)
    print(f64_values / f64_full)
    print(16.0 / f64_values)
    print(f64_values.sum())
    print(f64_values.min())
    print(f64_values.max())
    print(f64_values.mean())
    return 0
"#;
    let expected = concat!(
        "Array[int32](shape=[2], values=[0, 0])\n",
        "Array[int32](shape=[2], values=[3, 3])\n",
        "Array[int32](shape=[2], values=[1, 2])\n",
        "Array[int32](shape=[2], values=[4, 5])\n",
        "Array[int32](shape=[2], values=[2, 4])\n",
        "3\n1\n2\n1.5\n",
        "Array[int64](shape=[2], values=[0, 0])\n",
        "Array[int64](shape=[2], values=[2, 2])\n",
        "Array[int64](shape=[2], values=[5000000000, 6000000000])\n",
        "Array[int64](shape=[2], values=[5000000002, 6000000002])\n",
        "Array[int64](shape=[2], values=[2000000000, 1000000000])\n",
        "11000000000\n5000000000\n6000000000\n5500000000.0\n",
        "5000000000\n9\n",
        "Array[float32](shape=[2], values=[0.0, 0.0])\n",
        "Array[float32](shape=[2], values=[0.5, 0.5])\n",
        "Array[float32](shape=[2], values=[1.5, 2.5])\n",
        "Array[float32](shape=[2], values=[2.0, 3.0])\n",
        "Array[float32](shape=[2], values=[3.0, 5.0])\n",
        "4.0\n1.5\n2.5\n2.0\n",
        "Array[float64](shape=[2], values=[0.0, 0.0])\n",
        "Array[float64](shape=[2], values=[2.0, 2.0])\n",
        "Array[float64](shape=[2], values=[4.0, 8.0])\n",
        "Array[float64](shape=[2], values=[2.0, 4.0])\n",
        "Array[float64](shape=[2], values=[4.0, 2.0])\n",
        "12.0\n4.0\n8.0\n6.0\n",
    );

    assert_mir_and_direct_source_stdout_with_timeout_and_workers(
        "aura-array-all-dtypes",
        source,
        std::time::Duration::from_secs(60),
        expected,
        1,
    );
}

#[test]
fn numeric_array_nested_collection_copies_match_forced_mir_and_direct_backends() {
    let source = r#"
class ArrayHolder:
    array: Array[int32]
    count: int32

def print_array(value: own Option[Array[int32]]):
    match own value:
        case Some(array):
            print(array)
        case None:
            print("missing")

def main() -> int32:
    source: list[int32] = [3, 4]
    arrays: list[Array[int32]] = [Array[int32].from_list(source, [2])]
    print_array(arrays.get(0))
    arrays_copy = arrays.copy()
    print_array(arrays_copy.get(0))

    map_source: list[int32] = [7, 8]
    arrays_by_name: dict[str, Array[int32]] = {
        "item": Array[int32].from_list(map_source, [2])
    }
    print_array(arrays_by_name.get("item"))
    values = arrays_by_name.values()
    print_array(values.get(0))
    items = arrays_by_name.items()
    match own items.get(0):
        case Some((_key, value)):
            print(value)
        case None:
            print("missing item")
    map_copy = arrays_by_name.copy()
    print_array(map_copy.get("item"))

    holder_source: list[int32] = [11, 12]
    mut holder = ArrayHolder(
        array=Array[int32].from_list(holder_source, [2]),
        count=0
    )
    holder.count = 1
    print(holder.array)
    print(holder.count)
    return 0
"#;
    let expected = concat!(
        "Array[int32](shape=[2], values=[3, 4])\n",
        "Array[int32](shape=[2], values=[3, 4])\n",
        "Array[int32](shape=[2], values=[7, 8])\n",
        "Array[int32](shape=[2], values=[7, 8])\n",
        "Array[int32](shape=[2], values=[7, 8])\n",
        "Array[int32](shape=[2], values=[7, 8])\n",
        "Array[int32](shape=[2], values=[11, 12])\n",
        "1\n",
    );
    assert_run_and_direct_source_stdout(
        "aura-numeric-array-nested-collection-copies",
        source,
        expected,
    );
}

#[test]
fn numeric_array_invalid_shapes_match_forced_mir_and_direct_backends() {
    let cases = [
        (
            "rank zero",
            "def main():\n    print(Array[int64].zeros([]))\n",
            "array rank must be at least one",
        ),
        (
            "negative dimension",
            "def main():\n    print(Array[float64].full([-1], 2.0))\n",
            "Array shape axis 0 cannot be negative",
        ),
    ];

    for (label, source, expected_message) in cases {
        let (temp, source_path) =
            write_temp_source(&format!("aura-array-invalid-shape-{label}"), source);
        let mir = Command::new(aura_bin())
            .args(["run", "--backend", "mir"])
            .arg(&source_path)
            .output()
            .unwrap_or_else(|error| panic!("failed to run Array {label} trap on MIR: {error}"));
        assert_eq!(
            mir.status.code(),
            Some(1),
            "Array {label} should exit 1 on MIR; stderr was:\n{}",
            String::from_utf8_lossy(&mir.stderr)
        );

        let output_path = temp.path().join("out");
        let direct_build = Command::new(aura_bin())
            .args(["build", "--backend", "direct", "-o"])
            .arg(&output_path)
            .arg(&source_path)
            .output()
            .unwrap_or_else(|error| {
                panic!("failed to build Array {label} trap on direct: {error}")
            });
        assert!(
            direct_build.status.success(),
            "Array {label} should build on direct; stderr was:\n{}",
            String::from_utf8_lossy(&direct_build.stderr)
        );
        let direct = generated_binary(&output_path)
            .output()
            .unwrap_or_else(|error| panic!("failed to run Array {label} direct trap: {error}"));
        assert_eq!(
            direct.status.code(),
            Some(1),
            "Array {label} should exit 1 on direct; stderr was:\n{}",
            String::from_utf8_lossy(&direct.stderr)
        );
        assert_eq!(mir.stdout, direct.stdout, "Array {label} stdout diverged");
        assert_eq!(
            mir.stderr, direct.stderr,
            "Array {label} AU4007 diagnostic, span, and frame must match"
        );
        let stderr = String::from_utf8_lossy(&mir.stderr);
        assert!(stderr.contains("error[AU4007]"), "{stderr}");
        assert!(stderr.contains(expected_message), "{stderr}");
    }
}

#[test]
fn fixed_width_integer_methods_match_forced_mir_and_direct_backends() {
    let source = [
        "def main() -> int32:",
        "    signed_max = 127 as int8",
        "    signed_min = (-128) as int8",
        "    signed_one = 1 as int8",
        "    signed_two = 2 as int8",
        "    print(signed_max.wrapping_add(signed_one))",
        "    print(signed_min.wrapping_sub(signed_one))",
        "    print(signed_max.wrapping_mul(signed_two))",
        "    print(signed_max.saturating_add(signed_one))",
        "    print(signed_min.saturating_sub(signed_one))",
        "    print(signed_max.saturating_mul(signed_two))",
        "    unsigned_max = 255 as uint8",
        "    unsigned_zero = 0 as uint8",
        "    unsigned_one = 1 as uint8",
        "    unsigned_two = 2 as uint8",
        "    print(unsigned_max.wrapping_add(unsigned_one))",
        "    print(unsigned_zero.wrapping_sub(unsigned_one))",
        "    print(unsigned_max.wrapping_mul(unsigned_two))",
        "    print(unsigned_max.saturating_add(unsigned_one))",
        "    print(unsigned_zero.saturating_sub(unsigned_one))",
        "    print(unsigned_max.saturating_mul(unsigned_two))",
        "    narrow_max: int16 = 32767",
        "    narrow_min: int16 = -32768",
        "    print(narrow_max.wrapping_add(1))",
        "    print(narrow_min.wrapping_sub(1))",
        "    print(narrow_max.wrapping_mul(2))",
        "    print(narrow_max.saturating_add(1))",
        "    print(narrow_min.saturating_sub(1))",
        "    print(narrow_max.saturating_mul(2))",
        "    return 0",
    ]
    .join("\n");
    assert_run_and_direct_source_stdout(
        "aura-fixed-width-integer-methods",
        &source,
        "-128\n127\n-2\n127\n-128\n127\n0\n255\n254\n255\n0\n255\n-32768\n32767\n-2\n32767\n-32768\n32767\n",
    );
}

#[test]
fn numeric_array_traps_match_forced_mir_and_direct_backends() {
    let root = repo_root();
    let cases = [
        (
            "binary shape mismatch",
            "crates/aura-compiler/tests/fixtures/run-fail/array_binary_shape_mismatch.au",
            "error[AU4007]",
        ),
        (
            "checked overflow",
            "crates/aura-compiler/tests/fixtures/run-fail/array_checked_overflow.au",
            "error[AU4002]",
        ),
        (
            "floating division by zero",
            "crates/aura-compiler/tests/fixtures/run-fail/array_division_by_zero.au",
            "error[AU4004]",
        ),
        (
            "empty minimum",
            "crates/aura-compiler/tests/fixtures/run-fail/array_empty_min.au",
            "error[AU4007]",
        ),
        (
            "empty maximum",
            "crates/aura-compiler/tests/fixtures/run-fail/array_empty_max.au",
            "error[AU4007]",
        ),
        (
            "empty mean",
            "crates/aura-compiler/tests/fixtures/run-fail/array_empty_mean.au",
            "error[AU4007]",
        ),
        (
            "index rank mismatch",
            "crates/aura-compiler/tests/fixtures/run-fail/array_index_rank_mismatch.au",
            "error[AU4007]",
        ),
        (
            "integer mode shape mismatch",
            "crates/aura-compiler/tests/fixtures/run-fail/array_integer_mode_shape_mismatch.au",
            "error[AU4007]",
        ),
        (
            "map callback trap",
            "crates/aura-compiler/tests/fixtures/run-fail/array_map_callback_trap.au",
            "error[AU4003]",
        ),
        (
            "set out of bounds",
            "crates/aura-compiler/tests/fixtures/run-fail/array_set_out_of_bounds.au",
            "error[AU4003]",
        ),
        (
            "set rank mismatch",
            "crates/aura-compiler/tests/fixtures/run-fail/array_set_rank_mismatch.au",
            "error[AU4007]",
        ),
        (
            "indexed assignment out of bounds",
            "crates/aura-compiler/tests/fixtures/run-fail/array_index_assignment_out_of_bounds.au",
            "error[AU4003]",
        ),
        (
            "shape mismatch",
            "crates/aura-compiler/tests/fixtures/run-fail/array_shape_mismatch.au",
            "error[AU4007]",
        ),
        (
            "slice out of bounds",
            "crates/aura-compiler/tests/fixtures/run-fail/array_slice_negative_no_clamp.au",
            "error[AU4003]",
        ),
    ];

    for (label, fixture, expected_code) in cases {
        let mir = Command::new(aura_bin())
            .current_dir(&root)
            .args(["run", "--backend", "mir", fixture])
            .output()
            .unwrap_or_else(|error| panic!("failed to run Array {label} trap on MIR: {error}"));
        assert_eq!(
            mir.status.code(),
            Some(1),
            "Array {label} should exit 1 on MIR; stderr was:\n{}",
            String::from_utf8_lossy(&mir.stderr)
        );

        let output_dir = TempDir::new("aura-array-trap-direct");
        let output_path = output_dir.path().join("out");
        let direct_build = Command::new(aura_bin())
            .current_dir(&root)
            .args(["build", "--backend", "direct", "-o"])
            .arg(&output_path)
            .arg(fixture)
            .output()
            .unwrap_or_else(|error| {
                panic!("failed to build Array {label} trap on direct: {error}")
            });
        assert!(
            direct_build.status.success(),
            "Array {label} should build on direct; stderr was:\n{}",
            String::from_utf8_lossy(&direct_build.stderr)
        );

        let direct = generated_binary(&output_path)
            .current_dir(&root)
            .output()
            .unwrap_or_else(|error| panic!("failed to run Array {label} direct trap: {error}"));
        assert_eq!(
            direct.status.code(),
            Some(1),
            "Array {label} should exit 1 on direct; stderr was:\n{}",
            String::from_utf8_lossy(&direct.stderr)
        );
        assert_eq!(
            mir.stdout, direct.stdout,
            "Array {label} stdout must match on MIR and direct"
        );
        assert_eq!(
            mir.stderr, direct.stderr,
            "Array {label} code, message, source span, and call frame must be byte-identical"
        );
        assert!(
            String::from_utf8_lossy(&mir.stderr).contains(expected_code),
            "Array {label} should retain {expected_code}; stderr was:\n{}",
            String::from_utf8_lossy(&mir.stderr)
        );
    }
}

#[test]
fn run_backends_drop_partial_comprehension_before_propagating_trap() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-fail/comprehension_partial_result_trap.au"
    );
    let (_temp, source_path) = write_temp_source("aura-comprehension-partial-result-trap", source);

    for backend in ["mir", "direct"] {
        let output = Command::new(aura_bin())
            .args(["run", "--backend", backend])
            .arg(&source_path)
            .output()
            .unwrap_or_else(|error| {
                panic!("failed to run trapping comprehension with {backend}: {error}")
            });
        assert!(
            !output.status.success(),
            "{backend} trapping comprehension should fail"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "build 1\nbuild 2\nclose outer\n",
            "{backend} must preserve eager progress and unwind the surrounding resource"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("list index `0` is out of bounds for length `0`"),
            "{backend} should preserve the body trap, stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn run_backends_publish_mutable_match_guard_writeback_before_trap_cleanup() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-fail/match_mut_guard_trap_writeback.au"
    );
    let (_temp, source_path) = write_temp_source("aura-match-guard-trap-writeback", source);

    for backend in ["mir", "direct"] {
        let output = Command::new(aura_bin())
            .args(["run", "--backend", backend])
            .arg(&source_path)
            .output()
            .unwrap_or_else(|error| {
                panic!("failed to run trapping mutable match guard with {backend}: {error}")
            });
        assert!(!output.status.success(), "{backend} guard should trap");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "6\n",
            "{backend} cleanup must observe the guard mutation"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("division by zero"),
            "{backend} must retain the guard trap as primary; stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn compile_commands_accept_membership_and_comparison_chains() {
    let (temp, source_path) = write_temp_source(
        "aura-membership-and-chains",
        "def main():\n    ports = [80, 443]\n    if 443 in ports and 1 <= 80 < 1024:\n        print(\"ok\")\n",
    );
    let output_path = temp.path().join("out");
    let commands = [
        vec![
            "check".to_string(),
            "--format".to_string(),
            "json".to_string(),
        ],
        vec!["run".to_string()],
        vec![
            "build".to_string(),
            "--backend".to_string(),
            "direct".to_string(),
            "-o".to_string(),
            output_path.display().to_string(),
        ],
    ];

    for mut arguments in commands {
        let command_name = arguments[0].clone();
        arguments.push(source_path.display().to_string());
        let output = Command::new(aura_bin())
            .args(&arguments)
            .output()
            .unwrap_or_else(|error| panic!("failed to run aura {command_name}: {error}"));
        assert!(
            output.status.success(),
            "{command_name} should accept membership and comparison chains, stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let direct = Command::new(&output_path)
        .output()
        .expect("the direct binary should run");
    assert_eq!(String::from_utf8_lossy(&direct.stdout), "ok\n");

    let (_reject_temp, reject_path) = write_temp_source(
        "aura-membership-rejection",
        "def main():\n    print(1 in 5)\n",
    );
    let rejected = Command::new(aura_bin())
        .args(["check", "--format", "json"])
        .arg(reject_path.display().to_string())
        .output()
        .expect("failed to run aura check");
    assert!(!rejected.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&rejected.stderr).expect("check should emit JSON");
    let diagnostic = &report["diagnostics"][0];
    assert_eq!(diagnostic["code"], "AU2003");
    assert_eq!(
        diagnostic["message"],
        "`in` requires a `list[T]`, `set[T]`, `dict[K, V]`, or `str` container, found `int64`"
    );
}

#[test]
fn direct_backend_reports_recursion_overflow_without_signalling() {
    let source = r#"def recurse(n: int32) -> int32:
    if n == 0:
        return 0
    return recurse(n - 1)

def main() -> int32:
    return recurse(10000000)
"#;

    let (_build, run) = build_and_run_direct_source("aura-direct-recursion", source);
    assert!(
        !run.status.success(),
        "deep recursion should fail cleanly in the direct backend"
    );
    assert_ne!(
        run.status.code(),
        None,
        "direct backend recursion overflow should not terminate by signal"
    );
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("maximum call depth"),
        "expected a direct-backend recursion diagnostic, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn help_flags_exit_successfully() {
    for args in [["help"], ["--help"], ["-h"]] {
        let output = Command::new(aura_bin())
            .args(args)
            .output()
            .expect("failed to run aura help");

        assert!(
            output.status.success(),
            "help path {:?} should succeed, stderr was:\n{}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("usage: aura"),
            "help path {:?} should print usage",
            args
        );
        assert!(
            !stdout.contains("run-mir"),
            "help path {:?} should no longer advertise `run-mir`, stdout was:\n{}",
            args,
            stdout
        );
        assert!(
            stdout.contains("or: aura build -o <output>"),
            "help path {:?} should show that `build -o` is required, stdout was:\n{}",
            args,
            stdout
        );
        assert!(
            !stdout.contains("aura build [-o <output>]"),
            "help path {:?} must not show the required output option as optional, stdout was:\n{}",
            args,
            stdout
        );
        assert!(
            stdout
                .lines()
                .filter(|line| line.contains("aura build"))
                .all(|line| line.contains("-o <output>")),
            "every advertised build form must include required `-o <output>`, stdout was:\n{}",
            stdout
        );
        assert!(
            stdout.contains(
                "aura test [-k <substring>] [--format json] [--timeout-ms <n>] [path ...]"
            ),
            "help must advertise every maintained test-runner option, stdout was:\n{}",
            stdout
        );
        assert!(
            stdout.contains("or: aura upgrade"),
            "help must advertise the updater, stdout was:\n{}",
            stdout
        );
    }
}

#[cfg(unix)]
#[test]
fn upgrade_downloads_and_runs_the_official_installer_for_the_active_prefix() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new("aura-upgrade-success");
    let fake_bin = temp.path().join("bin");
    let prefix = temp.path().join("installed-aura");
    fs::create_dir_all(&fake_bin).expect("fake executable directory should be created");

    let installer = temp.path().join("installer.sh");
    fs::write(
        &installer,
        "#!/bin/sh\nset -eu\nprintf 'installer-prefix=%s\\n' \"$AURA_INSTALL_PREFIX\"\n",
    )
    .expect("fake installer should be written");

    let curl = fake_bin.join("curl");
    fs::write(
        &curl,
        "#!/bin/sh\nset -eu\noutput=\nwhile test \"$#\" -gt 0; do\n  case \"$1\" in\n    -o) output=$2; shift 2 ;;\n    *) shift ;;\n  esac\ndone\ntest -n \"$output\"\ncp \"$AURA_TEST_UPGRADE_INSTALLER\" \"$output\"\n",
    )
    .expect("fake curl should be written");
    let mut permissions = fs::metadata(&curl)
        .expect("fake curl metadata should exist")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&curl, permissions).expect("fake curl should be executable");

    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.clone()).chain(std::env::split_paths(&inherited_path)),
    )
    .expect("test PATH should be valid");
    let output = Command::new(aura_bin())
        .arg("upgrade")
        .env("PATH", path)
        .env(
            "AURA_UPGRADE_INSTALLER_URL",
            "https://example.invalid/install.sh",
        )
        .env("AURA_TEST_UPGRADE_INSTALLER", &installer)
        .env("AURA_INSTALL_PREFIX", &prefix)
        .output()
        .expect("failed to run aura upgrade");

    assert!(
        output.status.success(),
        "upgrade should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("installer-prefix={}", prefix.display())),
        "the installer must inherit the selected install prefix, stdout was:\n{stdout}"
    );
    assert!(
        stdout.contains("Aura upgrade complete"),
        "upgrade should report completion, stdout was:\n{stdout}"
    );
}

#[test]
fn upgrade_rejects_arguments_and_the_unratified_update_alias() {
    let extra = Command::new(aura_bin())
        .args(["upgrade", "now"])
        .output()
        .expect("failed to run aura upgrade with an extra argument");
    assert_eq!(extra.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&extra.stderr).contains("`aura upgrade` does not accept arguments"),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&extra.stderr)
    );

    let retired = Command::new(aura_bin())
        .arg("update")
        .output()
        .expect("failed to run the unsupported aura update spelling");
    assert_eq!(retired.status.code(), Some(2));
    assert!(
        !String::from_utf8_lossy(&retired.stdout).contains("upgrade"),
        "the unsupported spelling must not run the updater"
    );
}

#[test]
fn run_mir_command_is_rejected() {
    let fixture = repo_root().join("examples/basics/simple_example.au");
    let output = Command::new(aura_bin())
        .arg("run-mir")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run-mir");

    assert!(
        !output.status.success(),
        "`run-mir` should be rejected now, stdout was:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("usage: aura"),
        "`run-mir` rejection should print usage, stderr was:\n{}",
        stderr
    );
}

#[test]
fn version_flags_exit_successfully() {
    for args in [["version"], ["--version"], ["-V"]] {
        let output = Command::new(aura_bin())
            .args(args)
            .output()
            .expect("failed to run aura version");

        assert!(
            output.status.success(),
            "version path {:?} should succeed, stderr was:\n{}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let prefix = format!("aura {}-dev (", env!("CARGO_PKG_VERSION"));
        let commit = stdout
            .strip_prefix(&prefix)
            .and_then(|value| value.strip_suffix(")\n"))
            .unwrap_or_else(|| {
                panic!(
                    "version path {:?} should print the development channel and commit, stdout was:\n{}",
                    args, stdout
                )
            });
        assert_eq!(commit.len(), 12, "version commit should use 12 hex digits");
        assert!(
            commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "version commit should be hexadecimal, found `{commit}`"
        );
    }
}

#[test]
fn nested_package_module_can_be_checked_directly() {
    let fixture = repo_root().join("examples/modules/pkg/user.au");
    let output = Command::new(aura_bin())
        .arg("check")
        .arg(&fixture)
        .output()
        .expect("failed to run aura check");

    assert!(
        output.status.success(),
        "direct check of nested package module should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}

#[test]
fn nested_package_module_can_be_analyzed_directly() {
    let fixture = repo_root().join("examples/modules/pkg/user.au");
    let output = Command::new(aura_bin())
        .arg("analyze")
        .arg(&fixture)
        .output()
        .expect("failed to run aura analyze");

    assert!(
        output.status.success(),
        "direct analyze of nested package module should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"diagnostics\":[]"),
        "analysis should not report false import diagnostics, stdout was:\n{}",
        stdout
    );
    assert!(
        stdout.contains("\"name\":\"User\""),
        "analysis should still include symbols, stdout was:\n{}",
        stdout
    );
}

#[test]
fn analyze_recovers_symbols_for_dangling_dot_stdin_buffers() {
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

    let mut child = Command::new(aura_bin())
        .arg("analyze")
        .arg("--stdin")
        .arg("/virtual/counter.au")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura analyze");

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write source");

    let output = child
        .wait_with_output()
        .expect("failed to collect aura analyze output");

    assert!(
        output.status.success(),
        "analyze should succeed on dangling-dot buffers"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("analyze should return valid JSON");
    let symbols = json["symbols"]
        .as_array()
        .expect("symbols should be an array");
    let occurrences = json["occurrences"]
        .as_array()
        .expect("occurrences should be an array");
    assert!(
        !symbols.is_empty(),
        "dangling-dot analysis should still return symbols"
    );
    assert!(
        !occurrences.is_empty(),
        "dangling-dot analysis should still return occurrences"
    );
}

#[test]
fn analyze_recovers_symbols_for_dangling_dot_at_eof_stdin_buffers() {
    let source = [
        "class Counter:",
        "    value: int32",
        "",
        "def main() -> int32:",
        "    counter = Counter(value=1)",
        "    counter.",
    ]
    .join("\n");

    let mut child = Command::new(aura_bin())
        .arg("analyze")
        .arg("--stdin")
        .arg("/virtual/counter.au")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura analyze");

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write source");

    let output = child
        .wait_with_output()
        .expect("failed to collect aura analyze output");

    assert!(
        output.status.success(),
        "analyze should succeed on dangling-dot EOF buffers, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("analyze should return valid JSON");
    assert!(
        !json["symbols"]
            .as_array()
            .expect("symbols should be an array")
            .is_empty(),
        "dangling-dot EOF analysis should still return symbols"
    );
    assert!(
        !json["occurrences"]
            .as_array()
            .expect("occurrences should be an array")
            .is_empty(),
        "dangling-dot EOF analysis should still return occurrences"
    );
}

#[test]
fn analyze_stdin_resolves_local_module_imports() {
    let temp = TempDir::new("aura-cli-analyze-modules");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\n",
    )
    .expect("failed to write helper module");
    let main_path = temp.path().join("main.au");
    let source = "import helpers.math\n\ndef main() -> int32:\n    print(helpers.math.double(value=5))\n    return 0\n";

    let mut child = Command::new(aura_bin())
        .arg("analyze")
        .arg("--stdin")
        .arg(&main_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura analyze");

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write source");

    let output = child
        .wait_with_output()
        .expect("failed to collect aura analyze output");

    assert!(
        output.status.success(),
        "analyze should succeed for module-aware stdin buffers, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("analyze should return valid JSON");
    assert_eq!(
        json["diagnostics"]
            .as_array()
            .expect("diagnostics should be an array")
            .len(),
        0,
        "analysis should not report diagnostics for a valid local-module program"
    );
    assert!(
        json["occurrences"]
            .as_array()
            .expect("occurrences should be an array")
            .iter()
            .any(|occurrence| occurrence["hover"]
                .as_str()
                .unwrap_or_default()
                .contains("function double")),
        "analysis should include occurrences for imported module members"
    );
}

#[test]
fn check_stdin_resolves_local_module_imports() {
    let temp = TempDir::new("aura-cli-check-modules");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\n",
    )
    .expect("failed to write helper module");
    let main_path = temp.path().join("main.au");
    let source =
        "import helpers.math\n\ndef main() -> int32:\n    print(helpers.math.double(value=5))\n    return 0\n";

    let mut child = Command::new(aura_bin())
        .arg("check")
        .arg("--stdin")
        .arg(&main_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura check");

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write source");

    let output = child
        .wait_with_output()
        .expect("failed to collect aura check output");

    assert!(
        output.status.success(),
        "check should succeed for module-aware stdin buffers, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}

#[test]
fn run_stdin_resolves_local_module_imports() {
    let temp = TempDir::new("aura-cli-run-modules-stdin");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\n",
    )
    .expect("failed to write helper module");
    let main_path = temp.path().join("main.au");
    let source =
        "import helpers.math\n\ndef main() -> int32:\n    print(helpers.math.double(value=5))\n    return 0\n";

    let mut child = Command::new(aura_bin())
        .arg("run")
        .arg("--stdin")
        .arg(&main_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura run");

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write source");

    let output = child
        .wait_with_output()
        .expect("failed to collect aura run output");

    assert!(
        output.status.success(),
        "run should succeed for module-aware stdin buffers, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "10\n");
}

#[test]
fn run_stdin_with_path_resolves_local_module_imports() {
    let temp = TempDir::new("aura-cli-run-modules-stdin");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\n",
    )
    .expect("failed to write helper module");
    let main_path = temp.path().join("main.au");
    let source =
        "import helpers.math\n\ndef main() -> int32:\n    print(helpers.math.double(value=5))\n    return 0\n";

    let mut child = Command::new(aura_bin())
        .arg("run")
        .arg("--stdin")
        .arg(&main_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura run");

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write source");

    let output = child
        .wait_with_output()
        .expect("failed to collect aura run output");

    assert!(
        output.status.success(),
        "run should succeed for module-aware stdin buffers, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "10\n");
}

#[test]
fn mir_stdin_resolves_local_module_imports() {
    let temp = TempDir::new("aura-cli-mir-modules-stdin");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\n",
    )
    .expect("failed to write helper module");
    let main_path = temp.path().join("main.au");
    let source =
        "import helpers.math\n\ndef main() -> int32:\n    print(helpers.math.double(value=5))\n    return 0\n";

    let mut child = Command::new(aura_bin())
        .arg("mir")
        .arg("--stdin")
        .arg(&main_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura mir");

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write source");

    let output = child
        .wait_with_output()
        .expect("failed to collect aura mir output");

    assert!(
        output.status.success(),
        "mir should succeed for module-aware stdin buffers, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("double"),
        "MIR dump should include imported module calls"
    );
}

#[test]
fn complete_stdin_resolves_local_module_member_completions() {
    let temp = TempDir::new("aura-cli-complete-modules");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\n",
    )
    .expect("failed to write helper module");
    let main_path = temp.path().join("main.au");
    let source = "import helpers.math\n\ndef main() -> int32:\n    helpers.math.\n    return 0\n";

    let mut child = Command::new(aura_bin())
        .arg("complete")
        .arg("--line")
        .arg("3")
        .arg("--character")
        .arg("17")
        .arg("--trigger")
        .arg(".")
        .arg("--stdin")
        .arg(&main_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura complete");

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write source");

    let output = child
        .wait_with_output()
        .expect("failed to collect aura complete output");

    assert!(
        output.status.success(),
        "complete should succeed for module-aware stdin buffers, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("complete should return valid JSON");
    assert!(
        json.as_array()
            .expect("completions should be an array")
            .iter()
            .any(|item| item["name"].as_str() == Some("double")),
        "module member completions should include exported functions"
    );
}

#[test]
fn editor_stdin_analysis_and_completion_do_not_write_package_lockfile() {
    let temp = TempDir::new("aura-cli-editor-no-lock");
    fs::create_dir_all(temp.path().join("app/src")).expect("failed to create app src");
    fs::create_dir_all(temp.path().join("util/src")).expect("failed to create util src");
    fs::write(
        temp.path().join("app/Aura.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nutil = { path = \"../util\" }\n",
    )
    .expect("failed to write app manifest");
    fs::write(
        temp.path().join("util/Aura.toml"),
        "[package]\nname = \"util\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .expect("failed to write util manifest");
    fs::write(
        temp.path().join("util/src/math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\n",
    )
    .expect("failed to write util module");

    let main_path = temp.path().join("app/src/main.au");
    let analyze_source =
        "import util.math\n\ndef main() -> int32:\n    print(util.math.double(5))\n    return 0\n";
    let lockfile = temp.path().join("app/Aura.lock");
    assert!(
        !lockfile.exists(),
        "test package should start without a lockfile"
    );

    let mut analyze = Command::new(aura_bin())
        .arg("analyze")
        .arg("--stdin")
        .arg(&main_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura analyze");
    analyze
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(analyze_source.as_bytes())
        .expect("failed to write analyze source");
    let analyze_output = analyze
        .wait_with_output()
        .expect("failed to collect aura analyze output");
    assert!(
        analyze_output.status.success(),
        "analyze should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&analyze_output.stderr)
    );
    assert!(
        !lockfile.exists(),
        "analyze --stdin should not write Aura.lock for editor buffers"
    );

    let completion_source =
        "import util.math\n\ndef main() -> int32:\n    util.math.\n    return 0\n";
    let mut complete = Command::new(aura_bin())
        .arg("complete")
        .arg("--line")
        .arg("3")
        .arg("--character")
        .arg("14")
        .arg("--trigger")
        .arg(".")
        .arg("--stdin")
        .arg(&main_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura complete");
    complete
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(completion_source.as_bytes())
        .expect("failed to write completion source");
    let complete_output = complete
        .wait_with_output()
        .expect("failed to collect aura complete output");
    assert!(
        complete_output.status.success(),
        "complete should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&complete_output.stderr)
    );
    assert!(
        !lockfile.exists(),
        "complete --stdin should not write Aura.lock for editor buffers"
    );
}

#[test]
fn complete_stdin_includes_imported_trait_methods() {
    let temp = TempDir::new("aura-cli-complete-imported-trait");
    fs::create_dir_all(temp.path().join("pkg")).expect("failed to create package dir");
    fs::write(
        temp.path().join("pkg/named.au"),
        "public trait Named:\n    def name(self) -> str\n",
    )
    .expect("failed to write trait module");
    fs::write(
        temp.path().join("pkg/user.au"),
        "from pkg.named import Named\n\npublic class User:\n    public label: str\n\nimpl Named for User:\n    def name(self) -> str:\n        return self.label.clone()\n",
    )
    .expect("failed to write user module");
    let main_path = temp.path().join("main.au");
    let source =
        "from pkg.user import User\n\ndef main() -> int32:\n    user = User(label=\"Ada\")\n    user.\n    return 0\n";

    let mut child = Command::new(aura_bin())
        .arg("complete")
        .arg("--line")
        .arg("4")
        .arg("--character")
        .arg("9")
        .arg("--trigger")
        .arg(".")
        .arg("--stdin")
        .arg(&main_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura complete");

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write source");

    let output = child
        .wait_with_output()
        .expect("failed to collect aura complete output");

    assert!(
        output.status.success(),
        "complete should succeed for imported trait impl members, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .expect("complete should return valid JSON");
    let names = json
        .as_array()
        .expect("completions should be an array")
        .iter()
        .filter_map(|item| item["name"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    assert!(
        names.contains(&"label".to_string()),
        "completions should still include class fields"
    );
    assert!(
        names.contains(&"name".to_string()),
        "completions should include imported trait methods"
    );
}

#[test]
fn complete_recovers_member_completions_for_dangling_dot_at_eof_stdin_buffers() {
    let source = [
        "class Counter:",
        "    value: int32",
        "",
        "def main() -> int32:",
        "    counter = Counter(value=1)",
        "    counter.",
    ]
    .join("\n");

    let mut child = Command::new(aura_bin())
        .arg("complete")
        .arg("--line")
        .arg("5")
        .arg("--character")
        .arg("12")
        .arg("--trigger")
        .arg(".")
        .arg("--stdin")
        .arg("/virtual/counter.au")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura complete");

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write source");

    let output = child
        .wait_with_output()
        .expect("failed to collect aura complete output");

    assert!(
        output.status.success(),
        "complete should succeed on dangling-dot EOF buffers, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("complete should return valid JSON");
    assert!(
        json.as_array()
            .expect("completions should be an array")
            .iter()
            .any(|item| item["name"].as_str() == Some("value")),
        "dangling-dot EOF completions should still include members"
    );
}

#[test]
fn analyze_recovers_symbols_for_multiple_dangling_dots_with_imports() {
    let temp = TempDir::new("aura-analyze-multi-dangling-imports");
    let helpers_dir = temp.path().join("helpers");
    fs::create_dir_all(&helpers_dir).expect("failed to create helpers dir");
    fs::write(
        helpers_dir.join("math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\n",
    )
    .expect("failed to write helper math module");
    fs::write(
        helpers_dir.join("counter.au"),
        "public class Counter:\n    public value: int32\n",
    )
    .expect("failed to write helper counter module");
    let source_path = temp.path().join("main.au");
    fs::write(
        &source_path,
        "import helpers.math\nfrom helpers.counter import Counter\n\ndef main() -> int32:\n    counter = Counter(value=3)\n    print(helpers.math.\n    print(counter.\n    return 0\n",
    )
    .expect("failed to write main module");

    let output = Command::new(aura_bin())
        .arg("analyze")
        .arg(&source_path)
        .output()
        .expect("failed to run aura analyze");

    assert!(
        output.status.success(),
        "analyze should succeed on recoverable multiple dangling-dot buffers, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .expect("analyze should return valid JSON");
    assert!(
        json["symbols"]
            .as_array()
            .is_some_and(|symbols| !symbols.is_empty()),
        "analyze should still recover symbols for multiple dangling dots"
    );
    assert!(
        json["occurrences"]
            .as_array()
            .is_some_and(|occurrences| !occurrences.is_empty()),
        "analyze should still recover occurrences for multiple dangling dots"
    );
}

#[test]
fn complete_recovers_member_completions_for_multiple_dangling_dots_with_imports() {
    let temp = TempDir::new("aura-complete-multi-dangling-imports");
    let helpers_dir = temp.path().join("helpers");
    fs::create_dir_all(&helpers_dir).expect("failed to create helpers dir");
    fs::write(
        helpers_dir.join("math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\npublic def triple(value: int32) -> int32:\n    return value * 3\n",
    )
    .expect("failed to write helper math module");
    fs::write(
        helpers_dir.join("counter.au"),
        "public class Counter:\n    public value: int32\n",
    )
    .expect("failed to write helper counter module");
    let source_path = temp.path().join("main.au");
    fs::write(
        &source_path,
        "import helpers.math\nfrom helpers.counter import Counter\n\ndef main() -> int32:\n    counter = Counter(value=3)\n    print(helpers.math.\n    print(counter.\n    return 0\n",
    )
    .expect("failed to write main module");

    let output = Command::new(aura_bin())
        .arg("complete")
        .arg("--line")
        .arg("5")
        .arg("--character")
        .arg("23")
        .arg("--trigger")
        .arg(".")
        .arg(&source_path)
        .output()
        .expect("failed to run aura complete");

    assert!(
        output.status.success(),
        "complete should succeed on recoverable multiple dangling-dot buffers, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .expect("complete should return valid JSON");
    let names = json
        .as_array()
        .expect("completions should be an array")
        .iter()
        .filter_map(|item| item["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"double"));
    assert!(names.contains(&"triple"));
}

#[test]
fn build_produces_a_runnable_binary() {
    let fixture = repo_root().join("examples/point.au");
    let output_dir = TempDir::new("aura-build");
    let output_path = output_dir.path().join("point");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg(&fixture)
        .output()
        .expect("failed to run aura build");

    assert!(
        build.status.success(),
        "build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(output_path.exists(), "build should create an output binary");

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run built output");

    assert!(
        run.status.success(),
        "built binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "5.0\n");
}

#[test]
fn build_with_direct_backend_produces_runnable_binary_for_supported_program() {
    let temp = TempDir::new("aura-build-direct");
    let source_path = temp.path().join("main.au");
    fs::write(
        &source_path,
        "def helper(value: int32) -> int32:\n    return value + 2\n\n\
def main() -> int32:\n    mut current: int32 = 1\n    if current < 5:\n        current = helper(value=current)\n    print(current)\n    return 0\n",
    )
    .expect("failed to write direct-backend source");
    let output_path = temp.path().join("direct-main");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build --backend direct");

    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run direct-backend binary");

    assert!(
        run.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "3\n");
}

#[cfg(unix)]
#[test]
fn build_with_direct_backend_flushes_notice_before_waiting_for_concurrent_build() {
    use std::sync::mpsc;

    let temp = TempDir::new("aura-build-concurrent-wait");
    let source_path = temp.path().join("main.au");
    fs::write(&source_path, "def main() -> int32:\n    return 0\n")
        .expect("failed to write concurrent-build source");
    let output_path = temp.path().join("out");

    // Use an isolated runtime target so this real lock-path test neither waits
    // on nor stalls unrelated direct-build tests running in parallel.
    let target_dir = temp.path().join("native-target");
    fs::create_dir_all(&target_dir).expect("native target directory should exist");
    let held_locks = hold_native_runtime_build_locks(&target_dir);

    let mut child = Command::new(aura_bin())
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO", temp.path().join("missing-cargo"))
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura build behind the runtime lock");
    let stderr = child
        .stderr
        .take()
        .expect("concurrent build stderr should be captured");
    let (sender, receiver) = mpsc::channel();
    let stderr_reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut first_line = String::new();
        let result = reader.read_line(&mut first_line);
        let _ = sender.send((result, first_line));
        let mut rest = Vec::new();
        let _ = reader.read_to_end(&mut rest);
        rest
    });

    let observed = receiver.recv_timeout(std::time::Duration::from_secs(10));
    drop(held_locks);
    let barrier_error = match observed {
        Ok((Ok(_), line)) if line == "aura: waiting for a concurrent build...\n" => None,
        Ok((Ok(_), line)) => Some(format!(
            "the first observable line must explain the build wait, found {line:?}"
        )),
        Ok((Err(error), _)) => Some(format!(
            "concurrent build notice should be readable: {error}"
        )),
        Err(error) => Some(format!(
            "aura build should flush a notice before it blocks: {error}"
        )),
    };
    if let Some(error) = barrier_error {
        let _ = child.kill();
        let _ = child.wait();
        let _ = stderr_reader.join();
        panic!("{error}");
    }

    let status =
        wait_with_timeout(&mut child, std::time::Duration::from_secs(30)).unwrap_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("aura build did not finish after releasing the runtime lock")
        });
    let rest = stderr_reader
        .join()
        .expect("concurrent build stderr reader should finish");
    assert!(
        !status.success(),
        "the deliberately unavailable Cargo executable should fail after the lock release"
    );
    assert!(
        String::from_utf8_lossy(&rest).contains("failed to build Aura runtime artifacts"),
        "the build should proceed beyond the released lock to the intended toolchain failure; \
         remaining stderr was:\n{}",
        String::from_utf8_lossy(&rest)
    );
    assert!(
        !output_path.exists(),
        "a failed build must not create output"
    );
}

#[cfg(unix)]
#[test]
fn build_json_buffers_concurrent_wait_notice_into_one_failure_document() {
    let temp = TempDir::new("aura-build-json-concurrent-wait");
    let source_path = temp.path().join("main.au");
    fs::write(&source_path, "def main() -> int32:\n    return 0\n")
        .expect("failed to write JSON concurrent-build source");
    let output_path = temp.path().join("out");

    // Hold the exact isolated runtime-build lock. JSON output is deliberately
    // buffered, so the final structured note is the proof that the child
    // reached this barrier before it was released.
    let target_dir = temp.path().join("native-target");
    fs::create_dir_all(&target_dir).expect("native target directory should exist");
    let held_locks = hold_native_runtime_build_locks(&target_dir);

    let mut child = Command::new(aura_bin())
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO", temp.path().join("missing-cargo"))
        .arg("build")
        .arg("--format")
        .arg("json")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn JSON aura build behind the runtime lock");

    if let Some(status) = wait_with_timeout(&mut child, std::time::Duration::from_secs(3)) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        child
            .stdout
            .take()
            .expect("JSON build stdout should be captured")
            .read_to_end(&mut stdout)
            .expect("JSON build stdout should be readable");
        child
            .stderr
            .take()
            .expect("JSON build stderr should be captured")
            .read_to_end(&mut stderr)
            .expect("JSON build stderr should be readable");
        panic!(
            "JSON aura build completed before the held runtime lock was released \
             (status {status}); stdout was:\n{}stderr was:\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    }

    drop(held_locks);
    let status =
        wait_with_timeout(&mut child, std::time::Duration::from_secs(30)).unwrap_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("JSON aura build did not finish after releasing the runtime lock")
        });
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .stdout
        .take()
        .expect("JSON build stdout should be captured")
        .read_to_end(&mut stdout)
        .expect("JSON build stdout should be readable");
    child
        .stderr
        .take()
        .expect("JSON build stderr should be captured")
        .read_to_end(&mut stderr)
        .expect("JSON build stderr should be readable");

    let output = std::process::Output {
        status,
        stdout,
        stderr,
    };
    assert_eq!(
        output.status.code(),
        Some(1),
        "the unavailable Cargo executable should fail after lock release"
    );
    assert!(
        output.stdout.is_empty(),
        "a failed JSON build should not write stdout"
    );
    let report = parse_single_json_stderr(&output, "JSON build concurrent wait failure");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["diagnostics"].as_array().map(Vec::len), Some(1));
    assert!(
        report["diagnostics"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("failed to build Aura runtime artifacts")),
        "the build should reach the intended post-lock toolchain failure: {report}"
    );
    let notes = report["diagnostics"][0]["notes"]
        .as_array()
        .unwrap_or_else(|| panic!("JSON build failure should contain buffered notes: {report}"));
    assert_eq!(
        notes
            .iter()
            .filter(|note| *note == "aura: waiting for a concurrent build...")
            .count(),
        1,
        "the single JSON document must contain exactly one exact wait notice: {report}"
    );
    assert!(
        !output_path.exists(),
        "a failed build must not create output"
    );
}

#[test]
fn build_with_direct_backend_rejects_unsupported_programs() {
    let fixture = repo_root().join("examples/modules/helpers/math.au");
    let output_dir = TempDir::new("aura-build-direct-unsupported");
    let output_path = output_dir.path().join("helper-module-direct");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&fixture)
        .output()
        .expect("failed to run aura build --backend direct on non-entry module");

    assert!(
        !build.status.success(),
        "direct backend should reject non-entry modules"
    );
    assert!(
        String::from_utf8_lossy(&build.stderr)
            .contains("requires a `main` function or top-level script"),
        "non-entry direct backend errors should explain the missing entrypoint, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
}

#[test]
fn build_rejects_removed_mir_runtime_backend_option() {
    let fixture = repo_root().join("examples/point.au");
    let output_dir = TempDir::new("aura-build-removed-backend");
    let output_path = output_dir.path().join("point-removed-backend");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("mir-runtime")
        .arg("-o")
        .arg(&output_path)
        .arg(&fixture)
        .output()
        .expect("failed to run aura build with removed backend");

    assert!(
        !build.status.success(),
        "removed mir-runtime backend option should fail"
    );
    assert!(
        String::from_utf8_lossy(&build.stderr).contains("usage:")
            || String::from_utf8_lossy(&build.stderr).contains("auto|direct"),
        "removed backend option should report current build usage, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
}

#[test]
fn build_with_direct_backend_supports_point_example() {
    let fixture = repo_root().join("examples/point.au");
    let output_dir = TempDir::new("aura-build-direct-point");
    let output_path = output_dir.path().join("point-direct");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&fixture)
        .output()
        .expect("failed to run aura build --backend direct on point example");

    assert!(
        build.status.success(),
        "direct backend should support point example, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run direct-backend point binary");

    assert!(
        run.status.success(),
        "direct-backend point binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "5.0\n");
}

#[test]
fn build_with_direct_backend_supports_class_methods_example() {
    assert_direct_backend_example_runs(
        "examples/classes/methods.au",
        "methods-direct",
        "4\n8\n0\n",
    );
}

#[test]
fn build_with_direct_backend_supports_string_example() {
    assert_direct_backend_example_runs(
        "examples/strings/greeting.au",
        "greeting-direct",
        "hello, aura\n",
    );
}

#[test]
fn build_with_direct_backend_supports_string_methods_example() {
    assert_direct_backend_example_runs(
        "examples/strings/string_methods.au",
        "string-methods-direct",
        "13\ntrue\ntrue\ntrue\naura repo\n2\naura\nrepo\naura lang\naura repo\nAURA REPO\nrepo\nnone\naura\nnone\n9\n",
    );
}

#[test]
fn build_with_auto_backend_falls_back_for_rich_match_example() {
    let fixture = repo_root().join("examples/enums/rich_match.au");
    let output_dir = TempDir::new("aura-build-auto-rich-match");
    let output_path = output_dir.path().join("rich-match-auto");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("auto")
        .arg("-o")
        .arg(&output_path)
        .arg(&fixture)
        .output()
        .expect("failed to run aura build --backend auto on rich match example");

    assert!(
        build.status.success(),
        "auto backend should succeed for rich match example, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run auto-backend rich match binary");

    assert!(
        run.status.success(),
        "auto-backend rich match binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7\n30\n0\n");
}

#[test]
fn build_with_direct_backend_supports_indexed_member_chains_and_fstring_indexing() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-index-chain-fstring",
        "def main() -> int32:\n    keys = [\"a\", \"b\"]\n    idx: int32 = 1\n    mut counts = {\"key\": 7}\n    match keys.get(idx):\n        case Some(key):\n            print(key)\n        case None:\n            print(\"missing\")\n    print(f\"val: {counts[\"key\"]}\")\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct backend indexed-chain/fstring binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "b\nval: 7\n");
}

#[test]
fn build_with_direct_backend_supports_inferred_enum_match_variants() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-inferred-enum-match",
        "enum Signal:\n    Ready\n    Busy\n\ndef main() -> int32:\n    signal = Signal.Ready\n    match signal:\n        case Ready:\n            print(\"ready\")\n        case Busy:\n            print(\"busy\")\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct backend inferred-enum match binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "ready\n");
}

#[test]
fn build_with_direct_backend_supports_generic_class_field_arithmetic() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-generic-class-fields",
        "class Pair[A]:\n    a: A\n    b: A\n\ndef main() -> int32:\n    pair = Pair[int32](a=3, b=4)\n    inferred = Pair(a=10, b=3)\n    print(pair.a + pair.b)\n    print(inferred.a + inferred.b)\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct backend generic-class field arithmetic should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7\n13\n");
}

#[test]
fn build_with_direct_backend_supports_multi_payload_enum_variants() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-multi-payload-enum",
        "enum Pairing:\n    Pair(int32, int32)\n\ndef main() -> int32:\n    value = Pairing.Pair(2, 3)\n    match value:\n        case Pairing.Pair(a, b):\n            print(a + b)\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct backend multi-payload enum binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "5\n");
}

#[test]
fn check_reports_imported_module_syntax_errors_at_the_imported_file() {
    let temp = TempDir::new("aura-imported-module-syntax");
    let main_path = temp.path().join("main.au");
    let broken_path = temp.path().join("broken.au");
    fs::write(
        &main_path,
        "import broken\n\ndef main() -> int32:\n    return 0\n",
    )
    .expect("failed to write main module");
    fs::write(
        &broken_path,
        "def broken() -> int32:\n    return @@@ syntax error\n",
    )
    .expect("failed to write broken module");

    let output = Command::new(aura_bin())
        .arg("check")
        .arg(&main_path)
        .output()
        .expect("failed to run aura check");

    assert!(
        !output.status.success(),
        "syntax errors in imported modules should fail checking"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&broken_path.display().to_string()),
        "stderr should point at the imported module path, stderr was:\n{}",
        stderr
    );
    assert!(
        stderr.contains("unexpected character `@`"),
        "stderr should preserve the imported parser error, stderr was:\n{}",
        stderr
    );
}

#[test]
fn build_with_direct_backend_supports_numeric_builtins_example() {
    assert_direct_backend_example_runs(
        "examples/numbers/numeric_builtins.au",
        "numeric-builtins-direct",
        "7\n3.5\n2\n12\n9.0\n9.0\n",
    );
}

#[test]
fn build_with_direct_backend_supports_dict_basics_example() {
    assert_direct_backend_example_runs(
        "examples/collections/dict_basics.au",
        "map-basics-direct",
        "3\ntrue\n1\n1\n5\n(aura, 5)\n(repo, 3)\n3\n3\n3\ntrue\n",
    );
}

#[test]
fn build_with_direct_backend_supports_set_basics_example() {
    assert_direct_backend_example_runs(
        "examples/collections/set_basics.au",
        "set-basics-direct",
        "3\ntrue\nfalse\ntrue\ntrue\n9\ntrue\ntrue\n1\n",
    );
}

#[test]
fn build_with_direct_backend_supports_string_parsing_and_formatting_example() {
    assert_direct_backend_example_runs(
        "examples/strings/string_parsing_and_formatting.au",
        "string-parsing-formatting-direct",
        "42\n-9000000000\n3.5\ntrue\naura-lang-tests\ntrue\n12\n4\n9\n3.0\n",
    );
}

#[test]
fn build_with_direct_backend_supports_file_io_example() {
    assert_direct_backend_example_runs(
        "examples/io/read_text_file.au",
        "file-io-direct",
        "true\ntrue\n",
    );
}

#[test]
fn run_and_direct_backends_preserve_false_fs_exists_results() {
    let missing_name = format!(
        "aura-fs-exists-false-{}-{}.missing",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    );
    let missing_path = PathBuf::from(&missing_name);
    assert!(
        !missing_path.exists(),
        "the fs.exists false-result probe must start absent"
    );
    let source = format!(
        "import fs\n\ndef main() -> int32:\n    print(fs.exists(\"{}\"))\n    return 0\n",
        missing_name
    );

    assert_run_and_direct_source_stdout("aura-fs-exists-false", &source, "false\n");
}

#[test]
fn run_and_direct_backends_preserve_the_dynamic_json_surface() {
    let source = include_str!("../../aura-compiler/tests/fixtures/run-pass/json_dynamic_values.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/json_dynamic_values.stdout");

    assert_run_and_direct_source_stdout("aura-dynamic-json-parity", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_vec_algorithm_order_stability_and_ownership() {
    let source = include_str!("../../aura-compiler/tests/fixtures/run-pass/list_algorithms.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/list_algorithms.stdout");

    assert_run_and_direct_source_stdout("aura-vec-algorithms-parity", source, expected);
}

#[test]
fn run_and_direct_backends_transfer_json_task_results_and_clean_up_unobserved_values() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/task_json_result_cleanup.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/task_json_result_cleanup.stdout");

    assert_run_and_direct_source_stdout("aura-json-task-result-parity", source, expected);
}

#[test]
fn run_and_direct_backends_move_deep_fields_without_consuming_siblings() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/deep_projected_move_preserves_siblings.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/deep_projected_move_preserves_siblings.stdout"
    );

    assert_run_and_direct_source_stdout("aura-deep-projected-move-parity", source, expected);
}

#[test]
fn run_and_direct_backends_backtrack_before_moving_match_expression_payloads() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/consuming_nested_noncopy_match_expression.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/consuming_nested_noncopy_match_expression.stdout"
    );

    assert_run_and_direct_source_stdout("aura-consuming-match-expression-parity", source, expected);
}

#[test]
fn run_and_direct_backends_discover_queues_nested_in_task_arguments() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/task_nested_queue_capture_lifecycle.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/task_nested_queue_capture_lifecycle.stdout"
    );

    assert_run_and_direct_source_stdout("aura-nested-task-queue-parity", source, expected);
}

#[test]
fn run_and_direct_backends_transfer_structural_values_across_task_boundaries() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/task_transfer_runtime_matrix.au");
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/task_transfer_runtime_matrix.stdout"
    );

    assert_run_and_direct_source_stdout("aura-task-transfer-matrix", source, expected);
}

#[test]
fn run_and_direct_backends_move_noncopy_try_errors_through_from_conversion() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/try_noncopy_error_conversion.au");
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/try_noncopy_error_conversion.stdout"
    );

    assert_run_and_direct_source_stdout("aura-noncopy-try-from-parity", source, expected);
}

#[test]
fn build_with_direct_backend_supports_bytes_file_io_example() {
    assert_direct_backend_example_runs(
        "examples/io/bytes_file_io.au",
        "bytes-file-io-direct",
        "4\n65\n67\n5\n68\n",
    );
}

#[test]
fn build_with_direct_backend_caps_fs_read_to_string_and_read_bytes() {
    let temp = TempDir::new("aura-direct-file-read-cap");
    let file_path = temp.path().join("huge.txt");
    let file = fs::File::create(&file_path).expect("create oversized file");
    file.set_len((FILESYSTEM_READ_CAP_BYTES + 1) as u64)
        .expect("size oversized file");
    let source_path = temp.path().join("main.au");
    let source = format!(
        "import fs\n\ndef main() -> int32:\n    match fs.read_to_string(\"{path}\"):\n        case Result.Ok(_):\n            print(\"unexpected-string\")\n        case Result.Err(error):\n            print(error)\n    match fs.read_bytes(\"{path}\"):\n        case Result.Ok(_):\n            print(\"unexpected-bytes\")\n        case Result.Err(error):\n            print(error)\n    return 0\n",
        path = file_path.display()
    );
    fs::write(&source_path, source).expect("write Aura source");
    let output_path = temp.path().join("out");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build --backend direct");
    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run direct-backend binary");
    assert!(
        run.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "io.Error.InvalidData\nio.Error.InvalidData\n"
    );
}

#[test]
fn run_caps_fs_read_to_string_and_read_bytes() {
    let temp = TempDir::new("aura-run-file-read-cap");
    let file_path = temp.path().join("huge.txt");
    let file = fs::File::create(&file_path).expect("create oversized file");
    file.set_len((FILESYSTEM_READ_CAP_BYTES + 1) as u64)
        .expect("size oversized file");
    let source_path = temp.path().join("main.au");
    let source = format!(
        "import fs\n\ndef main() -> int32:\n    match fs.read_to_string(\"{path}\"):\n        case Result.Ok(_):\n            print(\"unexpected-string\")\n        case Result.Err(error):\n            print(error)\n    match fs.read_bytes(\"{path}\"):\n        case Result.Ok(_):\n            print(\"unexpected-bytes\")\n        case Result.Err(error):\n            print(error)\n    return 0\n",
        path = file_path.display()
    );
    fs::write(&source_path, source).expect("write Aura source");

    let run = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run aura run");

    assert!(
        run.status.success(),
        "aura run should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "io.Error.InvalidData\nio.Error.InvalidData\n"
    );
}

#[test]
fn run_and_direct_filesystem_read_to_string_accepts_above_retired_cap() {
    let temp = TempDir::new("aura-raised-file-read-cap");
    let file_path = temp.path().join("above-retired-cap.txt");
    let file = fs::File::create(&file_path).expect("create sparse file above retired cap");
    file.set_len((RETIRED_FILESYSTEM_READ_CAP_BYTES + 1) as u64)
        .expect("size sparse file above retired cap");
    let source = format!(
        "import fs\n\ndef main() -> int32:\n    match fs.read_to_string(\"{path}\"):\n        case Result.Ok(text):\n            print(text.byte_len())\n            return 0\n        case Result.Err(error):\n            print(error)\n            return 1\n",
        path = file_path.display()
    );

    assert_run_and_direct_source_stdout("aura-raised-file-read-cap", &source, "67108865\n");
}

#[test]
fn run_and_direct_backend_preserve_match_borrow_mut_writebacks_after_dead_branches() {
    let source = r#"enum Opt:
    Some(int32)
    None

def main() -> int32:
    mut x: Opt = Opt.Some(10)
    match mut x:
        case Some(v):
            v = v + 1
            if false:
                x = Opt.Some(100)
        case None:
            pass
    match x:
        case Some(v):
            print(v)
        case None:
            print(-1)
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-match-borrow-mut-dead-branch-writeback",
        source,
        "11\n",
    );
}

#[test]
fn run_and_direct_backend_preserve_field_match_writeback_across_sibling_mutation() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/match_borrow_mut_field_sibling_write_preserves_writeback.au"
    );
    assert_run_and_direct_source_stdout(
        "aura-match-borrow-mut-field-sibling-writeback",
        source,
        "9\n11\n",
    );
}

#[test]
fn run_and_direct_backends_preserve_int64_defaulting_boundaries_aliases_and_casts() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/default_integer_is_int64.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/default_integer_is_int64.stdout");
    assert_run_and_direct_source_stdout("aura-int64-defaulting", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_integer_call_equality_contexts() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/integer_call_equality.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/integer_call_equality.stdout");
    assert_run_and_direct_source_stdout("aura-integer-call-equality", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_floor_division_and_modulo() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/floor_division_and_modulo.au");
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/floor_division_and_modulo.stdout"
    );
    assert_run_and_direct_source_stdout("aura-floor-division-modulo", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_floor_division_across_integer_widths_and_places() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/floor_division_integer_widths_and_places.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/floor_division_integer_widths_and_places.stdout"
    );
    assert_run_and_direct_source_stdout("aura-floor-division-widths-places", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_integer_to_float_rounding() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/integer_to_float_rounding.au");
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/integer_to_float_rounding.stdout"
    );
    assert_run_and_direct_source_stdout("aura-integer-to-float-rounding", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_integer_to_float_expression_contexts() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/integer_to_float_contexts.au");
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/integer_to_float_contexts.stdout"
    );
    assert_run_and_direct_source_stdout("aura-integer-to-float-contexts", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_float_context_integer_literals() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/float_context_integer_literals.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/float_context_integer_literals.stdout"
    );
    assert_run_and_direct_source_stdout("aura-float-context-integer-literals", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_shortest_roundtrip_float_printing() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/float_shortest_roundtrip_printing.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/float_shortest_roundtrip_printing.stdout"
    );
    assert_run_and_direct_source_stdout("aura-shortest-roundtrip-float-printing", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_the_numbers_example() {
    let source = include_str!("../../../examples/basics/numbers.au");
    assert_run_and_direct_source_stdout(
        "aura-numbers-example",
        source,
        "2\n-3\n2\n-3\n-2\n3.5\n2.0\ntrue\ntrue\n42.0\n9007199254740992.0\n",
    );
}

#[test]
fn run_and_direct_backends_trap_float_floor_division_by_zero() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-fail/float_floor_division_by_zero.au");
    assert_run_and_direct_source_failure_with_timeout(
        "aura-float-floor-division-zero",
        source,
        std::time::Duration::from_secs(30),
        "",
        "division by zero",
    );
}

#[test]
fn run_and_direct_backends_trap_signed_floor_division_overflow() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-fail/int64_division_overflow.au");
    assert_run_and_direct_source_failure_with_timeout(
        "aura-int64-floor-division-overflow",
        source,
        std::time::Duration::from_secs(15),
        "",
        "integer value `9223372036854775808` does not fit in `int64`",
    );
}

#[test]
fn run_and_direct_backends_trap_boxed_int128_floor_division_overflow() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-fail/int128_floor_division_overflow.au"
    );
    assert_run_and_direct_source_failure_with_timeout(
        "aura-int128-floor-division-overflow",
        source,
        std::time::Duration::from_secs(30),
        "0\n",
        "integer value `170141183460469231731687303715884105728` does not fit in `int128`",
    );
}

#[test]
fn run_and_direct_backends_distinguish_exact_cast_from_rounding_conversion() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-fail/int64_to_float64_cast_inexact_boundary.au"
    );
    assert_run_and_direct_source_failure_with_timeout(
        "aura-int64-exact-float-cast-boundary",
        source,
        std::time::Duration::from_secs(15),
        "",
        "integer value `9007199254740993` cannot be represented exactly as `float64`",
    );
}

#[test]
fn run_and_direct_backends_preserve_contextual_int32_literal_inference() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/contextual_int32_literals_remain_int32.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/contextual_int32_literals_remain_int32.stdout"
    );
    assert_run_and_direct_source_stdout("aura-contextual-int32-inference", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_default_integer_generic_dispatch() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/default_integer_generic_dispatch.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/default_integer_generic_dispatch.stdout"
    );
    assert_run_and_direct_source_stdout("aura-default-int64-generic-dispatch", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_generic_numeric_receiver_dispatch() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/generic_numeric_receiver_dispatch.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/generic_numeric_receiver_dispatch.stdout"
    );
    assert_run_and_direct_source_stdout("generic-numeric-receiver-dispatch", source, expected);
}

#[test]
fn direct_backend_releases_owned_projected_receivers_on_every_dynamic_dispatch_path() {
    let source = r#"trait Label:
    def label(self) -> str

impl Label for int32:
    def label(self) -> str:
        return "first"

impl Label for int64:
    def label(self) -> str:
        return "later"

class Box[T: Label]:
    value: T

    def render(self) -> str:
        return self.value.label()

def main() -> int32:
    first: Box[int32] = Box[int32](value=1)
    later: Box[int64] = Box[int64](value=2)
    print(first.render())
    print(later.render())
    return 0
"#;

    let (_, run) =
        build_and_run_direct_source("aura-direct-owned-projected-dynamic-dispatch", source);
    assert!(
        run.status.success(),
        "direct-backend binary should release the projected receiver on every dispatch path, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "first\nlater\n");
}

#[test]
fn run_and_direct_backends_preserve_nested_numeric_generic_dispatch() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/nested_numeric_generic_dispatch.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/nested_numeric_generic_dispatch.stdout"
    );
    assert_run_and_direct_source_stdout("nested-numeric-generic-dispatch", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_try_error_conversion_width() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/try_numeric_error_conversion_width.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/try_numeric_error_conversion_width.stdout"
    );
    assert_run_and_direct_source_stdout("try-numeric-error-conversion-width", source, expected);
}

#[test]
fn run_and_direct_backends_preserve_default_int64_to_uint64_negation_failure() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-fail/uint64_unary_negation_underflow.au"
    );
    assert_run_and_direct_source_failure_with_timeout(
        "aura-default-int64-uint64-negation",
        source,
        std::time::Duration::from_secs(15),
        "",
        "integer value `-1` does not fit in `uint64`",
    );
}

#[test]
fn run_and_direct_backend_match_bare_none_literals_as_option_none() {
    let source = r#"def none_value() -> Option[int32]:
    return None

def main() -> int32:
    a: Option[int32] = None
    match a:
        case Some(value):
            print(value)
        case None:
            print(-1)

    nested: Option[Option[int32]] = Some(None)
    match nested:
        case Some(inner):
            match inner:
                case Some(value):
                    print(value)
                case None:
                    print(-2)
        case None:
            print(-3)

    match none_value():
        case Some(value):
            print(value)
        case None:
            print(-4)

    nested_left: Option[Option[int32]] = Option.Some(None)
    nested_right: Option[Option[int32]] = Option.Some(none_value())
    print(nested_left == nested_right)
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-bare-none-direct-match",
        source,
        "-1\n-2\n-4\ntrue\n",
    );
}

#[test]
fn mir_and_forced_direct_reject_noncopy_internal_exposure() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/check-fail/borrowed_noncopy_return_call.au"
    );
    let (temp, source_path) = write_temp_source("aura-borrowed-return-containment", source);
    let expected = "cannot move non-copy field `name` out of borrowed value `user`";

    let mir = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run forced MIR borrowed-return rejection");
    assert!(!mir.status.success(), "forced MIR should reject the call");
    assert!(
        String::from_utf8_lossy(&mir.stderr).contains(expected),
        "forced MIR diagnostic should explain containment, stderr was:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );

    let direct = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(temp.path().join("out"))
        .arg(&source_path)
        .output()
        .expect("failed to run forced direct borrowed-return rejection");
    assert!(
        !direct.status.success(),
        "forced direct should reject the call before code generation"
    );
    assert!(
        String::from_utf8_lossy(&direct.stderr).contains(expected),
        "forced direct diagnostic should explain containment, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
}

#[test]
fn run_and_direct_backend_preserve_bare_none_in_collection_paths_and_nested_options() {
    let source = r#"class Wrap:
    value: Option[Option[int32]]

def print_opt(value: Option[int32]):
    match value:
        case Some(v):
            print(v)
        case None:
            print(-1)

def print_nested(value: Option[Option[int32]]):
    match value:
        case Some(inner):
            match inner:
                case Some(v):
                    print(v)
                case None:
                    print(-2)
        case None:
            print(-3)

def main() -> int32:
    mut pushed = list[Option[int32]]()
    pushed.append(None)
    print_opt(pushed[0])

    literal: list[Option[int32]] = [None]
    print_opt(literal[0])

    mut values: list[Option[int32]] = [Option.Some(7)]
    print_opt(values.set(index=0, value=None))
    print_opt(values[0])

    mut counts: dict[str, Option[int32]] = {"a": Option.Some(1)}
    counts["a"] = None
    print_opt(counts["a"])

    mut seen: set[Option[int32]] = set[Option[int32]]()
    seen.add(None)
    for value in seen:
        print_opt(value)

    jobs = Queue[Option[int32]]()
    jobs.put(None)
    print_opt(jobs.get_or(Option.Some(99)))

    item = Wrap(value=Option.Some(None))
    print_nested(item.value)
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout(
        "aura-bare-none-collections-and-nested-option",
        source,
        // The generated program is tiny, but the default-parallel CLI suite can
        // delay its process after spawn while other native builds are linking.
        std::time::Duration::from_secs(15),
        "-1\n-1\n7\n-1\n-1\n-1\n-1\n-2\n",
    );
}

#[test]
fn check_rejects_match_borrow_mut_binding_use_after_scrutinee_reassign() {
    let source = "enum Opt:\n    Some(int32)\n    None\n\ndef main() -> int32:\n    mut x: Opt = Opt.Some(10)\n    match mut x:\n        case Some(v):\n            x = Opt.Some(v)\n            v = v + 1\n        case None:\n            pass\n    return 0\n";
    let (_temp, source_path) = write_temp_source("aura-stale-match-binding", source);

    let output = Command::new(aura_bin())
        .arg("check")
        .arg(&source_path)
        .output()
        .expect("failed to run aura check");

    assert!(
        !output.status.success(),
        "stale match-borrow bindings should be rejected"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cannot use pattern binding `v` after reassigning match scrutinee `x`"),
        "expected stale-binding diagnostic, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn check_and_forced_direct_reject_stale_field_match_binding() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/check-fail/match_borrow_mut_field_binding_use_after_scrutinee_reassign.au"
    );
    let (temp, source_path) = write_temp_source("aura-stale-field-match-binding", source);
    let expected =
        "cannot use pattern binding `v` after reassigning match scrutinee `holder.state`";

    let checked = Command::new(aura_bin())
        .arg("check")
        .arg(&source_path)
        .output()
        .expect("failed to run aura check");
    assert!(
        !checked.status.success(),
        "aura check should reject stale field bindings"
    );
    assert!(
        String::from_utf8_lossy(&checked.stderr).contains(expected),
        "expected rooted stale-binding diagnostic, stderr was:\n{}",
        String::from_utf8_lossy(&checked.stderr)
    );

    let direct = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(temp.path().join("out"))
        .arg(&source_path)
        .output()
        .expect("failed to run forced direct build");
    assert!(
        !direct.status.success(),
        "forced direct build should reject stale field bindings before code generation"
    );
    assert!(
        String::from_utf8_lossy(&direct.stderr).contains(expected),
        "forced direct diagnostic should retain the rooted field path, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
}

#[test]
fn check_accepts_module_qualified_builtin_io_error_variants() {
    let source =
        "import io\n\ndef main() -> int32:\n    err: io.Error = io.Error.NotFound\n    return 0\n";
    let (_temp, source_path) = write_temp_source("aura-qualified-io-error", source);

    let output = Command::new(aura_bin())
        .arg("check")
        .arg(&source_path)
        .output()
        .expect("failed to run aura check");

    assert!(
        output.status.success(),
        "qualified io.Error variants should type-check successfully, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn run_and_direct_backend_preserve_builtin_module_enum_identity() {
    let source = r#"import fs
import io

def main() -> int32:
    print(io.Error.NotFound)
    err: io.Error = io.Error.NotFound
    match err:
        case io.Error.NotFound:
            print(1)
        case _:
            print(2)

    other: io.Error = io.Error.Other(message="miss")
    print(other)
    match other:
        case io.Error.Other(message):
            print(message)
        case _:
            print("nope")

    match fs.read_to_string("/definitely/not/here"):
        case Result.Ok(_):
            print(3)
        case Result.Err(error):
            if error == io.Error.NotFound:
                print(4)
            else:
                print(5)
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-builtin-module-enum-identity",
        source,
        "io.Error.NotFound\n1\nio.Error.Other(miss)\nmiss\n4\n",
    );
}

#[test]
fn build_with_direct_backend_supports_tcp_echo_example() {
    assert_direct_backend_example_runs("examples/io/tcp_echo.au", "tcp-echo-direct", "echo:ping\n");
}

#[test]
fn build_with_direct_backend_supports_tcp_bytes_example() {
    assert_direct_backend_example_runs("examples/io/tcp_bytes.au", "tcp-bytes-direct", "4\n116\n");
}

#[test]
fn build_with_direct_backend_supports_udp_echo_example() {
    assert_direct_backend_example_runs(
        "examples/io/udp_echo.au",
        "udp-echo-direct",
        "udp:ping\nping\n",
    );
}

#[test]
fn build_with_direct_backend_supports_http_roundtrip_example() {
    assert_direct_backend_example_runs(
        "examples/io/http_roundtrip.au",
        "http-roundtrip-direct",
        "200\nPOST:/hello:body:ok\n",
    );
}

#[test]
fn build_with_direct_backend_supports_websocket_roundtrip_example() {
    assert_direct_backend_example_runs(
        "examples/io/websocket_roundtrip.au",
        "websocket-roundtrip-direct",
        "ws:hi\n",
    );
}

#[cfg(unix)]
#[test]
fn build_with_direct_backend_supports_unix_and_tls_example() {
    assert_direct_backend_example_runs(
        "examples/io/unix_tls_roundtrip.au",
        "unix-tls-roundtrip-direct",
        "unix:ping\n9\n",
    );
}

#[test]
fn build_with_direct_backend_supports_try_and_result_example() {
    assert_direct_backend_example_runs(
        "examples/error_handling/try_result.au",
        "try-result-direct",
        "6\ndivision by zero\n",
    );
}

#[test]
fn build_with_direct_backend_supports_with_cleanup_example() {
    assert_direct_backend_example_runs(
        "examples/resources/with_resource.au",
        "with-direct",
        "demo\nclosed demo\ndone\n",
    );
}

#[test]
fn build_with_direct_backend_supports_trait_dispatch_example() {
    assert_direct_backend_example_runs(
        "examples/traits/greeter.au",
        "greeter-direct",
        "hello aura\nhello aura\n",
    );
}

#[test]
fn build_with_direct_backend_supports_multi_type_trait_dispatch_example() {
    assert_direct_backend_example_runs(
        "examples/traits/generic_dispatch_multiple_types.au",
        "multi-trait-dispatch-direct",
        "dog\ncat\n",
    );
}

#[test]
fn build_with_direct_backend_supports_generic_trait_impl_example() {
    assert_direct_backend_example_runs(
        "examples/traits/generic_trait_impl.au",
        "generic-trait-impl-direct",
        "11\n",
    );
}

#[test]
fn build_with_direct_backend_prefers_more_specific_trait_impls() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-trait-specificity",
        "trait Show:\n    def show(self) -> str\n\nclass Box[T]:\n    value: T\n\nimpl[T] Show for Box[T]:\n    def show(self) -> str:\n        return \"generic\"\n\nimpl Show for Box[int32]:\n    def show(self) -> str:\n        return \"int32\"\n\ndef main() -> int32:\n    value = Box[int32](value=7)\n    print(value.show())\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct backend trait specialization should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "int32\n");
}

#[test]
fn build_with_direct_backend_supports_generic_trait_bounds_example() {
    assert_direct_backend_example_runs(
        "examples/traits/generic_trait_bounds.au",
        "generic-trait-bounds-direct",
        "20\n",
    );
}

#[test]
fn build_with_direct_backend_supports_operator_traits_example() {
    assert_direct_backend_example_runs(
        "examples/traits/operator_traits.au",
        "operator-traits-direct",
        "6\n8\n-6\n-8\n",
    );
}

#[test]
fn build_with_direct_backend_supports_ordering_traits_example() {
    assert_direct_backend_example_runs(
        "examples/traits/ordering_traits.au",
        "ordering-traits-direct",
        "true\ntrue\ntrue\ntrue\n2\n",
    );
}

#[test]
fn build_with_direct_backend_supports_generic_data_example() {
    assert_direct_backend_example_runs(
        "examples/generics/box_and_wrapper.au",
        "generic-direct",
        "7\nok\n",
    );
}

#[test]
fn build_with_direct_backend_supports_concurrency_example() {
    assert_direct_backend_example_runs(
        "examples/concurrency/task_group_start.au",
        "queues-direct",
        "2\n4\n6\n",
    );
}

#[test]
fn build_with_direct_backend_supports_queue_timeout_example() {
    assert_direct_backend_example_runs(
        "examples/concurrency/queue_timeout.au",
        "queue-timeout-direct",
        "timeout\n",
    );
}

#[test]
fn build_with_direct_backend_supports_borrow_parameters_example() {
    assert_direct_backend_example_runs(
        "examples/basics/borrow_parameters.au",
        "borrow-params-direct",
        "41\n42\n42\n",
    );
}

#[test]
fn build_with_direct_backend_supports_copy_return_selection_example() {
    assert_direct_backend_example_runs(
        "examples/basics/copy_return_selection.au",
        "copy-return-selection-direct",
        "7\n",
    );
}

#[test]
fn build_with_direct_backend_supports_mutating_methods_example() {
    assert_direct_backend_example_runs(
        "examples/classes/mutating_methods.au",
        "mutating-methods-direct",
        "6\n1\n",
    );
}

#[test]
fn build_with_direct_backend_supports_simple_example() {
    assert_direct_backend_example_runs(
        "examples/basics/simple_example.au",
        "simple-example-direct",
        "Ayoola Olafenwa\n834.6\n",
    );
}

#[test]
fn build_with_direct_backend_supports_generic_constructor_specialization_example() {
    assert_direct_backend_example_runs(
        "examples/generics/generic_constructor_specialization.au",
        "generic-specialization-direct",
        "42\n",
    );
}

#[test]
fn build_with_direct_backend_supports_explicit_builtin_enum_type_args_example() {
    assert_direct_backend_example_runs(
        "examples/enums/explicit_type_args.au",
        "explicit-enum-type-args-direct",
        "7\nbad\n",
    );
}

#[test]
fn build_with_direct_backend_supports_float_return_from_enum_match() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-enum-float-match",
        "enum Value:\n    IntVal(int32)\n    FloatVal(float64)\n\ndef to_float(v: Value) -> float64:\n    match v:\n        case Value.IntVal(i):\n            return 0.0\n        case Value.FloatVal(f):\n            return f\n\ndef main() -> int32:\n    value = Value.FloatVal(2.5)\n    print(to_float(value))\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2.5\n");
}

#[test]
fn build_with_direct_backend_supports_namespace_import_types_example() {
    assert_direct_backend_example_runs(
        "examples/modules/namespace_import_types.au",
        "namespace-import-types-direct",
        "4\ntrue\n1\n",
    );
}

#[test]
fn build_with_direct_backend_supports_for_range_example() {
    assert_direct_backend_example_runs(
        "examples/control_flow/for_range.au",
        "for-range-direct",
        "7\n",
    );
}

#[test]
fn build_with_direct_backend_supports_literal_match_example() {
    assert_direct_backend_example_runs(
        "examples/control_flow/match_literals.au",
        "match-literals-direct",
        "negative\nzero\nmany\nyes\nno\nrepo\nother\n",
    );
}

#[test]
fn build_with_direct_backend_supports_list_basics_example() {
    assert_direct_backend_example_runs(
        "examples/collections/list_basics.au",
        "vec-basics-direct",
        "3\n1\n2\n2\n20\n1\n99\nfalse\n",
    );
}

#[test]
fn build_with_direct_backend_supports_list_polish_example() {
    assert_direct_backend_example_runs(
        "examples/collections/list_polish.au",
        "vec-polish-direct",
        "Ada\nGrace\ntrue\n4\n1\n14\n13\n12\n11\ntrue\n100\ntrue\ntrue\n",
    );
}

#[test]
fn build_with_direct_backend_supports_list_iteration_example() {
    assert_direct_backend_example_runs(
        "examples/collections/list_iteration.au",
        "vec-iteration-direct",
        "Ada\nGrace\n2\n9\n",
    );
}

#[test]
fn build_with_direct_backend_supports_full_range_uint128_example() {
    assert_direct_backend_example_runs(
        "examples/numbers/uint128_values.au",
        "uint128-direct",
        "340282366920938463463374607431768211455\n340282366920938463463374607431768211455\n",
    );
}

#[test]
fn build_with_direct_backend_supports_bare_none_unit_values() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-none-unit",
        "def noop() -> None:\n    return None\n\ndef main() -> int32:\n    done: None = None\n    noop()\n    print(1)\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1\n");
}

#[test]
fn build_with_direct_backend_supports_list_literals_and_iteration() {
    let temp = TempDir::new("aura-build-direct-vec");
    let source_path = temp.path().join("main.au");
    fs::write(
        &source_path,
        "def main() -> int32:\n    mut values = [1, 2]\n    values.append(3)\n    mut total = 0\n    for value in values:\n        total += value\n    print(total)\n    return 0\n",
    )
    .expect("failed to write vec source");
    let output_path = temp.path().join("vec-main");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build --backend direct");

    assert!(
        build.status.success(),
        "direct backend vec build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run vec direct-backend binary");

    assert!(
        run.status.success(),
        "vec direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "6\n");
}

#[test]
fn build_with_direct_backend_supports_list_methods_and_constructor() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-list-methods",
        "def print_int_option(value: Option[int32]):\n    match value:\n        case Some(inner):\n            print(inner)\n        case None:\n            print(-1)\n\ndef main() -> int32:\n    values = list[int32]()\n    print(values.is_empty())\n    mut items: list[int32] = [1, 2, 3]\n    print(items.len())\n    print_int_option(items.get(1))\n    print(items.set(index=1, value=20))\n    print(items.pop(0))\n    items.append(99)\n    print(items.pop())\n    mut total: int32 = 0\n    for value in items:\n        total += value\n    print(total)\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "list direct-backend methods binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "true\n3\n2\n2\n1\n99\n23\n"
    );
}

#[test]
fn build_with_direct_backend_supports_string_map_and_numeric_builtins() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-string-map-numbers",
        "def print_int_option(value: Option[int32]):\n    match value:\n        case Some(inner):\n            print(inner)\n        case None:\n            print(-1)\n\ndef main() -> int32:\n    text = \"  aura repo  \"\n    print(text.len())\n    print(text.contains(\"repo\"))\n    print(text.starts_with(\"  au\"))\n    print(text.ends_with(\"  \"))\n    print(text.trim())\n    print(abs(-7))\n    print(min(9, 2))\n    print(max(4, 12))\n    print(sqrt(81.0))\n    mut counts: dict[str, int32] = {\"aura\": 1, \"codex\": 2}\n    print(counts.len())\n    print(\"aura\" in counts)\n    print_int_option(counts.get(\"aura\"))\n    counts[\"aura\"] = 5\n    print(counts[\"aura\"])\n    print(counts.keys().len())\n    print(counts.values().len())\n    print_int_option(counts.remove(\"codex\"))\n    print(counts.is_empty())\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct backend string/map/numbers binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "13\ntrue\ntrue\ntrue\naura repo\n7\n2\n12\n9.0\n2\ntrue\n1\n5\n2\n2\n2\nfalse\n"
    );
}

#[test]
fn string_lengths_and_negative_list_indices_match_run_and_direct_backends() {
    let source = r#"
def print_int_option(value: Option[int32]):
    match value:
        case Some(inner):
            print(inner)
        case None:
            print(-999)

def main() -> int32:
    text = "é🎉é"
    print(text.len())
    print(text.byte_len())

    mut values: list[int32] = [10, 20, 30, 40]
    print(values[-1])
    values[-2] = 35
    print(values[-2])
    print_int_option(values.get(-4))
    print_int_option(values.get(-5))
    print(values.set(index=-4, value=11))
    print(values.pop(-2))
    print(values.swap(first=-1, second=-3))
    print(values.insert(index=-1, value=99))
    end_index: int32 = values.len() as int32
    print(values.insert(index=end_index, value=77))
    for value in values:
        print(value)
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-string-lengths-negative-list-indices",
        source,
        "4\n9\n40\n35\n10\n-999\n10\n35\n\n\n\n40\n20\n99\n11\n77\n",
    );
}

#[test]
fn too_negative_list_index_traps_on_run_and_direct_backends() {
    let source = r#"
def main() -> int32:
    values: list[int32] = [10, 20, 30]
    print(values[-4])
    return 0
"#;

    assert_run_and_direct_source_failure_with_timeout(
        "aura-too-negative-list-index",
        source,
        std::time::Duration::from_secs(20),
        "",
        "list index `-4` is out of bounds for length `3`",
    );
}

#[test]
fn build_with_direct_backend_supports_queue_timeout_matches() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-queue-timeout",
        "def main() -> int32:\n    ch = Queue[int32]()\n    match ch.get(timeout=1ms):\n        case QueueReceive.Item(v):\n            print(v)\n        case QueueReceive.Closed:\n            print(1)\n        case QueueReceive.TimedOut:\n            print(2)\n        case QueueReceive.Cancelled:\n            print(3)\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct-backend queue timeout binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2\n");
}

#[test]
fn built_direct_binaries_render_runtime_errors_with_source_context() {
    let temp = TempDir::new("aura-build-direct-runtime-diag");
    let source_path = temp.path().join("main.au");
    fs::write(
        &source_path,
        "def main() -> int32:\n    print(1 // 0)\n    return 0\n",
    )
    .expect("failed to write runtime-error source");
    let output_path = temp.path().join("out");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build --backend direct");

    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run direct-backend binary");

    assert!(
        !run.status.success(),
        "direct-backend runtime-error binary should fail"
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("error[AU4004]: division by zero"));
    assert!(stderr.contains(&format!("{}:2:11", source_path.display())));
    assert!(stderr.contains("|"));
    assert!(stderr.contains("^"));
}

#[test]
fn default_build_supports_simple_example() {
    assert_default_backend_example_runs(
        "examples/basics/simple_example.au",
        "simple-example-auto",
        "Ayoola Olafenwa\n834.6\n",
    );
}

#[test]
fn default_build_supports_copy_return_selection_example() {
    assert_default_backend_example_runs(
        "examples/basics/copy_return_selection.au",
        "borrowed-lifetime-labels-auto",
        "7\n",
    );
}

#[test]
fn default_build_supports_literal_match_example() {
    assert_default_backend_example_runs(
        "examples/control_flow/match_literals.au",
        "match-literals-auto",
        "negative\nzero\nmany\nyes\nno\nrepo\nother\n",
    );
}

#[test]
fn default_build_supports_list_basics_example() {
    assert_default_backend_example_runs(
        "examples/collections/list_basics.au",
        "vec-basics-auto",
        "3\n1\n2\n2\n20\n1\n99\nfalse\n",
    );
}

#[test]
fn default_build_supports_list_polish_example() {
    assert_default_backend_example_runs(
        "examples/collections/list_polish.au",
        "vec-polish-auto",
        "Ada\nGrace\ntrue\n4\n1\n14\n13\n12\n11\ntrue\n100\ntrue\ntrue\n",
    );
}

#[test]
fn default_build_supports_dict_basics_example() {
    assert_default_backend_example_runs(
        "examples/collections/dict_basics.au",
        "map-basics-auto",
        "3\ntrue\n1\n1\n5\n(aura, 5)\n(repo, 3)\n3\n3\n3\ntrue\n",
    );
}

#[test]
fn default_build_supports_generic_trait_bounds_example() {
    assert_default_backend_example_runs(
        "examples/traits/generic_trait_bounds.au",
        "generic-trait-bounds-auto",
        "20\n",
    );
}

#[test]
fn default_build_supports_operator_traits_example() {
    assert_default_backend_example_runs(
        "examples/traits/operator_traits.au",
        "operator-traits-auto",
        "6\n8\n-6\n-8\n",
    );
}

#[test]
fn default_build_supports_ordering_traits_example() {
    assert_default_backend_example_runs(
        "examples/traits/ordering_traits.au",
        "ordering-traits-auto",
        "true\ntrue\ntrue\ntrue\n2\n",
    );
}

#[test]
fn default_build_supports_set_basics_example() {
    assert_default_backend_example_runs(
        "examples/collections/set_basics.au",
        "set-basics-auto",
        "3\ntrue\nfalse\ntrue\ntrue\n9\ntrue\ntrue\n1\n",
    );
}

#[test]
fn default_build_supports_list_iteration_example() {
    assert_default_backend_example_runs(
        "examples/collections/list_iteration.au",
        "vec-iteration-auto",
        "Ada\nGrace\n2\n9\n",
    );
}

#[test]
fn default_build_supports_generic_constructor_specialization_example() {
    assert_default_backend_example_runs(
        "examples/generics/generic_constructor_specialization.au",
        "generic-specialization-auto",
        "42\n",
    );
}

#[test]
fn default_build_supports_string_methods_example() {
    assert_default_backend_example_runs(
        "examples/strings/string_methods.au",
        "string-methods-auto",
        "13\ntrue\ntrue\ntrue\naura repo\n2\naura\nrepo\naura lang\naura repo\nAURA REPO\nrepo\nnone\naura\nnone\n9\n",
    );
}

#[test]
fn default_build_supports_numeric_builtins_example() {
    assert_default_backend_example_runs(
        "examples/numbers/numeric_builtins.au",
        "numeric-builtins-auto",
        "7\n3.5\n2\n12\n9.0\n9.0\n",
    );
}

#[test]
fn default_build_supports_string_parsing_and_formatting_example() {
    assert_default_backend_example_runs(
        "examples/strings/string_parsing_and_formatting.au",
        "string-parsing-formatting-auto",
        "42\n-9000000000\n3.5\ntrue\naura-lang-tests\ntrue\n12\n4\n9\n3.0\n",
    );
}

#[test]
fn default_build_supports_file_io_example() {
    assert_default_backend_example_runs(
        "examples/io/read_text_file.au",
        "file-io-auto",
        "true\ntrue\n",
    );
}

#[test]
fn default_build_supports_bytes_file_io_example() {
    assert_default_backend_example_runs(
        "examples/io/bytes_file_io.au",
        "bytes-file-io-auto",
        "4\n65\n67\n5\n68\n",
    );
}

#[test]
fn default_build_supports_tcp_echo_example() {
    assert_default_backend_example_runs("examples/io/tcp_echo.au", "tcp-echo-auto", "echo:ping\n");
}

#[test]
fn default_build_supports_tcp_bytes_example() {
    assert_default_backend_example_runs("examples/io/tcp_bytes.au", "tcp-bytes-auto", "4\n116\n");
}

#[test]
fn default_build_supports_udp_echo_example() {
    assert_default_backend_example_runs(
        "examples/io/udp_echo.au",
        "udp-echo-auto",
        "udp:ping\nping\n",
    );
}

#[test]
fn default_build_supports_http_roundtrip_example() {
    assert_default_backend_example_runs(
        "examples/io/http_roundtrip.au",
        "http-roundtrip-auto",
        "200\nPOST:/hello:body:ok\n",
    );
}

#[test]
fn default_build_supports_websocket_roundtrip_example() {
    assert_default_backend_example_runs(
        "examples/io/websocket_roundtrip.au",
        "websocket-roundtrip-auto",
        "ws:hi\n",
    );
}

#[cfg(unix)]
#[test]
fn default_build_supports_unix_and_tls_example() {
    assert_default_backend_example_runs(
        "examples/io/unix_tls_roundtrip.au",
        "unix-tls-roundtrip-auto",
        "unix:ping\n9\n",
    );
}

#[test]
fn default_build_supports_explicit_builtin_enum_type_args_example() {
    assert_default_backend_example_runs(
        "examples/enums/explicit_type_args.au",
        "explicit-enum-type-args-auto",
        "7\nbad\n",
    );
}

#[test]
fn default_build_supports_float_return_from_enum_match() {
    let (_, run) = build_and_run_default_source(
        "aura-build-auto-enum-float-match",
        "enum Value:\n    IntVal(int32)\n    FloatVal(float64)\n\ndef to_float(v: Value) -> float64:\n    match v:\n        case Value.IntVal(i):\n            return 0.0\n        case Value.FloatVal(f):\n            return f\n\ndef main() -> int32:\n    value = Value.FloatVal(2.5)\n    print(to_float(value))\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "default-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2.5\n");
}

#[test]
fn default_build_supports_namespace_import_types_example() {
    assert_default_backend_example_runs(
        "examples/modules/namespace_import_types.au",
        "namespace-import-types-auto",
        "4\ntrue\n1\n",
    );
}

#[test]
fn default_build_supports_generic_trait_impl_example() {
    assert_default_backend_example_runs(
        "examples/traits/generic_trait_impl.au",
        "generic-trait-impl-auto",
        "11\n",
    );
}

#[test]
fn default_build_supports_bare_none_unit_values() {
    let (_, run) = build_and_run_default_source(
        "aura-build-auto-none-unit",
        "def noop() -> None:\n    return None\n\ndef main() -> int32:\n    done: None = None\n    noop()\n    print(1)\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "default-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1\n");
}

#[test]
fn default_build_supports_queue_timeout_matches() {
    let (_, run) = build_and_run_default_source(
        "aura-build-auto-queue-timeout",
        "def main() -> int32:\n    ch = Queue[int32]()\n    match ch.get(timeout=1ms):\n        case QueueReceive.Item(v):\n            print(v)\n        case QueueReceive.Closed:\n            print(1)\n        case QueueReceive.TimedOut:\n            print(2)\n        case QueueReceive.Cancelled:\n            print(3)\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "default-backend queue timeout binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2\n");
}

#[test]
fn built_default_binaries_render_runtime_errors_with_source_context() {
    let temp = TempDir::new("aura-build-auto-runtime-diag");
    let source_path = temp.path().join("main.au");
    fs::write(
        &source_path,
        "def main() -> int32:\n    print(1 // 0)\n    return 0\n",
    )
    .expect("failed to write runtime-error source");
    let output_path = temp.path().join("out");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build");

    assert!(
        build.status.success(),
        "default backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run default-backend binary");

    assert!(
        !run.status.success(),
        "default-backend runtime-error binary should fail"
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("error[AU4004]: division by zero"));
    assert!(stderr.contains(&format!("{}:2:11", source_path.display())));
    assert!(stderr.contains("|"));
    assert!(stderr.contains("^"));
}

#[test]
fn build_with_direct_backend_supports_float_comparisons_in_conditions() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-float-cmp",
        "def main() -> int32:\n    x: float64 = 3.0\n    y: float64 = 3.0\n    if x == y:\n        print(\"equal\")\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "equal\n");
}

#[test]
fn build_with_direct_backend_supports_float_modulo() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-float-mod",
        "def main() -> int32:\n    x: float64 = 10.0\n    y: float64 = 3.0\n    print(x % y)\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1.0\n");
}

#[test]
fn build_with_direct_backend_runs_with_cleanup_on_normal_scope_exit() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-with-normal-exit",
        "class Handle:\n    name: str\n\n    def close(mut self):\n        print(\"closing \" + self.name)\n\ndef main() -> int32:\n    with h = Handle(name=\"db\"):\n        print(\"inside with\")\n    print(\"after with\")\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "inside with\nclosing db\nafter with\n"
    );
}

#[test]
fn build_with_direct_backend_preserves_scalar_return_values_through_with_cleanup() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-with-return",
        "class Handle:\n    name: str\n\n    def close(mut self):\n        print(\"closing \" + self.name)\n\ndef process() -> int32:\n    with h = Handle(name=\"file\"):\n        return 42\n    return 0\n\ndef main() -> int32:\n    print(process())\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "closing file\n42\n");
}

#[test]
fn build_with_direct_backend_prints_boolean_values_as_true_and_false() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-print-bool",
        "def main() -> int32:\n    print(1 == 1)\n    print(1 == 2)\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "true\nfalse\n");
}

#[test]
fn build_with_direct_backend_rejects_narrow_integer_overflow_at_runtime() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-int8-overflow",
        "def main() -> int32:\n    a: int8 = 127\n    b: int8 = 1\n    c = a + b\n    print(c)\n    return 0\n",
    );

    assert!(
        !run.status.success(),
        "direct-backend binary should reject int8 overflow"
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("integer value `128` does not fit in `int8`"),
        "direct-backend overflow should explain the failing int8 value, stderr was:\n{}",
        stderr
    );
}

#[test]
fn build_with_direct_backend_supports_trait_impls_on_builtin_types() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-builtin-trait",
        "trait Show:\n    def show(self) -> str\n\nimpl Show for int32:\n    def show(self) -> str:\n        return \"int\"\n\ndef main() -> int32:\n    value: int32 = 7\n    print(value.show())\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "int\n");
}

#[test]
fn build_with_direct_backend_runs_indirect_recursive_example() {
    assert_direct_backend_example_runs(
        "examples/classes/indirect_recursive.au",
        "indirect-recursive-direct",
        "2\n",
    );
}

#[test]
fn build_runs_indirect_recursive_example() {
    assert_default_backend_example_runs(
        "examples/classes/indirect_recursive.au",
        "indirect-recursive-default",
        "2\n",
    );
}

#[test]
fn build_with_direct_backend_supports_task_result_returning_plain_classes() {
    let (_, run) = build_and_run_direct_source(
        "aura-build-direct-task-result-class",
        "class Box:\n    value: int32\n\ndef make_box() -> Box:\n    return Box(value=7)\n\ndef main() -> int32:\n    with TaskGroup() as group:\n        task = group.start(make_box)\n        match task.result():\n            case TaskResult.Ready(box):\n                print(box.value)\n            case TaskResult.Error(_message):\n                print(0)\n            case TaskResult.TimedOut:\n                print(0)\n            case TaskResult.Cancelled:\n                print(0)\n    return 0\n",
    );

    assert!(
        run.status.success(),
        "direct-backend task result binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7\n");
}

#[test]
fn build_supports_task_result_returning_plain_classes() {
    let (temp, source_path) = write_temp_source(
        "aura-build-default-task-result-class",
        "class Box:\n    value: int32\n\ndef make_box() -> Box:\n    return Box(value=7)\n\ndef main() -> int32:\n    with TaskGroup() as group:\n        task = group.start(make_box)\n        match task.result():\n            case TaskResult.Ready(box):\n                print(box.value)\n            case TaskResult.Error(_message):\n                print(0)\n            case TaskResult.TimedOut:\n                print(0)\n            case TaskResult.Cancelled:\n                print(0)\n    return 0\n",
    );
    let output_path = temp.path().join("out");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build");

    assert!(
        build.status.success(),
        "default build should support task result returning plain classes, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run built binary");

    assert!(
        run.status.success(),
        "built binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7\n");
}

#[test]
fn build_produces_runnable_concurrency_binary() {
    let fixture = repo_root().join("examples/concurrency/task_group_start.au");
    let output_dir = TempDir::new("aura-build-concurrency");
    let output_path = output_dir.path().join("task-group-start");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg(&fixture)
        .output()
        .expect("failed to run aura build for concurrency example");

    assert!(
        build.status.success(),
        "build should succeed for concurrency example, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run built concurrency output");

    assert!(
        run.status.success(),
        "built concurrency binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2\n4\n6\n");
}

#[test]
fn build_from_stdin_produces_runnable_module_binary() {
    let temp = TempDir::new("aura-cli-stdin-build-modules");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/math.au"),
        "public def triple(value: int32) -> int32:\n    return value * 3\n",
    )
    .expect("failed to write helper module");
    let main_path = temp.path().join("main.au");
    let source = "import helpers.math\n\ndef main() -> int32:\n    print(helpers.math.triple(value=5))\n    return 0\n";
    let output_path = temp.path().join("stdin-built-modules");

    let mut child = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg("--stdin")
        .arg(&main_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura build for stdin module program");

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(source.as_bytes())
        .expect("failed to write source");

    let build = child
        .wait_with_output()
        .expect("failed to collect stdin build output");

    assert!(
        build.status.success(),
        "stdin build should succeed for module program, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run built stdin module program");

    assert!(
        run.status.success(),
        "built stdin module binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "15\n");
}

#[test]
fn built_binary_runs_after_source_file_is_removed() {
    let temp = TempDir::new("aura-cli-build-source-removal");
    let source_path = temp.path().join("main.au");
    fs::write(
        &source_path,
        "def main() -> int32:\n    print(value=21 * 2)\n    return 0\n",
    )
    .expect("failed to write source program");
    let output_path = temp.path().join("no-source-needed");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build for source-removal test");

    assert!(
        build.status.success(),
        "build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    fs::remove_file(&source_path).expect("failed to remove source after build");

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run built binary after source removal");

    assert!(
        run.status.success(),
        "built binary should not depend on source files at runtime, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");
}

#[test]
fn built_binary_exits_cleanly_when_stdout_pipe_closes() {
    let fixture = repo_root().join("examples/point.au");
    let output_dir = TempDir::new("aura-build-broken-pipe");
    let output_path = output_dir.path().join("point");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg(&fixture)
        .output()
        .expect("failed to run aura build");

    assert!(
        build.status.success(),
        "build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut child = generated_binary(&output_path)
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn built binary");

    drop(child.stdout.take());

    let status = child
        .wait()
        .expect("failed to wait for built binary after broken pipe");
    assert!(
        status.success(),
        "built binary should exit cleanly when stdout closes early"
    );
}

#[test]
fn run_executes_supported_programs() {
    let fixture = repo_root().join("examples/classes/methods.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run");

    assert!(
        output.status.success(),
        "run should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "4\n8\n0\n");
}

#[test]
fn run_executes_generic_constructor_specialization_example() {
    let fixture = repo_root().join("examples/generics/generic_constructor_specialization.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on generic constructor specialization example");

    assert!(
        output.status.success(),
        "run should succeed for generic constructor specialization example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "42\n");
}

#[test]
fn run_executes_generic_trait_impl_example() {
    let fixture = repo_root().join("examples/traits/generic_trait_impl.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on generic trait impl example");

    assert!(
        output.status.success(),
        "run should succeed for generic trait impl example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "11\n");
}

#[test]
fn run_executes_try_example() {
    let fixture = repo_root().join("examples/error_handling/try_result.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on try example");

    assert!(
        output.status.success(),
        "run should succeed for try example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "6\ndivision by zero\n"
    );
}

#[test]
fn run_executes_with_example() {
    let fixture = repo_root().join("examples/resources/with_resource.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on with example");

    assert!(
        output.status.success(),
        "run should succeed for with example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "demo\nclosed demo\ndone\n"
    );
}

#[test]
fn run_executes_copy_return_selection_example() {
    let fixture = repo_root().join("examples/basics/copy_return_selection.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on copy return selection example");

    assert!(
        output.status.success(),
        "run should succeed for copy return selection example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "7\n");
}

#[test]
fn run_executes_literal_match_example() {
    let fixture = repo_root().join("examples/control_flow/match_literals.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on literal match example");

    assert!(
        output.status.success(),
        "run should succeed for literal match example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "negative\nzero\nmany\nyes\nno\nrepo\nother\n"
    );
}

#[test]
fn run_executes_list_basics_example() {
    let fixture = repo_root().join("examples/collections/list_basics.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on vec basics example");

    assert!(
        output.status.success(),
        "run should succeed for vec basics example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "3\n1\n2\n2\n20\n1\n99\nfalse\n"
    );
}

#[test]
fn run_executes_list_polish_example() {
    let fixture = repo_root().join("examples/collections/list_polish.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on vec polish example");

    assert!(
        output.status.success(),
        "run should succeed for vec polish example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Ada\nGrace\ntrue\n4\n1\n14\n13\n12\n11\ntrue\n100\ntrue\ntrue\n"
    );
}

#[test]
fn run_executes_list_iteration_example() {
    let fixture = repo_root().join("examples/collections/list_iteration.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on vec iteration example");

    assert!(
        output.status.success(),
        "run should succeed for vec iteration example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Ada\nGrace\n2\n9\n"
    );
}

#[test]
fn run_executes_list_literals_and_iteration() {
    let temp = TempDir::new("aura-run-vec");
    let source_path = temp.path().join("main.au");
    fs::write(
        &source_path,
        "def main() -> int32:\n    mut values = [1, 2]\n    values.append(3)\n    mut total = 0\n    for value in values:\n        total += value\n    print(total)\n    return 0\n",
    )
    .expect("failed to write vec source");

    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run aura run");

    assert!(
        output.status.success(),
        "run vec execution should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "6\n");
}

#[test]
fn run_executes_vec_methods_and_constructor() {
    let temp = TempDir::new("aura-run-vec-methods");
    let source_path = temp.path().join("main.au");
    fs::write(
        &source_path,
        "def print_int_option(value: Option[int32]):\n    match value:\n        case Some(inner):\n            print(inner)\n        case None:\n            print(-1)\n\ndef main() -> int32:\n    values = list[int32]()\n    print(values.is_empty())\n    mut items: list[int32] = [1, 2, 3]\n    print(items.len())\n    print_int_option(items.get(1))\n    print(items.set(index=1, value=20))\n    print(items.pop(0))\n    items.append(99)\n    print(items.pop())\n    mut total: int32 = 0\n    for value in items:\n        total += value\n    print(total)\n    return 0\n",
    )
    .expect("failed to write vec methods source");

    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run aura run");

    assert!(
        output.status.success(),
        "run vec methods execution should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "true\n3\n2\n2\n1\n99\n23\n"
    );
}

#[test]
fn run_executes_dict_basics_example() {
    let fixture = repo_root().join("examples/collections/dict_basics.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on map basics example");

    assert!(
        output.status.success(),
        "run should succeed for map basics example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "3\ntrue\n1\n1\n5\n(aura, 5)\n(repo, 3)\n3\n3\n3\ntrue\n"
    );
}

#[test]
fn run_executes_generic_trait_bounds_example() {
    let fixture = repo_root().join("examples/traits/generic_trait_bounds.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on generic trait bounds example");

    assert!(
        output.status.success(),
        "run should succeed for generic trait bounds example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "20\n");
}

#[test]
fn run_executes_operator_traits_example() {
    let fixture = repo_root().join("examples/traits/operator_traits.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on operator traits example");

    assert!(
        output.status.success(),
        "run should succeed for operator traits example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "6\n8\n-6\n-8\n");
}

#[test]
fn run_executes_ordering_traits_example() {
    let fixture = repo_root().join("examples/traits/ordering_traits.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on ordering traits example");

    assert!(
        output.status.success(),
        "run should succeed for ordering traits example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "true\ntrue\ntrue\ntrue\n2\n"
    );
}

#[test]
fn run_executes_set_basics_example() {
    let fixture = repo_root().join("examples/collections/set_basics.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on set basics example");

    assert!(
        output.status.success(),
        "run should succeed for set basics example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "3\ntrue\nfalse\ntrue\ntrue\n9\ntrue\ntrue\n1\n"
    );
}

#[test]
fn run_executes_string_methods_example() {
    let fixture = repo_root().join("examples/strings/string_methods.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on string methods example");

    assert!(
        output.status.success(),
        "run should succeed for string methods example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "13\ntrue\ntrue\ntrue\naura repo\n2\naura\nrepo\naura lang\naura repo\nAURA REPO\nrepo\nnone\naura\nnone\n9\n"
    );
}

#[test]
fn run_executes_numeric_builtins_example() {
    let fixture = repo_root().join("examples/numbers/numeric_builtins.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on numeric builtins example");

    assert!(
        output.status.success(),
        "run should succeed for numeric builtins example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "7\n3.5\n2\n12\n9.0\n9.0\n"
    );
}

#[test]
fn run_executes_string_parsing_and_formatting_example() {
    let fixture = repo_root().join("examples/strings/string_parsing_and_formatting.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on string parsing example");

    assert!(
        output.status.success(),
        "run should succeed for string parsing example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "42\n-9000000000\n3.5\ntrue\naura-lang-tests\ntrue\n12\n4\n9\n3.0\n"
    );
}

#[test]
fn run_executes_file_io_example() {
    let fixture = repo_root().join("examples/io/read_text_file.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on file io example");

    assert!(
        output.status.success(),
        "run should succeed for file io example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "true\ntrue\n");
}

#[test]
fn run_executes_bytes_file_io_example() {
    let fixture = repo_root().join("examples/io/bytes_file_io.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on bytes file io example");

    assert!(
        output.status.success(),
        "run should succeed for bytes file io example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "4\n65\n67\n5\n68\n"
    );
}

#[test]
fn run_executes_tcp_echo_example() {
    let fixture = repo_root().join("examples/io/tcp_echo.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on tcp echo example");

    assert!(
        output.status.success(),
        "run should succeed for tcp echo example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "echo:ping\n");
}

#[test]
fn run_executes_tcp_bytes_example() {
    let fixture = repo_root().join("examples/io/tcp_bytes.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on tcp bytes example");

    assert!(
        output.status.success(),
        "run should succeed for tcp bytes example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "4\n116\n");
}

#[test]
fn run_executes_udp_echo_example() {
    let fixture = repo_root().join("examples/io/udp_echo.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on udp echo example");

    assert!(
        output.status.success(),
        "run should succeed for udp echo example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "udp:ping\nping\n");
}

#[test]
fn run_executes_http_roundtrip_example() {
    let fixture = repo_root().join("examples/io/http_roundtrip.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on http roundtrip example");

    assert!(
        output.status.success(),
        "run should succeed for http roundtrip example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "200\nPOST:/hello:body:ok\n"
    );
}

#[test]
fn run_executes_websocket_roundtrip_example() {
    let fixture = repo_root().join("examples/io/websocket_roundtrip.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on websocket roundtrip example");

    assert!(
        output.status.success(),
        "run should succeed for websocket roundtrip example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ws:hi\n");
}

#[cfg(unix)]
#[test]
fn run_executes_unix_and_tls_roundtrip_example() {
    let fixture = repo_root().join("examples/io/unix_tls_roundtrip.au");
    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to run aura run on unix/tls roundtrip example");

    assert!(
        output.status.success(),
        "run should succeed for unix/tls roundtrip example, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "unix:ping\n9\n");
}

#[test]
fn run_executes_string_map_and_numeric_builtins() {
    let temp = TempDir::new("aura-run-string-map-numbers");
    let source_path = temp.path().join("main.au");
    fs::write(
        &source_path,
        "def print_int_option(value: Option[int32]):\n    match value:\n        case Some(inner):\n            print(inner)\n        case None:\n            print(-1)\n\ndef main() -> int32:\n    text = \"  aura repo  \"\n    print(text.len())\n    print(text.contains(\"repo\"))\n    print(text.starts_with(\"  au\"))\n    print(text.ends_with(\"  \"))\n    print(text.trim())\n    print(abs(-7))\n    print(min(9, 2))\n    print(max(4, 12))\n    print(sqrt(81.0))\n    mut counts: dict[str, int32] = {\"aura\": 1, \"codex\": 2}\n    print(counts.len())\n    print(\"aura\" in counts)\n    print_int_option(counts.get(\"aura\"))\n    counts[\"aura\"] = 5\n    print(counts[\"aura\"])\n    print(counts.keys().len())\n    print(counts.values().len())\n    print_int_option(counts.remove(\"codex\"))\n    print(counts.is_empty())\n    return 0\n",
    )
    .expect("failed to write string/map/numbers source");

    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run aura run");

    assert!(
        output.status.success(),
        "run string/map/numbers execution should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "13\ntrue\ntrue\ntrue\naura repo\n7\n2\n12\n9.0\n2\ntrue\n1\n5\n2\n2\n2\nfalse\n"
    );
}

#[test]
fn run_executes_programs_with_local_modules() {
    let temp = TempDir::new("aura-cli-modules-run");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/math.au"),
        "public def add(left: int32, right: int32) -> int32:\n    return left + right\n",
    )
    .expect("failed to write helper module");
    fs::write(
        temp.path().join("main.au"),
        "from helpers.math import add\n\ndef main() -> int32:\n    print(add(left=3, right=4))\n    return 0\n",
    )
    .expect("failed to write main module");

    let output = Command::new(aura_bin())
        .arg("run")
        .arg(temp.path().join("main.au"))
        .output()
        .expect("failed to run aura on module program");

    assert!(
        output.status.success(),
        "run should succeed for module program, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "7\n");
}

#[test]
fn module_qualified_spawn_target_runs_across_commands() {
    let temp = TempDir::new("aura-cli-qualified-task-start");
    fs::create_dir_all(temp.path().join("pkg")).expect("failed to create module dir");
    fs::write(
        temp.path().join("pkg/helpers.au"),
        "public def work() -> int32:\n    return 1\n",
    )
    .expect("failed to write helper module");
    let source_path = temp.path().join("main.au");
    fs::write(
        &source_path,
        "import pkg.helpers\n\ndef main() -> int32:\n    with TaskGroup() as group:\n        task = group.start(pkg.helpers.work)\n        match task.result():\n            case TaskResult.Ready(value):\n                print(value)\n            case TaskResult.Error(_message):\n                print(0)\n            case TaskResult.TimedOut:\n                print(0)\n            case TaskResult.Cancelled:\n                print(0)\n    return 0\n",
    )
    .expect("failed to write main module");

    let check = Command::new(aura_bin())
        .arg("check")
        .arg(&source_path)
        .output()
        .expect("failed to run aura check");
    assert!(
        check.status.success(),
        "check should accept module-qualified task start targets, stderr was:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );

    for command in ["run"] {
        let output = Command::new(aura_bin())
            .arg(command)
            .arg(&source_path)
            .output()
            .expect("failed to run aura command");

        assert!(
            output.status.success(),
            "{} should execute module-qualified task start targets, stderr was:\n{}",
            command,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout) == "1\n",
            "{} should print the spawned result, stdout was:\n{}",
            command,
            String::from_utf8_lossy(&output.stdout)
        );
    }

    for backend in ["auto", "direct"] {
        let output_path = temp.path().join(format!("out-{backend}"));
        let build = Command::new(aura_bin())
            .arg("build")
            .arg("--backend")
            .arg(backend)
            .arg("-o")
            .arg(&output_path)
            .arg(&source_path)
            .output()
            .expect("failed to run aura build");

        assert!(
            build.status.success(),
            "build --backend {backend} should accept module-qualified task start targets, stderr was:\n{}",
            String::from_utf8_lossy(&build.stderr)
        );

        let run = generated_binary(&output_path)
            .output()
            .expect("failed to run built task binary");
        assert!(
            run.status.success(),
            "built binary for backend {backend} should succeed, stderr was:\n{}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout), "1\n");
    }
}

#[test]
fn run_handles_long_binary_expression_chains_quickly() {
    let terms = (1..=24)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(" + ");
    let source = format!(
        "def main() -> int32:\n    result = {}\n    print(result)\n    return 0\n",
        terms
    );
    let (_temp, source_path) = write_temp_source("aura-cli-long-expr", &source);

    let mut child = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura run on long expression");

    let status = wait_with_timeout(&mut child, std::time::Duration::from_secs(2));
    if status.is_none() {
        child.kill().expect("failed to kill timed out aura run");
    }
    let output = child
        .wait_with_output()
        .expect("failed to collect aura run output for long expression");

    assert!(
        status.is_some(),
        "run should finish quickly for long binary expression chains"
    );
    assert!(
        output.status.success(),
        "run should succeed for long binary expression chains, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "300\n");
}

#[test]
fn build_produces_runnable_binary_for_program_with_local_modules() {
    let temp = TempDir::new("aura-cli-modules-build");
    fs::create_dir_all(temp.path().join("helpers")).expect("failed to create helper dir");
    fs::write(
        temp.path().join("helpers/math.au"),
        "public def double(value: int32) -> int32:\n    return value * 2\n",
    )
    .expect("failed to write helper module");
    fs::write(
        temp.path().join("main.au"),
        "import helpers.math\n\ndef main() -> int32:\n    print(helpers.math.double(value=5))\n    return 0\n",
    )
    .expect("failed to write main module");
    let output_path = temp.path().join("aura-modules");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg(temp.path().join("main.au"))
        .output()
        .expect("failed to build module program");

    assert!(
        build.status.success(),
        "build should succeed for module program, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run built module program");

    assert!(
        run.status.success(),
        "built module binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "10\n");
}

#[test]
fn build_executes_multiple_specialized_trait_impl_dispatch() {
    let source = r#"trait Show:
    def show(self) -> str

class Box[T]:
    value: T

impl Show for Box[int32]:
    def show(self) -> str:
        return f"{self.value}"

impl Show for Box[str]:
    def show(self) -> str:
        return self.value.clone()

def render[T: Show](value: T) -> None:
    print(value.show())

def main() -> int32:
    render(Box[int32](value=7))
    render(Box(value="hi"))
    return 0
"#;
    let (temp, source_path) = write_temp_source("aura-cli-build-specialized-trait-impls", source);
    let output_path = temp.path().join("specialized-trait-impls");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build specialized trait impl program");

    assert!(
        build.status.success(),
        "build should succeed for multiple specialized trait impls, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run built specialized trait impl program");

    assert!(
        run.status.success(),
        "built specialized trait impl binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7\nhi\n");
}

#[test]
fn build_executes_nested_generic_trait_bound_dispatch() {
    let source = r#"trait Add2[Rhs, Out]:
    def add2(self, rhs: own Rhs) -> Out

class Box[T]:
    value: T

impl Add2[int32, int32] for int32:
    def add2(self, rhs: own int32) -> int32:
        return self + rhs

impl[T: Add2[T, T]] Add2[Box[T], Box[T]] for Box[T]:
    def add2(self, rhs: own Box[T]) -> Box[T]:
        return Box(value=self.value.add2(rhs=rhs.value))

def main() -> int32:
    left: Box[int32] = Box(value=3)
    right: Box[int32] = Box(value=4)
    result: Box[int32] = left.add2(rhs=right)
    print(result.value)
    return 0
"#;
    let (temp, source_path) =
        write_temp_source("aura-cli-build-nested-generic-trait-bound-dispatch", source);
    let output_path = temp.path().join("nested-generic-trait-bound-dispatch");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build nested generic trait bound program");

    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run nested generic trait bound program");

    assert!(
        run.status.success(),
        "direct-backend binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7\n");
}

#[test]
fn build_executes_trait_impl_associated_methods() {
    let source = r#"trait Factory:
    def make() -> int32

class Widget:
    value: int32

impl Factory for Widget:
    def make() -> int32:
        return 7

def main() -> int32:
    print(Widget.make())
    return 0
"#;
    let (temp, source_path) = write_temp_source("aura-cli-build-trait-associated-methods", source);
    let output_path = temp.path().join("trait-associated-methods");

    let build = Command::new(aura_bin())
        .arg("build")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build trait impl associated method program");

    assert!(
        build.status.success(),
        "build should succeed for trait impl associated methods, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = generated_binary(&output_path)
        .output()
        .expect("failed to run built trait impl associated method program");

    assert!(
        run.status.success(),
        "built trait impl associated method binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7\n");
}

#[test]
fn direct_backend_build_supports_advanced_io_and_network_surface() {
    let temp = TempDir::new("aura-cli-direct-advanced-io-net");
    let file_path = temp.path().join("data.bin");
    let source = format!(
        r#"import io
import fs
import net

def serve_udp(addresses: Queue[str]) -> Result[str, io.Error]:
    with server_socket = try net.udp_bind("127.0.0.1:0"):
        addresses.put(try server_socket.local_addr())
        match own try server_socket.recv_from(1024, timeout=1s):
            case Option.Some(packet):
                text = try packet.text()
                try server_socket.send_text(packet.address(), "udp:" + text, timeout=1s)
                return Result.Ok(text)
            case Option.None:
                return Result.Ok("missing")

def serve_http(addresses: Queue[str]) -> Result[None, io.Error]:
    with server_listener = try net.http_listen("127.0.0.1:0"):
        addresses.put(try server_listener.local_addr())
        exchange = try server_listener.accept(timeout=1s)
        with request = exchange:
            method = request.method()
            path = request.path()
            body = try request.body_text()
            headers = request.headers()
            match own headers.get("X-Test"):
                case Option.Some(test_header):
                    try request.respond_text(200, method + ":" + path + ":" + body + ":" + test_header, {{"Content-Type": "text/plain"}})
                    return Result.Ok(None)
                case Option.None:
                    try request.respond_text(400, "missing X-Test", {{"Content-Type": "text/plain"}})
                    return Result.Ok(None)

def serve_http_bytes(addresses: Queue[str]) -> Result[None, io.Error]:
    with server_listener = try net.http_listen("127.0.0.1:0"):
        addresses.put(try server_listener.local_addr())
        exchange = try server_listener.accept(timeout=1s)
        with request = exchange:
            body = request.body_bytes()
            try request.respond_bytes(202, body, {{"Content-Type": "application/octet-stream"}})
            return Result.Ok(None)

def serve_ws(addresses: Queue[str]) -> Result[None, io.Error]:
    with server_listener = try net.websocket_listen("127.0.0.1:0"):
        addresses.put(try server_listener.local_addr())
        socket = try server_listener.accept(timeout=1s)
        with server_socket = socket:
            match own try server_socket.recv_text(timeout=1s):
                case Option.Some(text):
                    try server_socket.send_text("ws:" + text, timeout=1s)
                    return Result.Ok(None)
                case Option.None:
                    return Result.Ok(None)

def receive_address(addresses: Queue[str]) -> Result[str, io.Error]:
    match own addresses.get(timeout=1s):
        case QueueReceive.Item(address):
            return Result.Ok(address)
        case QueueReceive.Closed:
            return Result.Err(io.Error.Other(message="address queue closed"))
        case QueueReceive.TimedOut:
            return Result.Err(io.Error.TimedOut)
        case QueueReceive.Cancelled:
            return Result.Err(io.Error.Cancelled)

def run() -> Result[None, io.Error]:
    bytes: list[uint8] = [65 as uint8, 66 as uint8]
    try fs.write_bytes("{path}", bytes)
    try fs.append_bytes("{path}", [67 as uint8, 10 as uint8])
    read_back = try fs.read_bytes("{path}")
    print(read_back.len())
    print(read_back[0])
    print(read_back[2])

    with TaskGroup() as group:
        udp_addresses = Queue[str](capacity=1)
        udp_task = group.start(serve_udp, udp_addresses)
        udp_addr = try receive_address(udp_addresses)
        udp_client = try net.udp_bind("127.0.0.1:0")
        with client_socket = udp_client:
            try client_socket.send_text(udp_addr, "ping", timeout=1s)
            match own try client_socket.recv_from(1024, timeout=1s):
                case Option.Some(packet):
                    print(try packet.text())
                case Option.None:
                    return Result.Ok(None)
        match own udp_task.result():
            case TaskResult.Ready(result):
                match own result:
                    case Result.Ok(text):
                        print(text)
                    case Result.Err(error):
                        return Result.Err(error)
            case TaskResult.Error(_message):
                return Result.Ok(None)
            case TaskResult.Cancelled:
                return Result.Ok(None)
            case TaskResult.TimedOut:
                return Result.Ok(None)

        http_addresses = Queue[str](capacity=1)
        http_task = group.start(serve_http, http_addresses)
        http_addr = try receive_address(http_addresses)
        headers: dict[str, str] = {{"X-Test": "ok"}}
        response = try net.http_request_text("POST", "http://" + http_addr + "/hello", "body", headers.copy())
        with http_response = response:
            print(http_response.status())
            print(try http_response.text())
        match own http_task.result():
            case TaskResult.Ready(result):
                try result
            case TaskResult.Error(_message):
                return Result.Ok(None)
            case TaskResult.Cancelled:
                return Result.Ok(None)
            case TaskResult.TimedOut:
                return Result.Ok(None)

        http_bytes_addresses = Queue[str](capacity=1)
        http_bytes_task = group.start(serve_http_bytes, http_bytes_addresses)
        http_bytes_addr = try receive_address(http_bytes_addresses)
        bytes_response = try net.http_request_bytes("POST", "http://" + http_bytes_addr + "/bytes", [1 as uint8, 2 as uint8], headers)
        with received_bytes = bytes_response:
            print(received_bytes.status())
            print(received_bytes.bytes().len())
        match own http_bytes_task.result():
            case TaskResult.Ready(result):
                try result
            case TaskResult.Error(_message):
                return Result.Ok(None)
            case TaskResult.Cancelled:
                return Result.Ok(None)
            case TaskResult.TimedOut:
                return Result.Ok(None)

        ws_addresses = Queue[str](capacity=1)
        ws_task = group.start(serve_ws, ws_addresses)
        ws_addr = try receive_address(ws_addresses)
        client = try net.websocket_connect_timeout("ws://" + ws_addr + "/", 1s)
        with ws_client = client:
            try ws_client.send_text("hi", timeout=1s)
            match own try ws_client.recv_text(timeout=1s):
                case Option.Some(text):
                    print(text)
                case Option.None:
                    return Result.Ok(None)
        match own ws_task.result():
            case TaskResult.Ready(result):
                try result
            case TaskResult.Error(_message):
                return Result.Ok(None)
            case TaskResult.Cancelled:
                return Result.Ok(None)
            case TaskResult.TimedOut:
                return Result.Ok(None)

    return Result.Ok(None)

def main() -> int32:
    match own run():
        case Result.Ok(_):
            return 0
        case Result.Err(error):
            print(error)
            return 1
"#,
        path = file_path.display()
    );

    let (_build, run) = build_and_run_direct_source("aura-cli-direct-advanced-io-net", &source);
    assert!(
        run.status.success(),
        "direct backend advanced io/network binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "4\n65\n67\nudp:ping\nping\n200\nPOST:/hello:body:ok\n202\n2\nws:hi\n"
    );
}

#[test]
fn run_and_direct_backend_match_unannotated_get_or_none_and_result_or_none() {
    let source = r#"
def worker() -> int32:
    return 7

def main() -> int32:
    jobs = Queue[int32]()
    jobs.put(5)
    queue_opt = jobs.get_or_none()
    match queue_opt:
        case Some(value):
            print(value)
        case None:
            print(-1)

    with TaskGroup() as group:
        task = group.start(worker)
        task_opt = task.result_or_none(timeout=50ms)
        match task_opt:
            case Some(value):
                print(value)
            case None:
                print(-2)
    return 0
"#;

    assert_run_and_direct_source_stdout("aura-unannotated-option-match-lowering", source, "5\n7\n");
}

#[test]
fn run_and_direct_backend_match_bare_none_in_indirect_option_field() {
    let source = r#"
class Node:
    value: int32
    next: indirect Node?

def main() -> int32:
    tail = Node(value=2, next=None)
    match tail.next:
        case Some(next):
            print(next.value)
        case None:
            print(-1)
    return 0
"#;

    assert_run_and_direct_source_stdout("aura-indirect-option-none-match", source, "-1\n");
}

#[test]
fn run_and_direct_backend_allow_match_expression_value_scrutinee_first_use() {
    let source = r#"
class Box:
    value: int32

def take(b: Box) -> int32:
    return b.value

def main() -> int32:
    b = Box(value=5)
    n = match take(b):
        case 1:
            10
        case _:
            20
    print(n)
    return 0
"#;

    assert_run_and_direct_source_stdout("aura-match-expr-value-scrutinee", source, "20\n");
}

#[test]
fn run_preserves_buffered_stdout_on_runtime_error() {
    let source = r#"
def main() -> int32:
    print("first")
    print("second")
    values = [1, 2]
    print(values[99])
    return 0
"#;
    let (_temp, source_path) = write_temp_source("aura-run-buffered-stdout-error", source);

    let output = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run aura run on buffered stdout error source");

    assert!(
        !output.status.success(),
        "run should fail for the runtime error source"
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "first\nsecond\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("out of bounds"),
        "runtime error should mention the out-of-bounds access, stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn with_task_group_joins_start_soon_before_scope_exit() {
    let source = r#"
def producer(jobs: Queue[int32]) -> None:
    sleep(20ms)
    jobs.put(9)

def main() -> int32:
    jobs = Queue[int32]()
    with TaskGroup() as group:
        group.start_soon(producer, jobs)
        print("scope")
    print(jobs.get_or(-1))
    return 0
"#;

    assert_run_and_direct_source_stdout("aura-task-group-start-soon-join", source, "scope\n9\n");
}

#[test]
fn task_group_stack_overrides_preserve_bounds_argument_order_and_backend_parity() {
    let source = r#"
def stack_bytes() -> int64:
    print("stack")
    return 262144

def argument(label: str, value: int32) -> int32:
    print(label)
    return value

def publish_sum(values: Queue[int32], left: int32, right: int32) -> None:
    values.put(left + right)

def publish(values: Queue[int32], value: int32) -> None:
    values.put(value)

def main() -> int32:
    values = Queue[int32]()
    with TaskGroup() as group:
        group.start_with_stack(
            stack_bytes(),
            publish_sum,
            values,
            right=argument("right", 2),
            left=argument("left", 1)
        )
        group.start_soon_with_stack(67108864, publish, values, 9)
    first = values.get_or(-1)
    second = values.get_or(-1)
    if first < second:
        print(first)
        print(second)
    else:
        print(second)
        print(first)
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-task-group-stack-override",
        source,
        "stack\nright\nleft\n3\n9\n",
    );
}

#[test]
fn task_group_stack_overrides_reject_dynamic_out_of_range_values_on_both_backends() {
    let source = r#"
def stack_bytes() -> int64:
    return 0

def work() -> int32:
    return 1

def main() -> int32:
    with TaskGroup() as group:
        group.start_with_stack(stack_bytes(), work)
    return 0
"#;

    assert_run_and_direct_source_failure_with_timeout(
        "aura-task-group-stack-override-bounds",
        source,
        std::time::Duration::from_secs(20),
        "",
        "task stack size must be between 262144 and 67108864 bytes, found 0",
    );
}

#[test]
fn dynamic_json_dumps_depth_limit_fits_forced_512_kib_tasks_on_both_backends() {
    let source = r#"
import json

def dump_at_depth_limit() -> None:
    mut array_source = "null"
    mut object_source = "null"
    mut depth: int32 = 0
    while depth < 128:
        array_source = "[" + array_source + "]"
        object_source = "{\"x\":" + object_source + "}"
        depth += 1
    match json.parse(array_source):
        case Result.Ok(value):
            print(value)
            print(json.dumps(value).len())
            print(f"{value}".len())
        case Result.Err(error):
            print(error)
    match json.parse(object_source):
        case Result.Ok(value):
            print(value)
            print(json.dumps(value).len())
            print(f"{value}".len())
        case Result.Err(error):
            print(error)
    match json.parse("[" + array_source + "]"):
        case Result.Ok(value):
            print(value)
        case Result.Err(json.Error.NestingTooDeep(limit, line, column)):
            print(limit)
            print(line)
            print(column)
        case Result.Err(error):
            print(error)

def main() -> int32:
    with TaskGroup() as group:
        group.start_soon_with_stack(524288, dump_at_depth_limit)
    return 0
"#;

    let mut array_render = "json.Value.Null".to_string();
    let mut object_render = "json.Value.Null".to_string();
    for _ in 0..128 {
        array_render = format!("json.Value.Array([{array_render}])");
        object_render = format!("json.Value.Object({{x: {object_render}}})");
    }
    let expected = format!(
        "{array_render}\n260\n{}\n{object_render}\n772\n{}\n128\n1\n129\n",
        array_render.len(),
        object_render.len()
    );
    assert_run_and_direct_source_stdout("aura-json-dumps-small-stack", source, &expected);
}

#[test]
fn task_results_surface_errors_without_aborting_the_program() {
    let source = r#"
def bad() -> int32:
    values: list[int32] = [1, 2]
    return values[7]

def main() -> int32:
    with TaskGroup() as group:
        task = group.start(bad)
        match task.result(timeout=100ms):
            case TaskResult.Ready(value):
                print(value)
            case TaskResult.Error(message):
                print(message.contains("out of bounds"))
            case TaskResult.TimedOut:
                print(false)
            case TaskResult.Cancelled:
                print(false)

        print(task.result_or(-1))

        maybe = task.result_or_none(timeout=100ms)
        match maybe:
            case Some(value):
                print(value)
            case None:
                print(-1)
    print("after")
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-task-result-error-surface",
        source,
        "true\n-1\n-1\nafter\n",
    );
}

#[test]
fn unread_task_failures_abort_task_group_scope() {
    let source = r#"
def boom() -> int32:
    values: list[int32] = [1, 2]
    return values[7]

def main() -> int32:
    print("before")
    with TaskGroup() as group:
        group.start(boom)
    print("after")
    return 0
"#;
    let (temp, source_path) = write_temp_source("aura-task-group-unread-failure", source);

    let run = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run aura run on unread task failure source");
    assert!(
        !run.status.success(),
        "run should fail when a task group scope exits with an unread task failure"
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "before\n");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("out of bounds"),
        "run stderr should surface the unread task failure, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build unread task failure source");
    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let direct = generated_binary(&output_path)
        .output()
        .expect("failed to run direct unread task failure binary");
    assert!(
        !direct.status.success(),
        "direct binary should fail when a task group scope exits with an unread task failure"
    );
    assert_eq!(String::from_utf8_lossy(&direct.stdout), "before\n");
    assert!(
        String::from_utf8_lossy(&direct.stderr).contains("out of bounds"),
        "direct stderr should surface the unread task failure, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
}

#[test]
fn cancelled_yields_for_cpu_bound_lightweight_tasks() {
    let source = r#"
def worker() -> int32:
    mut n: int32 = 0
    while n < 1000000:
        if cancelled():
            return 9999
        n += 1
    return n

def main() -> int32:
    with TaskGroup() as group:
        task = group.start(worker)
        sleep(1ms)
        group.cancel()
        match task.result(timeout=10s):
            case TaskResult.Ready(value):
                print(value)
            case TaskResult.Error(_message):
                print(-1)
            case TaskResult.TimedOut:
                print(-2)
            case TaskResult.Cancelled:
                print(-3)
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        "aura-cancelled-yields",
        source,
        std::time::Duration::from_secs(20),
        "9999\n",
        Some(1),
    );
}

#[test]
fn loop_backedge_safepoints_prevent_timer_and_queue_starvation() {
    let hot_loop_millis = hosted_ci_timing_millis(100);
    let max_latency_millis = hosted_ci_timing_millis(50);
    let source = format!(
        r#"
import sys

def sleep_then_report(progress: Queue[int64]) -> None:
    started_at = sys.monotonic_time_ms()
    sleep(10ms)
    progress.put(sys.monotonic_time_ms() - started_at)

def run_hot_loop() -> None:
    started_at = sys.monotonic_time_ms()
    while true:
        if sys.monotonic_time_ms() - started_at >= {hot_loop_millis}:
            return

def report_queue_progress(started_at: int64, progress: Queue[int64]) -> None:
    progress.put(sys.monotonic_time_ms() - started_at)

def main() -> int32:
    timer_progress = Queue[int64]()
    queue_progress = Queue[int64]()
    started_at = sys.monotonic_time_ms()

    with TaskGroup() as group:
        group.start(sleep_then_report, timer_progress)
        group.start(run_hot_loop)
        group.start(report_queue_progress, started_at, queue_progress)

        queue_latency = queue_progress.get_or(-1, timeout=1s)
        timer_latency = timer_progress.get_or(-1, timeout=1s)
        print(queue_latency >= 0 and queue_latency <= {max_latency_millis})
        print(timer_latency >= 10 and timer_latency <= {max_latency_millis})
    return 0
"#
    );

    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        "aura-loop-backedge-safepoints",
        &source,
        std::time::Duration::from_secs(10),
        "true\ntrue\n",
        Some(1),
    );
}

#[test]
fn loop_backedge_safepoints_prevent_socket_readiness_starvation() {
    let hot_loop_millis = hosted_ci_timing_millis(200);
    let max_latency_millis = hosted_ci_timing_millis(100);
    let source = format!(
        r#"
import io
import net
import sys

def accept_then_report(
    addresses: Queue[str],
    progress: Queue[int64]
) -> Result[None, io.Error]:
    with server_listener = try net.listen("127.0.0.1:0"):
        addresses.put(try server_listener.local_addr())
        started_at = sys.monotonic_time_ms()
        match own server_listener.accept(timeout=1s):
            case Result.Ok(stream):
                with accepted_stream = stream:
                    progress.put(sys.monotonic_time_ms() - started_at)
            case Result.Err(_):
                progress.put(-1)
    return Result.Ok(None)

def connect_after_delay(address: str) -> None:
    sleep(10ms)
    match own net.connect_timeout(address, 1s):
        case Result.Ok(stream):
            with client_stream = stream:
                pass
        case Result.Err(_):
            pass

def run_hot_loop() -> None:
    started_at = sys.monotonic_time_ms()
    while true:
        if sys.monotonic_time_ms() - started_at >= {hot_loop_millis}:
            return

def socket_probe() -> Result[bool, io.Error]:
    addresses = Queue[str](capacity=1)
    socket_progress = Queue[int64]()

    with TaskGroup() as group:
        group.start(accept_then_report, addresses, socket_progress)
        match own addresses.get(timeout=1s):
            case QueueReceive.Item(address):
                group.start(connect_after_delay, address)
                group.start(run_hot_loop)

                socket_latency = socket_progress.get_or(-1, timeout=1s)
                return Result.Ok(socket_latency >= 10 and socket_latency <= {max_latency_millis})
            case QueueReceive.Closed:
                return Result.Ok(false)
            case QueueReceive.TimedOut:
                return Result.Ok(false)
            case QueueReceive.Cancelled:
                return Result.Ok(false)

def main() -> int32:
    match socket_probe():
        case Result.Ok(progressed):
            print(progressed)
        case Result.Err(_):
            print(false)
    return 0
"#
    );

    // Readiness must arrive in the first half of the 200 ms hot loop. Without
    // a cooperative backedge, the accept task cannot resume until the loop ends.
    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        "aura-loop-backedge-socket-safepoints",
        &source,
        std::time::Duration::from_secs(10),
        "true\n",
        Some(1),
    );
}

#[test]
fn self_receiver_method_result_can_bind_to_a_name() {
    let source = r#"
class Box:
    value: str

    def take(own self) -> str:
        return self.value

def main() -> int32:
    b = Box(value="held")
    x = b.take()
    print(x)
    return 0
"#;

    let (_temp, source_path) = write_temp_source("aura-value-receiver-binding", source);
    let check = Command::new(aura_bin())
        .arg("check")
        .arg(&source_path)
        .output()
        .expect("failed to run aura check on value receiver binding source");

    assert!(
        check.status.success(),
        "check should accept binding a value-receiver result, stderr was:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );

    assert_run_and_direct_source_stdout("aura-value-receiver-binding", source, "held\n");
}

#[test]
fn list_insert_clamps_out_of_bounds_indices_on_run_and_direct_backends() {
    let source = r#"
def main() -> int32:
    mut values = [1, 2, 3]
    values.insert(index=99, value=7)
    values.insert(index=-99, value=0)
    values.insert(index=5, value=8)
    values.insert(index=-1, value=6)
    for value in values:
        print(value)
    return 0
"#;
    assert_run_and_direct_source_stdout(
        "aura-list-insert-clamping",
        source,
        "0\n1\n2\n3\n7\n6\n8\n",
    );
}

#[test]
fn vec_set_out_of_bounds_is_a_runtime_error() {
    let source = r#"
def main() -> int32:
    mut values = [1, 2, 3]
    print("before")
    print(values.set(index=99, value=7))
    print("after")
    return 0
"#;
    let (temp, source_path) = write_temp_source("aura-vec-set-oob", source);

    let run = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run aura run on vec set source");
    assert!(!run.status.success(), "run should fail for vec set OOB");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "before\n");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("out of bounds"),
        "run stderr should mention the out-of-bounds set, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build direct vec set source");
    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let direct = generated_binary(&output_path)
        .output()
        .expect("failed to run direct vec set binary");
    assert!(
        !direct.status.success(),
        "direct binary should fail for vec set OOB"
    );
    assert_eq!(String::from_utf8_lossy(&direct.stdout), "before\n");
    assert!(
        String::from_utf8_lossy(&direct.stderr).contains("out of bounds"),
        "direct stderr should mention the out-of-bounds set, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
}

#[test]
fn vec_remove_out_of_bounds_is_a_runtime_error() {
    let source = r#"
def main() -> int32:
    mut values = [1, 2, 3]
    print("before")
    print(values.pop(index=99))
    print("after")
    return 0
"#;
    let (temp, source_path) = write_temp_source("aura-vec-remove-oob", source);

    let run = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run aura run on vec remove source");
    assert!(!run.status.success(), "run should fail for vec remove OOB");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "before\n");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("out of bounds"),
        "run stderr should mention the out-of-bounds remove, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build direct vec remove source");
    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let direct = generated_binary(&output_path)
        .output()
        .expect("failed to run direct vec remove binary");
    assert!(
        !direct.status.success(),
        "direct binary should fail for vec remove OOB"
    );
    assert_eq!(String::from_utf8_lossy(&direct.stdout), "before\n");
    assert!(
        String::from_utf8_lossy(&direct.stderr).contains("out of bounds"),
        "direct stderr should mention the out-of-bounds remove, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
}

#[test]
fn vec_swap_out_of_bounds_is_a_runtime_error() {
    let source = r#"
def main() -> int32:
    mut values = [1, 2, 3]
    print("before")
    print(values.swap(first=0, second=99))
    print("after")
    return 0
"#;
    let (temp, source_path) = write_temp_source("aura-vec-swap-oob", source);

    let run = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run aura run on vec swap source");
    assert!(!run.status.success(), "run should fail for vec swap OOB");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "before\n");
    assert!(
        String::from_utf8_lossy(&run.stderr)
            .contains("list swap indices `0` and `99` are out of bounds for length `3`"),
        "run stderr should mention both out-of-bounds swap indices, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build direct vec swap source");
    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let direct = generated_binary(&output_path)
        .output()
        .expect("failed to run direct vec swap binary");
    assert!(
        !direct.status.success(),
        "direct binary should fail for vec swap OOB"
    );
    assert_eq!(String::from_utf8_lossy(&direct.stdout), "before\n");
    assert!(
        String::from_utf8_lossy(&direct.stderr)
            .contains("list swap indices `0` and `99` are out of bounds for length `3`"),
        "direct stderr should mention both out-of-bounds swap indices, stderr was:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
}

#[test]
fn queue_iteration_exits_when_task_group_is_cancelled() {
    let source = r#"
def worker(q: Queue[int32]):
    sleep(60m)

def main() -> int32:
    q: Queue[int32] = Queue[int32]()
    with TaskGroup() as g:
        g.start_soon(worker, q)
        sleep(50ms)
        print("about to cancel")
        g.cancel()
        print("about to iterate")
        for v in q:
            print(v)
        print("loop done")
    print("scope done")
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout(
        "aura-queue-iteration-cancel",
        source,
        std::time::Duration::from_secs(30),
        "about to cancel\nabout to iterate\nloop done\nscope done\n",
    );
}

#[test]
fn queue_iteration_exits_when_a_sibling_task_fails() {
    let source = r#"
def producer(q: Queue[int32]):
    q.put(1)
    values = [1]
    _ = values[99]

def main() -> int32:
    print("before")
    q: Queue[int32] = Queue[int32]()
    with TaskGroup() as g:
        g.start(producer, q)
        for v in q:
            pass
    print("after")
    return 0
"#;

    assert_run_and_direct_source_failure_with_timeout(
        "aura-queue-iteration-sibling-failure",
        source,
        std::time::Duration::from_secs(30),
        "before\n",
        "out of bounds",
    );
}

#[test]
fn queue_iteration_exits_when_task_group_producers_return_cleanly() {
    let source = r#"
def producer(q: Queue[int32]):
    q.put(1)
    q.put(2)

def main() -> int32:
    q: Queue[int32] = Queue[int32]()
    with TaskGroup() as g:
        g.start_soon(producer, q)
        for v in q:
            print(v)
        print("loop done")
    print("scope done")
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout(
        "aura-queue-iteration-clean-return",
        source,
        std::time::Duration::from_secs(30),
        "1\n2\nloop done\nscope done\n",
    );
}

#[test]
fn direct_backend_unwinds_with_resources_before_runtime_trap() {
    let source = r#"
class Resource:
    name: str

    def close(mut self):
        print("close " + self.name)

def main() -> int32:
    with a = Resource(name="A"):
        with b = Resource(name="B"):
            values: list[int32] = []
            print(values[5])
    return 0
"#;

    let (_, run) = build_and_run_direct_source("aura-direct-with-trap-cleanup", source);
    assert!(
        !run.status.success(),
        "direct binary should fail on list OOB"
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "close B\nclose A\n");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("list index `5` is out of bounds"),
        "stderr should include list OOB diagnostic, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn direct_backend_unwinds_with_resources_when_callee_traps() {
    let source = r#"
class Resource:
    name: str

    def close(mut self):
        print("close " + self.name)

def boom() -> int32:
    values: list[int32] = []
    return values[5]

def main() -> int32:
    with a = Resource(name="A"):
        with b = Resource(name="B"):
            with c = Resource(name="C"):
                return boom()
    return 0
"#;

    let (_, run) = build_and_run_direct_source("aura-direct-with-callee-trap-cleanup", source);
    assert!(
        !run.status.success(),
        "direct binary should fail on list OOB"
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "close C\nclose B\nclose A\n"
    );
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("list index `5` is out of bounds"),
        "stderr should include list OOB diagnostic, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn direct_backend_callee_trap_cleanup_uses_current_resource_state() {
    let source = r#"
class Resource:
    name: str

    def close(mut self):
        print("close " + self.name)

def boom() -> int32:
    values: list[int32] = []
    return values[5]

def main() -> int32:
    with resource = Resource(name="old"):
        resource.name = "new"
        return boom()
    return 0
"#;

    let (_, run) = build_and_run_direct_source("aura-direct-with-current-cleanup", source);
    assert!(
        !run.status.success(),
        "direct binary should fail on list OOB"
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "close new\n");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("list index `5` is out of bounds"),
        "stderr should include list OOB diagnostic, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn direct_backend_preserves_body_trap_when_cleanup_also_traps() {
    let source = r#"
class Resource:
    name: str

    def close(mut self):
        print("close " + self.name)
        print(1 // 0)

def boom() -> int32:
    print("body")
    return 1 // 0

def main() -> int32:
    with resource = Resource(name="A"):
        return boom()
    return 0
"#;

    let (_, run) = build_and_run_direct_source("aura-direct-primary-trap-diagnostic", source);
    assert!(
        !run.status.success(),
        "direct binary should fail when the body traps"
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "body\nclose A\n");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("return 1 // 0"),
        "direct backend should report the primary body trap, stderr was:\n{}",
        stderr
    );
    assert!(
        !stderr.contains("print(1 // 0)"),
        "cleanup trap should not replace the primary body trap, stderr was:\n{}",
        stderr
    );
}

#[test]
fn direct_backend_recursion_limit_uses_source_diagnostic() {
    let source = r#"
def recurse(value: int32) -> int32:
    return recurse(value + 1)

def main() -> int32:
    return recurse(0)
"#;

    let (_, run) = build_and_run_direct_source("aura-direct-recursion-diagnostic", source);
    assert!(
        !run.status.success(),
        "direct binary should fail on recursion limit"
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("maximum call depth") && stderr.contains("while calling `recurse`"),
        "stderr should describe the Aura recursion limit, stderr was:\n{}",
        stderr
    );
    assert!(
        stderr.contains("-->") && !stderr.contains("direct backend"),
        "stderr should render with source context and avoid backend-specific wording, stderr was:\n{}",
        stderr
    );
}

#[test]
fn direct_backend_recursion_with_with_frames_matches_run_cleanup_count() {
    let source = r#"
class Resource:
    def close(mut self):
        print("CLOSE_REC")

def recurse(value: int32) -> int32:
    with resource = Resource():
        return recurse(value + 1)

def main() -> int32:
    return recurse(0)
"#;

    let (temp, source_path) = write_temp_source("aura-recursion-with-cleanup-count", source);
    let run = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run aura run");
    assert!(!run.status.success(), "aura run should fail on recursion");

    let run_stdout = String::from_utf8_lossy(&run.stdout);
    let run_close_count = run_stdout
        .lines()
        .filter(|line| *line == "CLOSE_REC")
        .count();
    assert_eq!(
        run_close_count, 254,
        "aura run should preserve the established cleanup count"
    );

    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .arg("build")
        .arg("--backend")
        .arg("direct")
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to run aura build --backend direct");
    assert!(
        build.status.success(),
        "direct backend build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let direct = generated_binary(&output_path)
        .output()
        .expect("failed to run direct binary");
    assert!(
        !direct.status.success(),
        "direct binary should fail on recursion"
    );
    let direct_stdout = String::from_utf8_lossy(&direct.stdout);
    let direct_close_count = direct_stdout
        .lines()
        .filter(|line| *line == "CLOSE_REC")
        .count();
    assert_eq!(
        direct_close_count, run_close_count,
        "direct backend should unwind the same number of with frames as aura run"
    );
}

#[test]
fn direct_backend_unwinds_with_resources_before_recursion_limit() {
    let source = r#"
class Resource:
    name: str

    def close(mut self):
        print("close " + self.name)

def recurse(value: int32) -> int32:
    return recurse(value + 1)

def main() -> int32:
    with resource = Resource(name="A"):
        return recurse(0)
    return 0
"#;

    let (_, run) = build_and_run_direct_source("aura-direct-recursion-cleanup", source);
    assert!(
        !run.status.success(),
        "direct binary should fail on recursion limit"
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "close A\n");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("maximum call depth"),
        "stderr should describe the Aura recursion limit, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn run_flushes_stdout_before_sigkill() {
    let source = r#"
def main() -> int32:
    print("before")
    while true:
        sleep(1s)
    return 0
"#;

    let (temp, source_path) = write_temp_source("aura-run-sigkill-flush", source);
    let mut child = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aura run");

    std::thread::sleep(std::time::Duration::from_millis(300));
    child.kill().expect("failed to kill hung aura run");
    let output = child
        .wait_with_output()
        .expect("failed to collect killed aura run output");
    drop(temp);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("before\n"),
        "aura run should flush stdout as prints happen, stdout was:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn process_completed_exposes_binary_stdout_bytes_in_run_and_direct_backend() {
    let source = r#"import process

def run_binary_stdout() -> Result[None, process.Error]:
    completed = try process.run(["/usr/bin/env", "python3", "-c", "import sys; sys.stdout.buffer.write(bytes([255, 0, 65]))"], stdout=process.pipe(), stderr=process.pipe(), timeout=2s, group=true)
    bytes = completed.stdout_bytes()
    print(bytes.len())
    print(bytes[0])
    print(bytes[1])
    print(bytes[2])
    return Result.Ok(None)

def main() -> int32:
    match run_binary_stdout():
        case Result.Ok(_):
            return 0
        case Result.Err(error):
            print(error)
            return 1
"#;

    assert_run_and_direct_source_stdout(
        "aura-process-completed-stdout-bytes",
        source,
        "3\n255\n0\n65\n",
    );
}

#[test]
fn process_completed_stdout_bytes_get_matches_short_option_patterns() {
    let source = r#"import process

def inspect_first_byte() -> Result[None, process.Error]:
    completed = try process.run(["/bin/echo", "hi"], stdout=process.pipe(), stderr=process.pipe(), timeout=2s, group=true)
    opt = completed.stdout_bytes().get(0)
    match opt:
        case Some(byte):
            print("some")
            print(byte)
        case None:
            print("none")
    return Result.Ok(None)

def main() -> int32:
    match inspect_first_byte():
        case Result.Ok(_):
            return 0
        case Result.Err(error):
            print(error)
            return 1
"#;

    assert_run_and_direct_source_stdout(
        "aura-process-stdout-bytes-short-option-match",
        source,
        "some\n104\n",
    );
}

#[test]
fn retrying_network_worker_runs_with_computed_backoff_on_both_backends() {
    let example = repo_root().join("examples/agents/retrying_network_worker.au");
    let source = fs::read_to_string(&example)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", example.display()));

    let expected = concat!(
        "recover request 1\n",
        "recover retry 4ms\n",
        "recover request 2\n",
        "recover result 200\n",
        "rate request 1\n",
        "rate retry 6ms\n",
        "rate request 2\n",
        "rate result 429\n",
        "exhaust request 1\n",
        "exhaust retry 3ms\n",
        "exhaust request 2\n",
        "exhaust retry 5ms\n",
        "exhaust request 3\n",
        "exhaust result 503\n",
        "requests 7\n",
    );

    assert_run_and_direct_source_stdout_with_timeout(
        "aura-retrying-network-worker",
        &source,
        std::time::Duration::from_secs(20),
        expected,
    );

    let retry_body = source
        .split_once("def request_with_retry")
        .and_then(|(_, rest)| rest.split_once("\ndef work"))
        .map(|(body, _)| body)
        .expect("example should define request_with_retry before work");

    let marker = |needle: &str| {
        retry_body
            .find(needle)
            .unwrap_or_else(|| panic!("retry worker should contain `{needle}`"))
    };
    let retryable_guard = marker("if status != 503:");
    let final_attempt_guard = marker("if attempt == max_attempts:");
    let jitter = marker("jitter = rng.next_int(0, 4) * 1ms");
    let delay = marker("delay = backoff + jitter");
    let retry_log = marker("print(f\"{name} retry {delay}\")");
    let sleep = marker("sleep(delay)");
    let double = marker("backoff = backoff * 2");

    assert!(
        retryable_guard < final_attempt_guard
            && final_attempt_guard < jitter
            && jitter < delay
            && delay < retry_log
            && retry_log < sleep
            && sleep < double,
        "the final-attempt guard must precede jitter, logging, sleep, and doubling"
    );
    assert_eq!(retry_body.matches("rng.next_int(0, 4)").count(), 1);
    assert_eq!(retry_body.matches("sleep(delay)").count(), 1);
}

#[test]
fn queue_iteration_without_registered_producers_exits() {
    let source = r#"
def main() -> int32:
    jobs: Queue[int32] = Queue[int32]()
    for job in jobs:
        print(job)
    print("done")
    return 0
"#;

    // No producer exists in this semantic pin, so extra scheduler workers
    // cannot affect the result. Keep one worker to avoid making the 30-second
    // deadlock watchdog measure host-wide CLI-process oversubscription.
    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        "aura-queue-iteration-zero-producers",
        source,
        std::time::Duration::from_secs(30),
        "done\n",
        Some(1),
    );
}

#[test]
fn queue_iteration_waits_for_standalone_task_group_producers() {
    let source = r#"
def producer(jobs: Queue[int32]) -> None:
    sleep(1ms)
    jobs.put(7)
    jobs.close()

def main() -> int32:
    jobs: Queue[int32] = Queue[int32]()
    group = TaskGroup()
    group.start(producer, jobs)
    for job in jobs:
        print(job)
    print("done")
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout(
        "aura-queue-iteration-standalone-task-group",
        source,
        std::time::Duration::from_secs(30),
        "7\ndone\n",
    );
}

#[test]
fn wait_any_without_tasks_times_out_immediately() {
    let source = r#"def main() -> int32:
    tasks = list[Task[int32]]()
    match wait_any(tasks):
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
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout(
        "aura-wait-any-empty",
        source,
        std::time::Duration::from_secs(15),
        "timedout\n",
    );
}

#[test]
fn queue_get_or_without_timeout_returns_default_immediately() {
    let source = r#"def main() -> int32:
    jobs = Queue[int32]()
    print("before")
    print(jobs.get_or(7))
    print("after")
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout_and_workers(
        "aura-queue-get-or-no-timeout",
        source,
        std::time::Duration::from_secs(15),
        "before\n7\nafter\n",
        Some(1),
    );
}

#[test]
fn task_result_or_without_timeout_returns_fallback_immediately() {
    let source = r#"def slow() -> int32:
    sleep(100ms)
    return 5

def main() -> int32:
    with TaskGroup() as group:
        task = group.start(slow)
        print(task.result_or(-1))
        match task.result_or_none():
            case Some(value):
                print(value)
            case None:
                print(-2)
    return 0
"#;

    assert_run_and_direct_source_stdout_with_timeout(
        "aura-task-result-or-no-timeout",
        source,
        std::time::Duration::from_secs(15),
        "-1\n-2\n",
    );
}

#[test]
fn fs_write_bytes_accepts_empty_lists_in_run_and_direct_backend() {
    let source = r#"import fs

def main() -> int32:
    match fs.write_bytes("/tmp/aura-empty-bytes.bin", []):
        case Result.Ok(_):
            print("ok")
        case Result.Err(error):
            print(error)
    return 0
"#;

    assert_run_and_direct_source_stdout("aura-fs-write-empty-bytes", source, "ok\n");
}

#[test]
fn direct_backend_build_supports_process_module_surface() {
    let temp = TempDir::new("aura-cli-direct-process");
    let cwd = fs::canonicalize(temp.path())
        .expect("temp path should canonicalize")
        .display()
        .to_string();
    let source = format!(
        r#"import process

def run(cwd: own str) -> Result[None, process.Error]:
    env: dict[str, str] = {{"AURA_PROCESS_VAR": "present"}}
    completed = try process.run(["/usr/bin/printenv", "AURA_PROCESS_VAR"], env=env, timeout=2s, group=true)
    print(completed.stdout().trim())
    print(completed.stderr().len())
    pwd = try process.run(["/bin/pwd"], cwd=Option.Some(cwd), timeout=2s, group=true)
    print(pwd.stdout().trim())
    print(pwd.stderr().len())
    print(completed.status())
    print(pwd.status())

    with child = try process.start(["/bin/cat"], stdin=process.pipe(), stdout=process.pipe(), stderr=process.null(), group=true):
        match child.stdin():
            case Option.Some(found_pipe):
                stdin_pipe: process.Pipe = found_pipe
                try stdin_pipe.write_all("echo from cat\n", timeout=500ms)
                try stdin_pipe.flush()
                stdin_pipe.close()
            case Option.None:
                return Result.Ok(None)

        match child.stdout():
            case Option.Some(found_pipe):
                stdout_pipe: process.Pipe = found_pipe
                match try stdout_pipe.read_line(timeout=500ms):
                    case Option.Some(text):
                        print(text)
                    case Option.None:
                        return Result.Ok(None)
            case Option.None:
                return Result.Ok(None)

        print(child.wait(timeout=2s))
    with supervisor = process.supervisor():
        try supervisor.start(name="flaky", command=["/usr/bin/false"], restart=process.RestartPolicy.OnFailure, backoff=10ms, max_restarts=1, group=true)
        print(try supervisor.wait_or_none(timeout=500ms))
        print(try supervisor.wait_or_none(timeout=500ms))
        print(supervisor.is_empty())
        try supervisor.start(name="sleeper", command=["/bin/sleep", "1"], restart=process.RestartPolicy.Never, group=true)
        print(supervisor.is_empty())
        try supervisor.stop()
        print(supervisor.is_empty())
    return Result.Ok(None)

def main() -> int32:
    match run("{cwd}"):
        case Result.Ok(_):
            return 0
        case Result.Err(error):
            print(error)
            return 1
"#,
        cwd = cwd,
    );

    let (_build, run) = build_and_run_direct_source("aura-cli-direct-process", &source);
    assert!(
        run.status.success(),
        "direct backend process binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        format!(
            "present\n0\n{cwd}\n0\nExitStatus.Exited(0)\nExitStatus.Exited(0)\necho from cat\nWait.Exited(ExitStatus.Exited(0))\nOption.Some(SupervisorEvent.Restarted(flaky, ExitStatus.Exited(1), 1))\nOption.Some(SupervisorEvent.Exited(flaky, ExitStatus.Exited(1), 1))\ntrue\nfalse\ntrue\n",
            cwd = cwd,
        )
    );
}

#[cfg(unix)]
#[test]
fn direct_backend_build_supports_unix_and_tls_network_surface() {
    let temp = TempDir::new("aura-cli-direct-unix-tls");
    let unix_path = PathBuf::from(format!(
        "/tmp/aura-cli-{}-{}.sock",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    ));

    let certificate = generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("should generate self-signed certificate");
    let cert_pem = certificate.cert.pem();
    let key_pem = certificate.key_pair.serialize_pem();
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    fs::write(&cert_path, cert_pem).expect("should write cert pem");
    fs::write(&key_path, key_pem).expect("should write key pem");

    let source = format!(
        r#"import io
import net

def serve_unix(path: own str, ready: Queue[bool]) -> Result[None, io.Error]:
    with server_listener = try net.unix_listen(path):
        ready.put(true)
        stream = try server_listener.accept(timeout=1s)
        with server_stream = stream:
            match own try server_stream.read_line(timeout=1s):
                case Option.Some(text):
                    try server_stream.write_all("unix:" + text, timeout=1s)
                    return Result.Ok(None)
                case Option.None:
                    return Result.Ok(None)

def serve_tls(cert_path: own str, key_path: own str, addresses: Queue[str]) -> Result[None, io.Error]:
    with server_listener = try net.tls_listen("127.0.0.1:0", cert_path, key_path):
        addresses.put(try server_listener.local_addr())
        stream = try server_listener.accept(timeout=2s)
        with server_stream = stream:
            match own try server_stream.read_line(timeout=2s):
                case Option.Some(text):
                    try server_stream.write_all("tls:" + text + "\n", timeout=2s)
                    return Result.Ok(None)
                case Option.None:
                    return Result.Ok(None)

def run() -> Result[None, io.Error]:
    with TaskGroup() as group:
        unix_ready = Queue[bool](capacity=1)
        unix_task = group.start(serve_unix, "{unix_path}", unix_ready)
        match unix_ready.get(timeout=1s):
            case QueueReceive.Item(_):
                client = try net.unix_connect_timeout("{unix_path}", 1s)
                with unix_client = client:
                    try unix_client.write_all("ping\n", timeout=1s)
                    match own try unix_client.read_line(timeout=1s):
                        case Option.Some(text):
                            print(text)
                        case Option.None:
                            return Result.Ok(None)
            case QueueReceive.Closed:
                return Result.Err(io.Error.Other(message="Unix readiness queue closed"))
            case QueueReceive.TimedOut:
                return Result.Err(io.Error.TimedOut)
            case QueueReceive.Cancelled:
                return Result.Err(io.Error.Cancelled)
        match own unix_task.result():
            case TaskResult.Ready(result):
                try result
            case TaskResult.Error(_message):
                return Result.Ok(None)
            case TaskResult.Cancelled:
                return Result.Ok(None)
            case TaskResult.TimedOut:
                return Result.Ok(None)

        tls_addresses = Queue[str](capacity=1)
        tls_task = group.start(serve_tls, "{cert_path}", "{key_path}", tls_addresses)
        match tls_addresses.get(timeout=2s):
            case QueueReceive.Item(tls_addr):
                stream = try net.tls_connect_timeout(tls_addr, "localhost", "{cert_path}", 2s)
                with tls_client = stream:
                    try tls_client.write_all("ping!\n", timeout=2s)
                    match own try tls_client.read_line(timeout=2s):
                        case Option.Some(text):
                            print(text)
                        case Option.None:
                            return Result.Ok(None)
            case QueueReceive.Closed:
                return Result.Err(io.Error.Other(message="TLS address queue closed"))
            case QueueReceive.TimedOut:
                return Result.Err(io.Error.TimedOut)
            case QueueReceive.Cancelled:
                return Result.Err(io.Error.Cancelled)
        match own tls_task.result():
            case TaskResult.Ready(result):
                try result
            case TaskResult.Error(_message):
                return Result.Ok(None)
            case TaskResult.Cancelled:
                return Result.Ok(None)
            case TaskResult.TimedOut:
                return Result.Ok(None)

    return Result.Ok(None)

def main() -> int32:
    match own run():
        case Result.Ok(_):
            return 0
        case Result.Err(error):
            print(error)
            return 1
"#,
        unix_path = unix_path.display(),
        cert_path = cert_path.display(),
        key_path = key_path.display()
    );

    let (_build, run) = build_and_run_direct_source("aura-cli-direct-unix-tls", &source);
    let _ = fs::remove_file(&unix_path);
    assert!(
        run.status.success(),
        "direct backend unix/tls binary should exit successfully, stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "unix:ping\ntls:ping!\n"
    );
}

#[test]
fn zero_sized_udp_reads_return_typed_invalid_input_in_mir_and_direct_backends() {
    let source = r#"import io
import net

def probe() -> Result[None, io.Error]:
    with socket = try net.udp_bind("127.0.0.1:0"):
        print(socket.recv(0, timeout=1ms))
        print(socket.recv_from(0, timeout=1ms))
    return Result.Ok(None)

def main() -> int32:
    match probe():
        case Result.Ok(_):
            return 0
        case Result.Err(error):
            print(error)
            return 1
"#;

    assert_run_and_direct_source_stdout(
        "aura-zero-sized-udp-read",
        source,
        "Result.Err(io.Error.InvalidInput)\nResult.Err(io.Error.InvalidInput)\n",
    );
}

#[test]
fn direct_backend_metrics_int64_overflow_fails_at_runtime() {
    let source = r#"import metrics

def main() -> int32:
    metrics.reset()
    metrics.increment("requests", 9223372036854775807)
    metrics.increment("requests", 1)
    print(metrics.get("requests"))
    return 0
"#;

    let (_build, run) = build_and_run_direct_source("aura-direct-metrics-overflow", source);
    assert!(!run.status.success(), "metrics overflow should fail");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("metric value overflowed `int64`"),
        "unexpected direct-backend metrics diagnostic: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        run.stdout.is_empty(),
        "overflow should stop before metrics.get"
    );
}

#[test]
fn run_and_direct_backend_match_d6_parameter_loop_and_task_defaults() {
    let source = r#"class Message:
    text: str

def read(message: Message) -> int32:
    return message.text.len() as int32

def consume(value: own str):
    print(value)

def task_read(value: str) -> int32:
    return value.len() as int32

def main() -> int32:
    message = Message(text="shared")
    print(read(message))
    print(message.text)

    names = ["Ada", "Grace"]
    for name in names:
        print(name)
    print(names.len())

    owned = ["moved"]
    for value in own owned:
        consume(value)

    captured = "capture"
    with TaskGroup() as group:
        task = group.start(task_read, captured)
        match task.result():
            case TaskResult.Ready(value):
                print(value)
            case TaskResult.Error(_message):
                print(-1)
            case TaskResult.Cancelled:
                print(-2)
            case TaskResult.TimedOut:
                print(-3)
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-d6-defaults",
        source,
        "6\nshared\nAda\nGrace\n2\nmoved\n7\n",
    );
}

#[test]
fn check_and_direct_backend_preserve_d6_own_parameter_guidance() {
    let source = r#"def take(value: str) -> str:
    return value
"#;
    let (temp, source_path) = write_temp_source("aura-d6-own-guidance", source);
    let expected = "parameter `value` is borrowed; declare it as `own str` to take ownership, or clone the value before consuming it";

    let checked = Command::new(aura_bin())
        .arg("check")
        .arg(&source_path)
        .output()
        .expect("failed to run D6 ownership check");
    assert!(
        !checked.status.success(),
        "borrowed parameter move should fail"
    );
    assert!(
        String::from_utf8_lossy(&checked.stderr).contains(expected),
        "unexpected D6 check diagnostic:\n{}",
        String::from_utf8_lossy(&checked.stderr)
    );

    let direct = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(temp.path().join("out"))
        .arg(&source_path)
        .output()
        .expect("failed to run forced-direct D6 ownership check");
    assert!(
        !direct.status.success(),
        "forced direct should reject a borrowed parameter move before code generation"
    );
    assert!(
        String::from_utf8_lossy(&direct.stderr).contains(expected),
        "unexpected forced-direct D6 diagnostic:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
}

#[test]
fn check_and_direct_backend_reject_queue_iteration_capability_modifiers() {
    let expected = "Queue iteration receives values; each received item is already owned by the loop binding, and the Queue handle is a copy value, so ownership modifiers have nothing to modify; use the bare form `for item in queue:`";

    for (name, modifier) in [("own", "own "), ("mut", "mut ")] {
        let source = format!(
            "def main() -> int32:\n    queue = Queue[int64]()\n    for item in {modifier}queue:\n        print(item)\n    return 0\n"
        );
        let (temp, source_path) =
            write_temp_source(&format!("aura-queue-capability-{name}"), &source);

        let checked = Command::new(aura_bin())
            .arg("check")
            .arg(&source_path)
            .output()
            .expect("failed to check a Queue iteration modifier");
        assert!(
            !checked.status.success(),
            "Queue iteration modifier `{name}` should fail"
        );
        assert!(
            String::from_utf8_lossy(&checked.stderr).contains(expected),
            "unexpected Queue `{name}` diagnostic:\n{}",
            String::from_utf8_lossy(&checked.stderr)
        );

        let direct = Command::new(aura_bin())
            .args(["build", "--backend", "direct", "-o"])
            .arg(temp.path().join("out"))
            .arg(&source_path)
            .output()
            .expect("failed to run forced-direct Queue modifier check");
        assert!(
            !direct.status.success(),
            "forced direct should reject Queue iteration modifier `{name}`"
        );
        assert!(
            String::from_utf8_lossy(&direct.stderr).contains(expected),
            "unexpected forced-direct Queue `{name}` diagnostic:\n{}",
            String::from_utf8_lossy(&direct.stderr)
        );
    }
}

#[test]
fn check_and_direct_backend_reject_range_iteration_capability_modifiers() {
    let expected = "Range iteration yields copy `int64` values, so ownership modifiers have nothing to modify or transfer; use the bare form `for item in range(...):`";

    for (name, modifier) in [("own", "own "), ("mut", "mut ")] {
        let source = format!(
            "def main() -> int32:\n    for item in {modifier}range(0, 3):\n        print(item)\n    return 0\n"
        );
        let (temp, source_path) =
            write_temp_source(&format!("aura-range-capability-{name}"), &source);

        let checked = Command::new(aura_bin())
            .arg("check")
            .arg(&source_path)
            .output()
            .expect("failed to check a Range iteration modifier");
        assert!(
            !checked.status.success(),
            "Range iteration modifier `{name}` should fail"
        );
        assert!(
            String::from_utf8_lossy(&checked.stderr).contains(expected),
            "unexpected Range `{name}` diagnostic:\n{}",
            String::from_utf8_lossy(&checked.stderr)
        );

        let direct = Command::new(aura_bin())
            .args(["build", "--backend", "direct", "-o"])
            .arg(temp.path().join("out"))
            .arg(&source_path)
            .output()
            .expect("failed to run forced-direct Range modifier check");
        assert!(
            !direct.status.success(),
            "forced direct should reject Range iteration modifier `{name}`"
        );
        assert!(
            String::from_utf8_lossy(&direct.stderr).contains(expected),
            "unexpected forced-direct Range `{name}` diagnostic:\n{}",
            String::from_utf8_lossy(&direct.stderr)
        );
    }
}

fn run_and_direct_failure_outputs(prefix: &str, source: &str) -> [std::process::Output; 2] {
    let (temp, source_path) = write_temp_source(prefix, source);
    let run = Command::new(aura_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("failed to run assertion source");
    assert!(
        !run.status.success(),
        "aura run should fail for assertion source"
    );

    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build assertion source with the direct backend");
    assert!(
        build.status.success(),
        "direct assertion build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let direct = generated_binary(&output_path)
        .output()
        .expect("failed to run direct assertion binary");
    assert!(
        !direct.status.success(),
        "direct assertion binary should fail"
    );

    [run, direct]
}

#[test]
fn assertions_preserve_exact_messages_in_run_and_direct_backends() {
    for (name, suffix, expected_first_line) in [
        ("default", "", "error[AU4001]: assertion failed"),
        (
            "custom",
            ", \"custom assertion\"",
            "error[AU4001]: custom assertion",
        ),
        ("empty", ", \"\"", "error[AU4001]: "),
        ("whitespace", ", \"   \"", "error[AU4001]:    "),
    ] {
        let source = format!("def main():\n    assert false{suffix}\n");
        for output in
            run_and_direct_failure_outputs(&format!("aura-assert-message-{name}"), &source)
        {
            assert!(output.stdout.is_empty(), "{name} should not print");
            assert_eq!(
                String::from_utf8_lossy(&output.stderr).lines().next(),
                Some(expected_first_line),
                "{name} assertion message must be preserved exactly"
            );
        }
    }
}

#[test]
fn assertion_introspection_is_once_only_and_byte_identical_across_backends() {
    let source = r#"def left() -> int64:
    print("left")
    return 41

def right() -> int64:
    print("right")
    return 42

def message() -> str:
    print("message")
    return "numbers differ"

def main():
    assert left() == right(), message()
"#;

    let [mir, direct] = run_and_direct_failure_outputs("aura-assert-introspection-parity", source);
    for output in [&mir, &direct] {
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "left\nright\nmessage\n"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("error[AU4001]: numbers differ"), "{stderr}");
        assert!(stderr.contains("left = 41"), "{stderr}");
        assert!(stderr.contains("right = 42"), "{stderr}");
    }
    assert_eq!(mir.stderr, direct.stderr);
}

#[test]
fn assertions_evaluate_condition_once_and_message_only_on_failure() {
    let passing = r#"def lazy_message() -> str:
    print("unexpected message")
    return "unused"

def main():
    print("before")
    assert true, lazy_message()
    print("after")
"#;
    assert_run_and_direct_source_stdout(
        "aura-assert-lazy-passing-message",
        passing,
        "before\nafter\n",
    );

    let failing = r#"class Probe:
    condition_calls: int32
    message_calls: int32

    def condition(mut self) -> bool:
        self.condition_calls += 1
        print(f"condition {self.condition_calls}")
        return false

    def message(mut self) -> str:
        self.message_calls += 1
        print(f"message {self.message_calls}")
        return "evaluated once"

def main():
    mut probe = Probe(condition_calls=0, message_calls=0)
    assert probe.condition(), probe.message()
"#;
    for output in run_and_direct_failure_outputs("aura-assert-order", failing) {
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "condition 1\nmessage 1\n"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).lines().next(),
            Some("error[AU4001]: evaluated once")
        );
    }
}

#[test]
fn assertion_operand_traps_precede_assertion_failure() {
    let condition_trap = r#"def condition() -> bool:
    print("condition")
    values: list[bool] = [true]
    return values[5]

def message() -> str:
    print("message")
    return "assertion should not run"

def main():
    assert condition(), message()
"#;
    for output in run_and_direct_failure_outputs("aura-assert-condition-trap", condition_trap) {
        assert_eq!(String::from_utf8_lossy(&output.stdout), "condition\n");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("list index `5` is out of bounds"));
        assert!(!stderr.contains("assertion should not run"));
    }

    let message_trap = r#"def message() -> str:
    print("message")
    values: list[int32] = [1]
    print(values[5])
    return "assertion should not run"

def main():
    print("condition")
    assert false, message()
"#;
    for output in run_and_direct_failure_outputs("aura-assert-message-trap", message_trap) {
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "condition\nmessage\n"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("list index `5` is out of bounds"));
        assert!(!stderr.contains("assertion failed"));
    }
}

#[test]
fn assertion_failure_remains_primary_when_cleanup_also_traps() {
    let source = r#"class Resource:
    def close(mut self):
        print("close")
        print(1 // 0)

def main():
    with resource = Resource():
        print("body")
        assert false, "body assertion"
"#;

    for output in run_and_direct_failure_outputs("aura-assert-cleanup-primary", source) {
        assert_eq!(String::from_utf8_lossy(&output.stdout), "body\nclose\n");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).lines().next(),
            Some("error[AU4001]: body assertion")
        );
    }
}

#[test]
fn aura_test_discovers_test_functions_and_keeps_main_files_working() {
    let temp = TempDir::new("aura-test-discovery");
    let tests = temp.path().join("tests");
    fs::create_dir_all(&tests).expect("test directory should create");

    fs::write(
        tests.join("functions.au"),
        "def test_adds():\n    assert 1 + 1 == 2\n\ndef test_membership():\n    values = [1, 2]\n    assert 2 in values\n\ndef test_reports_failure():\n    assert 1 == 2, \"one is not two\"\n\ndef helper() -> int32:\n    return 1\n",
    )
    .expect("function test source should write");
    fs::write(
        tests.join("main_style.au"),
        "def main() -> int32:\n    print(\"main style\")\n    return 0\n",
    )
    .expect("main-style test source should write");

    let run = Command::new(aura_bin())
        .current_dir(temp.path())
        .arg("test")
        .output()
        .expect("failed to run aura test");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);

    // Each `def test_*()` is its own result, named by file and function.
    assert!(
        stdout.contains("::test_adds"),
        "expected a per-function result, stdout was:\n{stdout}"
    );
    assert!(stdout.contains("::test_membership"), "{stdout}");
    // A file without any `def test_*()` still reports one result for the file.
    assert!(stdout.contains("main_style.au"), "{stdout}");
    assert!(
        !stdout.contains("::helper") && !stderr.contains("::helper"),
        "a non-test function must not be discovered"
    );

    // A failing assertion reports its message and span, not just a count.
    assert!(
        stderr.contains("::test_reports_failure"),
        "expected the failing function to be named, stderr was:\n{stderr}"
    );
    assert!(stderr.contains("one is not two"), "{stderr}");
    assert!(stderr.contains("functions.au:9:5"), "{stderr}");

    assert!(stdout.contains("3 passed; 1 failed"), "{stdout}");
    assert!(!run.status.success(), "a failing test must fail the run");
}

#[test]
fn aura_test_maintained_assertions_example_pins_the_runner_contract() {
    let example = repo_root().join("examples/basics/assertions.au");
    let run = Command::new(aura_bin())
        .args(["test", "--format", "json", "-k", "[unicode]"])
        .arg(&example)
        .output()
        .expect("maintained assertions example should run as a test module");

    assert!(
        run.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(run.stderr.is_empty());
    let report: serde_json::Value =
        serde_json::from_slice(&run.stdout).expect("example JSON report should parse");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["summary"]["selected"], 1);
    assert_eq!(report["summary"]["passed"], 1);
    assert_eq!(report["summary"]["failed"], 0);
    assert!(report["discovery"][0]["name"]
        .as_str()
        .unwrap()
        .ends_with("::test_registered"));
    assert_eq!(report["discovery"][0]["stdout"], "registering cases\n");
    assert!(report["tests"][0]["name"]
        .as_str()
        .unwrap()
        .ends_with("::test_registered[unicode]"));
    assert_eq!(report["tests"][0]["outcome"], "passed");
    assert_eq!(
        report["tests"][0]["stdout"],
        "setup\nunicode case\nteardown\n"
    );
}

#[test]
fn aura_test_treats_file_level_assertions_as_test_results() {
    let temp = TempDir::new("aura-file-assert-tests");
    let passing_path = temp.path().join("passing.au");
    fs::write(
        &passing_path,
        "def main():\n    assert true, \"passing assertion\"\n",
    )
    .expect("passing assertion test should write");
    let passing = Command::new(aura_bin())
        .args(["test"])
        .arg(&passing_path)
        .output()
        .expect("failed to run passing file-level assertion test");
    assert!(
        passing.status.success(),
        "passing assertion test should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&passing.stderr)
    );
    assert!(String::from_utf8_lossy(&passing.stdout).contains("1 passed; 0 failed"));

    let failing_path = temp.path().join("failing.au");
    fs::write(
        &failing_path,
        "def main():\n    assert false, \"file-level assertion\"\n",
    )
    .expect("failing assertion test should write");
    let failing = Command::new(aura_bin())
        .args(["test"])
        .arg(&failing_path)
        .output()
        .expect("failed to run failing file-level assertion test");
    assert!(
        !failing.status.success(),
        "failing assertion test should fail"
    );
    assert!(String::from_utf8_lossy(&failing.stdout).contains("0 passed; 1 failed"));
    let stderr = String::from_utf8_lossy(&failing.stderr);
    assert!(stderr.contains("FAILED"));
    assert!(stderr.contains("error[AU4001]: file-level assertion"));
    assert!(stderr.contains("assert false"));
}

#[test]
fn aura_test_filter_is_literal_case_sensitive_and_validates_usage() {
    let temp = TempDir::new("aura-test-filter");
    let source_path = temp.path().join("filter.au");
    fs::write(
        &source_path,
        "def test_Alpha():\n    pass\n\ndef test_alphabet():\n    pass\n\ndef test_beta():\n    pass\n",
    )
    .expect("filter source should write");

    let selected = Command::new(aura_bin())
        .args(["test", "-k", "Alpha"])
        .arg(&source_path)
        .output()
        .expect("filtered test run should start");
    assert!(selected.status.success());
    let stdout = String::from_utf8_lossy(&selected.stdout);
    assert!(stdout.contains("::test_Alpha"), "{stdout}");
    assert!(!stdout.contains("::test_alphabet"), "{stdout}");
    assert!(stdout.contains("1 passed; 0 failed"), "{stdout}");

    let no_match = Command::new(aura_bin())
        .args(["test", "-k", "ALPHA"])
        .arg(&source_path)
        .output()
        .expect("zero-match test run should start");
    assert!(no_match.status.success());
    assert_eq!(
        String::from_utf8_lossy(&no_match.stdout),
        "0 passed; 0 failed\n"
    );

    let missing = Command::new(aura_bin())
        .args(["test", "-k"])
        .output()
        .expect("missing-filter-value run should start");
    assert_eq!(missing.status.code(), Some(2));

    for arguments in [
        vec!["test", "-k", ""],
        vec!["test", "-k", "alpha", "-k", "beta"],
    ] {
        let invalid = Command::new(aura_bin())
            .args(arguments)
            .arg(&source_path)
            .output()
            .expect("invalid filtered test run should start");
        assert_eq!(invalid.status.code(), Some(2));
    }
}

#[test]
fn aura_test_preserves_source_declaration_order_and_normalizes_reported_paths() {
    let temp = TempDir::new("aura-test-source-order");
    let tests = temp.path().join("tests");
    fs::create_dir_all(&tests).expect("test directory should create");
    fs::write(
        tests.join("order.au"),
        "def test_zeta():\n    pass\n\ndef test_alpha():\n    pass\n\ndef test_middle():\n    pass\n",
    )
    .expect("ordered test source should write");

    let run = Command::new(aura_bin())
        .current_dir(temp.path())
        .args(["test", "--format", "json", "./tests/../tests/order.au"])
        .output()
        .expect("ordered test run should start");
    assert!(
        run.status.success(),
        "stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&run.stdout).expect("ordered JSON report should parse");
    let tests = report["tests"]
        .as_array()
        .expect("tests should be an array");
    assert_eq!(
        tests
            .iter()
            .map(|record| record["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "tests/order.au::test_zeta",
            "tests/order.au::test_alpha",
            "tests/order.au::test_middle",
        ]
    );
    assert!(tests
        .iter()
        .all(|record| record["file"] == "tests/order.au"));
}

#[test]
fn aura_test_json_is_one_ordered_schema_versioned_document() {
    let temp = TempDir::new("aura-test-json");
    let source_path = temp.path().join("json.au");
    fs::write(
        &source_path,
        "def test_first():\n    print(\"captured program output\")\n\ndef test_second():\n    assert 1 == 2, \"second failed\"\n",
    )
    .expect("JSON test source should write");

    let run = Command::new(aura_bin())
        .args(["test", "--format", "json"])
        .arg(&source_path)
        .output()
        .expect("JSON test run should start");
    assert_eq!(run.status.code(), Some(1));
    assert!(
        run.stderr.is_empty(),
        "stderr was not empty: {:?}",
        run.stderr
    );
    let report: serde_json::Value =
        serde_json::from_slice(&run.stdout).expect("stdout should be one JSON document");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["summary"]["selected"], 2);
    assert_eq!(report["summary"]["passed"], 1);
    assert_eq!(report["summary"]["failed"], 1);
    let tests = report["tests"]
        .as_array()
        .expect("tests should be an array");
    assert!(tests[0]["name"].as_str().unwrap().ends_with("::test_first"));
    assert_eq!(tests[0]["outcome"], "passed");
    assert!(tests[0]["duration_ms"].as_u64().is_some());
    assert_eq!(tests[0]["stdout"], "captured program output\n");
    assert!(tests[1]["name"]
        .as_str()
        .unwrap()
        .ends_with("::test_second"));
    assert_eq!(tests[1]["outcome"], "failed");
    assert_eq!(tests[1]["diagnostic"]["code"], "AU4001");
    assert_eq!(
        tests[1]["diagnostic"]["assertion_operands"],
        serde_json::json!([
            {"label": "left", "type": "int64", "value": "1", "truncated": false},
            {"label": "right", "type": "int64", "value": "2", "truncated": false}
        ])
    );
    assert!(tests[1].get("reason").is_none());
}

#[test]
fn aura_test_runs_setup_and_teardown_for_each_selected_case() {
    let temp = TempDir::new("aura-test-hooks");
    let source_path = temp.path().join("hooks.au");
    fs::write(
        &source_path,
        "def setup():\n    print(\"setup\")\n\ndef teardown():\n    print(\"teardown\")\n\ndef test_one():\n    print(\"one\")\n\ndef test_two():\n    print(\"two\")\n    assert false, \"two failed\"\n",
    )
    .expect("hook source should write");

    let run = Command::new(aura_bin())
        .args(["test"])
        .arg(&source_path)
        .output()
        .expect("hook test run should start");
    assert_eq!(run.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(stdout.matches("setup\n").count(), 2, "{stdout}");
    assert_eq!(stdout.matches("teardown\n").count(), 2, "{stdout}");
    assert!(stdout.contains("setup\none\nteardown\n"), "{stdout}");
    assert!(stdout.contains("setup\ntwo\nteardown\n"), "{stdout}");

    let selected = Command::new(aura_bin())
        .args(["test", "-k", "test_one"])
        .arg(&source_path)
        .output()
        .expect("selected hook test run should start");
    let selected_stdout = String::from_utf8_lossy(&selected.stdout);
    assert_eq!(selected_stdout.matches("setup\n").count(), 1);
    assert_eq!(selected_stdout.matches("teardown\n").count(), 1);
    assert!(!selected_stdout.contains("two\n"));
}

#[test]
fn aura_test_ignores_class_trait_and_impl_methods_named_like_hooks() {
    let temp = TempDir::new("aura-test-non-module-hooks");
    let source_path = temp.path().join("non_module_hooks.au");
    fs::write(
        &source_path,
        r#"trait Lifecycle:
    def setup(self) -> None
    def teardown(self) -> None

class Helper:
    value: int32

    def setup(self):
        print("class setup must not run")

    def teardown(self):
        print("class teardown must not run")

class Worker:
    value: int32

impl Lifecycle for Worker:
    def setup(self) -> None:
        print("impl setup must not run")

    def teardown(self) -> None:
        print("impl teardown must not run")

def test_ok():
    pass
"#,
    )
    .expect("non-module-hook source should write");

    let run = Command::new(aura_bin())
        .args(["test"])
        .arg(&source_path)
        .output()
        .expect("non-module-hook test run should start");
    assert!(
        run.status.success(),
        "stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("::test_ok"), "{stdout}");
    assert!(!stdout.contains("must not run"), "{stdout}");
}

#[test]
fn aura_test_non_function_hook_collisions_are_structured_diagnostics() {
    let temp = TempDir::new("aura-test-hook-collision");
    let source_path = temp.path().join("hook_collision.au");
    fs::write(&source_path, "setup = 1\n\ndef test_ok():\n    pass\n")
        .expect("hook-collision source should write");

    let run = Command::new(aura_bin())
        .args(["test", "--format", "json"])
        .arg(&source_path)
        .output()
        .expect("hook-collision test run should start");
    assert_eq!(run.status.code(), Some(1));
    assert!(run.stderr.is_empty());
    let report: serde_json::Value =
        serde_json::from_slice(&run.stdout).expect("hook-collision JSON should parse");
    assert_eq!(report["tests"][0]["diagnostic"]["code"], "AU2999");
    assert_eq!(
        report["tests"][0]["diagnostic"]["message"],
        "test hook `setup` must be a module function"
    );
    assert_eq!(
        report["tests"][0]["diagnostic"]["primary_span"]["start"]["line"],
        1
    );

    fs::write(
        &source_path,
        "public teardown: int32 = 1\n\ndef test_ok():\n    pass\n",
    )
    .expect("constant teardown collision source should write");
    let constant_teardown = Command::new(aura_bin())
        .args(["test", "--format", "json"])
        .arg(&source_path)
        .output()
        .expect("constant teardown collision test run should start");
    assert_eq!(constant_teardown.status.code(), Some(1));
    assert!(constant_teardown.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&constant_teardown.stdout)
        .expect("constant teardown collision JSON should parse");
    assert_eq!(report["tests"][0]["diagnostic"]["code"], "AU2999");
    assert_eq!(
        report["tests"][0]["diagnostic"]["message"],
        "test hook `teardown` must be a module function"
    );
    assert_eq!(
        report["tests"][0]["diagnostic"]["primary_span"]["start"]["line"],
        1
    );

    fs::write(
        &source_path,
        "def setup(value: int32):\n    pass\n\ndef test_ok():\n    pass\n",
    )
    .expect("invalid-hook-signature source should write");
    let invalid_signature = Command::new(aura_bin())
        .args(["test", "--format", "json"])
        .arg(&source_path)
        .output()
        .expect("invalid-hook-signature test run should start");
    let report: serde_json::Value = serde_json::from_slice(&invalid_signature.stdout)
        .expect("invalid-hook-signature JSON should parse");
    assert_eq!(report["tests"][0]["diagnostic"]["code"], "AU2999");
    assert_eq!(
        report["tests"][0]["diagnostic"]["message"],
        "test hook `setup` must be parameterless and return `None`"
    );
    assert_eq!(
        report["tests"][0]["diagnostic"]["primary_span"]["start"]["line"],
        1
    );
}

#[test]
fn aura_test_hook_failures_preserve_primary_and_report_teardown_secondarily() {
    let temp = TempDir::new("aura-test-hook-failures");
    let source_path = temp.path().join("hook_failures.au");
    fs::write(
        &source_path,
        "def setup():\n    print(\"setup\")\n\ndef teardown():\n    print(\"teardown\")\n    assert false, \"teardown failed\"\n\ndef test_body():\n    print(\"body\")\n    assert false, \"body failed\"\n",
    )
    .expect("hook-failure source should write");

    let run = Command::new(aura_bin())
        .args(["test"])
        .arg(&source_path)
        .output()
        .expect("hook-failure test run should start");
    assert_eq!(run.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&run.stdout)
            .matches("teardown\n")
            .count(),
        1,
        "teardown must run after the body traps"
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("error[AU4001]: body failed"), "{stderr}");
    assert!(stderr.contains("teardown also failed for"), "{stderr}");
    assert!(
        stderr.contains("error[AU4001]: teardown failed"),
        "{stderr}"
    );

    let json_run = Command::new(aura_bin())
        .args(["test", "--format", "json"])
        .arg(&source_path)
        .output()
        .expect("JSON hook-failure test run should start");
    let report: serde_json::Value =
        serde_json::from_slice(&json_run.stdout).expect("hook-failure JSON should parse");
    assert_eq!(report["tests"][0]["diagnostic"]["message"], "body failed");
    assert_eq!(report["tests"][0]["secondary"]["stage"], "teardown");
    assert_eq!(
        report["tests"][0]["secondary"]["diagnostic"]["message"],
        "teardown failed"
    );
    assert!(report["tests"][0]["secondary"]["diagnostic"]["primary_span"].is_object());

    fs::write(
        &source_path,
        "def setup():\n    print(\"setup\")\n    assert false, \"setup failed\"\n\ndef teardown():\n    print(\"teardown\")\n\ndef test_body():\n    print(\"body must not run\")\n",
    )
    .expect("setup-failure source should write");
    let setup_failure = Command::new(aura_bin())
        .args(["test"])
        .arg(&source_path)
        .output()
        .expect("setup-failure test run should start");
    let stdout = String::from_utf8_lossy(&setup_failure.stdout);
    assert!(stdout.contains("setup\nteardown\n"), "{stdout}");
    assert!(!stdout.contains("body must not run"), "{stdout}");
    assert!(String::from_utf8_lossy(&setup_failure.stderr).contains("setup failed"));
}

#[test]
fn aura_test_lifecycle_order_is_observable_through_external_side_effects() {
    let temp = TempDir::new("aura-test-lifecycle-side-effects");
    let source_path = temp.path().join("lifecycle.au");
    let trace_path = temp.path().join("trace.txt");
    let trace = trace_path.display();
    fs::write(
        &source_path,
        format!(
            r#"import fs

def setup():
    match fs.append_string("{trace}", "setup\n"):
        case Result.Ok(_):
            pass
        case Result.Err(_):
            assert false, "setup append failed"

def teardown():
    match fs.append_string("{trace}", "teardown\n"):
        case Result.Ok(_):
            pass
        case Result.Err(_):
            assert false, "teardown append failed"

def test_first():
    match fs.append_string("{trace}", "first\n"):
        case Result.Ok(_):
            pass
        case Result.Err(_):
            assert false, "first append failed"

def test_second():
    match fs.append_string("{trace}", "second\n"):
        case Result.Ok(_):
            pass
        case Result.Err(_):
            assert false, "second append failed"
"#
        ),
    )
    .expect("lifecycle source should write");

    let run = Command::new(aura_bin())
        .args(["test"])
        .arg(&source_path)
        .output()
        .expect("lifecycle test run should start");
    assert!(
        run.status.success(),
        "stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        fs::read_to_string(&trace_path).expect("lifecycle trace should exist"),
        "setup\nfirst\nteardown\nsetup\nsecond\nteardown\n"
    );
}

#[test]
fn aura_test_reuses_one_checked_module_when_setup_rewrites_the_source() {
    let temp = TempDir::new("aura-test-checked-module-reuse");
    let source_path = temp.path().join("checked_once.au");
    let escaped_path = source_path
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let replacement = "def test_checked_body():\n    assert false, \"rewritten source ran\"\n";
    let escaped_replacement = replacement
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    fs::write(
        &source_path,
        format!(
            r#"import fs

def setup():
    print("original setup")
    match fs.write_string("{escaped_path}", "{escaped_replacement}"):
        case Result.Ok(_):
            pass
        case Result.Err(_):
            assert false, "source rewrite failed"

def teardown():
    print("original teardown")

def test_checked_body():
    print("original body")
"#
        ),
    )
    .expect("checked-once source should write");

    let run = Command::new(aura_bin())
        .args(["test"])
        .arg(&source_path)
        .output()
        .expect("checked-once test run should start");
    assert!(
        run.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("original setup\noriginal body\noriginal teardown\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("rewritten source ran"), "{stdout}");
    assert_eq!(
        fs::read_to_string(&source_path).expect("rewritten source should remain readable"),
        replacement,
        "setup must really rewrite the source before the body executes"
    );
}

#[cfg(unix)]
#[test]
fn aura_test_preserves_manifest_authorized_ffi_across_lifecycle_phases() {
    let temp = TempDir::new("aura-test-checked-ffi");
    let source_dir = temp.path().join("src");
    fs::create_dir_all(&source_dir).expect("FFI package source directory should create");
    fs::write(
        temp.path().join("Aura.toml"),
        "[package]\nname = \"test_runner_ffi\"\nversion = \"0.1.0\"\nedition = \"2026\"\nallow_ffi = true\n",
    )
    .expect("FFI package manifest should write");
    let source_path = source_dir.join("main.au");
    fs::write(
        &source_path,
        r#"public extern "C" def getpid() -> int32

def setup():
    assert getpid() > 0
    print("ffi setup")

def teardown():
    assert getpid() > 0
    print("ffi teardown")

def test_ffi():
    assert getpid() > 0
    print("ffi body")
"#,
    )
    .expect("FFI lifecycle source should write");

    let run = Command::new(aura_bin())
        .args(["test"])
        .arg(&source_path)
        .output()
        .expect("FFI lifecycle test should start");
    assert!(
        run.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ffi setup\nffi body\nffi teardown\n"),
        "{stdout}"
    );
    assert!(stdout.contains("1 passed; 0 failed"), "{stdout}");
}

#[test]
fn aura_test_json_runner_failures_use_reason_and_do_not_leak_program_stdout() {
    let temp = TempDir::new("aura-test-json-runner-failure");
    let source_path = temp.path().join("timeout.au");
    fs::write(
        &source_path,
        "def test_timeout():\n    print(\"must stay inside the result runner\")\n    while true:\n        pass\n",
    )
    .expect("timeout source should write");

    let run = Command::new(aura_bin())
        .args(["test", "--format", "json", "--timeout-ms", "100"])
        .arg(&source_path)
        .output()
        .expect("JSON timeout test run should start");
    assert_eq!(run.status.code(), Some(1));
    assert!(run.stderr.is_empty());
    let report: serde_json::Value =
        serde_json::from_slice(&run.stdout).expect("stdout should contain only JSON");
    assert_eq!(report["tests"][0]["outcome"], "failed");
    assert!(report["tests"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("timed out after 100ms"));
    assert!(report["tests"][0].get("diagnostic").is_none());
    assert_eq!(
        report["tests"][0]["stdout"],
        "must stay inside the result runner\n"
    );
}

#[test]
fn aura_test_expands_parameter_registrations_before_filtering() {
    let temp = TempDir::new("aura-test-parameters");
    let source_path = temp.path().join("parameters.au");
    fs::write(
        &source_path,
        "def zero():\n    print(\"zero\")\n\ndef unicode_case():\n    print(\"unicode\")\n\ndef test_cases() -> list[(str, def() -> None)]:\n    print(\"registration output\")\n    return [(\"zero\", zero), (\"unicode\", unicode_case)]\n",
    )
    .expect("parameter source should write");

    let run = Command::new(aura_bin())
        .args(["test", "-k", "[unicode]"])
        .arg(&source_path)
        .output()
        .expect("parameterized test run should start");
    assert!(
        run.status.success(),
        "stderr was:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("::test_cases[unicode]"),
        "{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(!stdout.contains("::test_cases[zero]"), "{stdout}");
    assert_eq!(
        stdout.matches("registration output\n").count(),
        1,
        "{stdout}"
    );
    assert!(stdout.contains("unicode\n"), "{stdout}");
    assert!(!stdout.contains("zero\n"), "{stdout}");

    let json_run = Command::new(aura_bin())
        .args(["test", "--format", "json", "-k", "[unicode]"])
        .arg(&source_path)
        .output()
        .expect("JSON parameterized test run should start");
    assert!(json_run.status.success());
    assert!(json_run.stderr.is_empty());
    let report: serde_json::Value =
        serde_json::from_slice(&json_run.stdout).expect("JSON report should parse");
    assert_eq!(report["discovery"][0]["stdout"], "registration output\n");
    assert_eq!(report["tests"][0]["stdout"], "unicode\n");
}

#[test]
fn aura_test_registration_type_rejects_capturing_closures() {
    let temp = TempDir::new("aura-test-captured-parameter");
    let source_path = temp.path().join("captured_parameter.au");
    fs::write(
        &source_path,
        "def test_cases() -> list[(str, def() -> None)]:\n    factor = 2\n    return [(\"captured\", lambda: print(factor))]\n",
    )
    .expect("captured parameter source should write");

    let run = Command::new(aura_bin())
        .args(["test"])
        .arg(&source_path)
        .output()
        .expect("captured parameter test run should start");
    assert_eq!(run.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("found `(str, closure def() -> None)`"),
        "{stderr}"
    );
    assert!(!stderr.contains("registered case"), "{stderr}");
}

#[test]
fn aura_test_validates_parameter_labels_and_allows_empty_registration() {
    let temp = TempDir::new("aura-test-parameter-labels");
    let source_path = temp.path().join("parameter_labels.au");

    fs::write(
        &source_path,
        "def registered_case():\n    pass\n\ndef test_cases() -> list[(str, def() -> None)]:\n    return [(\"same\", registered_case), (\"same\", registered_case)]\n",
    )
    .expect("duplicate-label source should write");
    let duplicate = Command::new(aura_bin())
        .args(["test"])
        .arg(&source_path)
        .output()
        .expect("duplicate-label test run should start");
    assert_eq!(duplicate.status.code(), Some(1));
    let duplicate_stderr = String::from_utf8_lossy(&duplicate.stderr);
    assert!(
        duplicate_stderr.contains("duplicate test registration label `same`"),
        "{duplicate_stderr}"
    );

    fs::write(
        &source_path,
        "def registered_case():\n    pass\n\ndef test_cases() -> list[(str, def() -> None)]:\n    return [(\"\", registered_case)]\n",
    )
    .expect("empty-label source should write");
    let empty_label = Command::new(aura_bin())
        .args(["test"])
        .arg(&source_path)
        .output()
        .expect("empty-label test run should start");
    assert_eq!(empty_label.status.code(), Some(1));
    let empty_label_stderr = String::from_utf8_lossy(&empty_label.stderr);
    assert!(
        empty_label_stderr.contains("test registration labels must be non-empty"),
        "{empty_label_stderr}"
    );

    fs::write(
        &source_path,
        "def test_cases() -> list[(str, def() -> None)]:\n    return []\n",
    )
    .expect("empty-registration source should write");
    let empty_registration = Command::new(aura_bin())
        .args(["test"])
        .arg(&source_path)
        .output()
        .expect("empty-registration test run should start");
    assert!(empty_registration.status.success());
    assert_eq!(
        String::from_utf8_lossy(&empty_registration.stdout),
        "0 passed; 0 failed\n"
    );
}

#[test]
fn aura_test_registration_timeout_retains_stdout_in_the_single_json_document() {
    let temp = TempDir::new("aura-test-registration-timeout-output");
    let source_path = temp.path().join("registration_timeout.au");
    fs::write(
        &source_path,
        "def test_cases() -> list[(str, def() -> None)]:\n    print(\"registration began\")\n    while true:\n        pass\n    return []\n",
    )
    .expect("registration-timeout source should write");

    let run = Command::new(aura_bin())
        .args(["test", "--format", "json", "--timeout-ms", "100"])
        .arg(&source_path)
        .output()
        .expect("registration-timeout test run should start");
    assert_eq!(run.status.code(), Some(1));
    assert!(run.stderr.is_empty());
    let report: serde_json::Value =
        serde_json::from_slice(&run.stdout).expect("registration-timeout JSON should parse");
    assert!(report["tests"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("test registration timed out after 100ms"));
    assert_eq!(report["tests"][0]["stdout"], "registration began\n");
}

#[test]
fn native_cache_format_carries_the_aura_identity_epoch() {
    let main = include_str!("../src/main.rs");
    assert!(
        main.contains(r#"const NATIVE_CACHE_FORMAT: &str = "aura-native-cache-v5";"#),
        "native cache format must carry the Aura identity epoch"
    );
    assert!(
        !main.contains("aura-native-cache-v4"),
        "the prior cache format must not linger in key material"
    );
}

#[test]
fn typed_select_queue_priority_and_loser_preservation_match_with_four_workers() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/select_queue_priority.au");
    let expected =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/select_queue_priority.stdout");
    assert_mir_and_direct_source_stdout_with_timeout_and_workers(
        "aura-typed-select-queue-priority",
        source,
        std::time::Duration::from_secs(30),
        expected,
        4,
    );
}

#[test]
fn typed_select_nested_queue_payload_types_match_on_both_backends() {
    let source =
        include_str!("../../aura-compiler/tests/fixtures/run-pass/select_nested_payload_typing.au");
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/select_nested_payload_typing.stdout"
    );
    assert_mir_and_direct_source_stdout_with_timeout_and_workers(
        "aura-typed-select-nested-payload-typing",
        source,
        std::time::Duration::from_secs(30),
        expected,
        4,
    );
}

#[test]
fn typed_select_nonrepeatable_task_delivery_matches_with_four_workers() {
    let source = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/select_nonrepeatable_task_delivery.au"
    );
    let expected = include_str!(
        "../../aura-compiler/tests/fixtures/run-pass/select_nonrepeatable_task_delivery.stdout"
    );
    assert_mir_and_direct_source_stdout_with_timeout_and_workers(
        "aura-typed-select-nonrepeatable-task-delivery",
        source,
        std::time::Duration::from_secs(30),
        expected,
        4,
    );
}

#[test]
fn typed_select_pending_queue_task_deadline_and_cancellation_match_with_four_workers() {
    let source = r#"def publish(queue: Queue[int32], value: int32) -> int32:
    sleep(2ms)
    queue.put(value)
    return value

def finish(value: int32) -> int32:
    sleep(2ms)
    return value

def fail() -> int32:
    return 1 // 0

def observe(label: str, queue: Queue[int32]) -> Queue[int32]:
    print(label)
    return queue

def wait_until_cancelled(queue: Queue[int32]) -> int32:
    match select(queue, 1s):
        case SelectOutcome.Queue(_, _):
            return -1
        case SelectOutcome.Task(_, _):
            return -2
        case SelectOutcome.Deadline(_):
            return -3
        case SelectOutcome.Cancelled:
            return 9

def main():
    pending = Queue[int32]()
    with TaskGroup() as group:
        producer = group.start(publish, pending, 41)
        match select(100ms, pending):
            case SelectOutcome.Queue(index, outcome):
                print(index)
                print(outcome)
            case SelectOutcome.Task(_, _):
                print("unexpected task")
            case SelectOutcome.Deadline(_):
                print("unexpected deadline")
            case SelectOutcome.Cancelled:
                print("unexpected cancellation")
        print(producer.result())

    with TaskGroup() as group:
        task = group.start(finish, 42)
        match select(100ms, task):
            case SelectOutcome.Queue(_, _):
                print("unexpected queue")
            case SelectOutcome.Task(index, outcome):
                print(index)
                print(outcome)
            case SelectOutcome.Deadline(_):
                print("unexpected deadline")
            case SelectOutcome.Cancelled:
                print("unexpected cancellation")
        print(task.result())

    with TaskGroup() as group:
        failed = group.start(fail)
        match select(failed):
            case SelectOutcome.Queue(_, _):
                print("unexpected queue")
            case SelectOutcome.Task(index, outcome):
                print(index)
                match outcome:
                    case TaskResult.Ready(_):
                        print("unexpected ready")
                    case TaskResult.Error(_):
                        print("task error")
                    case TaskResult.TimedOut:
                        print("unexpected timeout")
                    case TaskResult.Cancelled:
                        print("terminal child cancelled")
            case SelectOutcome.Deadline(_):
                print("unexpected deadline")
            case SelectOutcome.Cancelled:
                print("unexpected cancellation")

    never = Queue[int32]()
    with TaskGroup() as group:
        waiter = group.start(wait_until_cancelled, never)
        sleep(2ms)
        group.cancel()
        print(waiter.result())

    first = Queue[int32]()
    second = Queue[int32]()
    first.put(1)
    second.put(2)
    print(select(observe("first", first), observe("second", second)))
    print(second.get())

    print(select(0ms, 0ms))
"#;
    assert_mir_and_direct_source_stdout_with_timeout_and_workers(
        "aura-typed-select-four-worker-matrix",
        source,
        std::time::Duration::from_secs(30),
        concat!(
            "1\n",
            "QueueReceive.Item(41)\n",
            "TaskResult.Ready(41)\n",
            "1\n",
            "TaskResult.Ready(42)\n",
            "TaskResult.Ready(42)\n",
            "0\n",
            "task error\n",
            "TaskResult.Ready(9)\n",
            "first\n",
            "second\n",
            "SelectOutcome.Queue(0, QueueReceive.Item(1))\n",
            "QueueReceive.Item(2)\n",
            "SelectOutcome.Deadline(0)\n",
        ),
        4,
    );
}

#[test]
fn typed_select_negative_deadline_is_au4001_on_both_backends_with_four_workers() {
    let source = "def main():\n    print(select(Duration.ms(-1)))\n";
    let (temp, source_path) = write_temp_source("aura-typed-select-negative-deadline", source);

    let mut mir = Command::new(aura_bin());
    mir.env("AURA_WORKERS", "4")
        .args(["run", "--backend", "mir"])
        .arg(&source_path);
    let mir = command_output_with_timeout(
        mir,
        std::time::Duration::from_secs(30),
        "typed-select negative-deadline MIR fixture",
    );
    assert!(!mir.status.success());
    assert_eq!(
        String::from_utf8_lossy(&mir.stderr).lines().next(),
        Some("error[AU4001]: select deadline must be non-negative")
    );

    let output_path = temp.path().join("out");
    let build = Command::new(aura_bin())
        .args(["build", "--backend", "direct", "-o"])
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("failed to build typed-select negative-deadline fixture");
    assert!(
        build.status.success(),
        "typed-select negative-deadline direct build should succeed, stderr was:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let mut direct = generated_binary(&output_path);
    direct.env("AURA_WORKERS", "4");
    let direct = command_output_with_timeout(
        direct,
        std::time::Duration::from_secs(30),
        "typed-select negative-deadline direct fixture",
    );
    assert!(!direct.status.success());
    assert_eq!(
        String::from_utf8_lossy(&direct.stderr).lines().next(),
        Some("error[AU4001]: select deadline must be non-negative")
    );
}

#[test]
fn capture_free_function_values_cover_storage_copy_calls_and_task_targets_on_both_backends() {
    let source = r#"
class Pipeline:
    transform: def(int32) -> int32

def increment(value: int32) -> int32:
    return value + 1

def double(value: int32) -> int32:
    return value * 2

def offset(value: int32 = 10) -> int32:
    return value + 1

def mark(label: str, value: int32) -> int32:
    print(label)
    return value

def first_default(value: int32 = mark("first-default", 11)) -> int32:
    return value

def second_default(value: int32 = mark("second-default", 22)) -> int32:
    return value

def first_task_default(value: int32 = mark("task-first-default", 31)) -> int32:
    return value

def second_task_default(value: int32 = mark("task-second-default", 42)) -> int32:
    return value

def combine(first: int32, second: int32) -> int32:
    return first * 10 + second

def apply(transform: def(int32) -> int32, value: int32) -> int32:
    return transform(value)

def apply_owned(transform: own def(int32) -> int32, value: int32) -> int32:
    return transform(value)

def choose_transform(use_increment: bool) -> def(int32) -> int32:
    return increment if use_increment else double

def publish(values: Queue[int32], value: int32) -> None:
    values.put(value)

def empty[T]() -> Option[T]:
    return None

def main() -> int32:
    selected: def(int32) -> int32 = increment
    copied = selected
    known_offset = offset
    selected_default = first_default if false else second_default
    known_combine = combine
    pipeline = Pipeline(transform=copied)
    transforms: list[def(int32) -> int32] = [pipeline.transform, double]

    print(selected(1))
    print(apply(pipeline.transform, 2))
    print(transforms[0](3))
    print(transforms[1](4))
    print(apply_owned(copied, 5))
    print(copied(6))
    runtime_selected = choose_transform(false)
    print(runtime_selected(7))
    print(known_offset())
    print(known_offset(30))
    print(known_offset(value=40))
    print(selected_default())
    print(known_combine(second=mark("named-second", 2), first=mark("named-first", 1)))

    values = Queue[int32]()
    publisher = publish
    task_target = choose_transform(true)
    local_empty: def() -> Option[int32] = empty
    selected_task_default = first_task_default if false else second_task_default
    with TaskGroup() as group:
        task = group.start(task_target, 20)
        empty_task = group.start(local_empty)
        group.start_soon(publisher, values, 9)
        print(task.result_or(-1, timeout=1s))
        match empty_task.result_or(Option.Some(99), timeout=1s):
            case Option.None:
                print("local-none")
            case Option.Some(value):
                print(value)
    print(values.get_or(-1))
    with default_group = TaskGroup():
        default_task = default_group.start(selected_task_default)
        print(default_task.result_or(-1, timeout=1s))
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-capture-free-function-values",
        source,
        "2\n3\n4\n8\n6\n7\n14\n11\n31\n41\nsecond-default\n22\nnamed-second\nnamed-first\n12\n21\nlocal-none\n9\ntask-second-default\n42\n",
    );
}

#[test]
fn imported_module_function_values_store_pass_and_call_on_both_backends() {
    assert_default_backend_example_runs(
        "examples/modules/function_values.au",
        "module-function-values-auto",
        "10\n12\nnone\n",
    );
    assert_direct_backend_example_runs(
        "examples/modules/function_values.au",
        "module-function-values-direct",
        "10\n12\nnone\n",
    );
}

#[test]
fn imported_builtin_function_values_use_normal_dispatch_on_both_backends() {
    let source = r#"
import process

def main() -> int32:
    factory: def() -> process.Stdio = process.pipe
    stream = factory()
    match own stream:
        case process.Stdio.Pipe:
            print("pipe")
        case process.Stdio.Null:
            print("null")
        case process.Stdio.Inherit:
            print("inherit")
    return 0
"#;

    assert_run_and_direct_source_stdout("aura-imported-builtin-function-value", source, "pipe\n");
}

#[test]
fn imported_builtin_function_values_retain_process_run_defaults_on_both_backends() {
    let fixture =
        "crates/aura-compiler/tests/fixtures/run-pass/function_value_imported_builtin_defaults.au";
    let expected = "true\nbuiltin-defaults\n";

    assert_default_backend_example_runs(fixture, "builtin-function-defaults-auto", expected);
    assert_direct_backend_example_runs(fixture, "builtin-function-defaults-direct", expected);
}

#[test]
fn generic_default_can_supply_a_function_value_for_calls_and_tasks_on_both_backends() {
    let source = r#"
def empty[T]() -> Option[T]:
    return None

def supplier[T](callback: def() -> Option[T] = empty) -> def() -> Option[T]:
    return callback

def main() -> int32:
    supplied = supplier[str]()
    match supplied():
        case Option.None:
            print("ordinary-none")
        case Option.Some(value):
            print(value)

    with group = TaskGroup():
        task = group.start(supplied)
        match task.result_or(Option.Some("fallback"), timeout=1s):
            case Option.None:
                print("task-none")
            case Option.Some(value):
                print(value)
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-function-value-generic-default-supplier",
        source,
        "ordinary-none\ntask-none\n",
    );
}

#[test]
fn inferred_function_values_preserve_mut_writeback_and_own_consumption_on_both_backends() {
    let source = r#"
class Counter:
    value: int32

def increment(counter: mut Counter) -> None:
    counter.value += 1

def take(value: own str) -> str:
    return value

class Holder:
    mutate: def(mut Counter) -> None
    consume: def(own str) -> str

def apply_mut(callback: def(mut Counter) -> None, counter: mut Counter) -> None:
    callback(counter)

def apply_own(callback: def(own str) -> str, value: own str) -> str:
    return callback(value)

def main() -> int32:
    mutate: def(mut Counter) -> None = increment
    consume: def(own str) -> str = take
    mutators: list[def(mut Counter) -> None] = [mutate]
    consumers: list[def(own str) -> str] = [consume]
    holder = Holder(mutate=mutate, consume=consume)
    mut counter = Counter(value=41)
    text = "owned"

    apply_mut(mutate, counter)
    mutators[0](counter)
    holder.mutate(counter)
    parameter = apply_own(consume, "parameter-owned")
    first = consumers[0]("vector-owned")
    result = holder.consume(text)
    print(counter.value)
    print(parameter)
    print(first)
    print(result)
    return 0
"#;

    assert_run_and_direct_source_stdout(
        "aura-function-value-capabilities",
        source,
        "44\nparameter-owned\nvector-owned\nowned\n",
    );
}

#[test]
fn runtime_selected_function_value_traps_keep_the_dynamic_target_frame_on_both_backends() {
    let source = r#"
def explode(value: int32) -> int32:
    return 1 // value

def safe(value: int32) -> int32:
    return value

def choose(should_explode: bool) -> def(int32) -> int32:
    return explode if should_explode else safe

def main() -> int32:
    selected = choose(true)
    return selected(0)
"#;

    assert_run_and_direct_source_failure_with_timeout(
        "aura-dynamic-function-value-frame",
        source,
        std::time::Duration::from_secs(20),
        "",
        "Aura call chain (innermost first): explode at 2:1 -> main at 11:1",
    );
}

#[test]
fn trapping_function_value_defaults_report_the_public_target_on_both_backends() {
    let source = r#"
def default_trap(value: int32 = 1 // 0) -> int32:
    return value

def main() -> int32:
    selected = default_trap
    return selected()
"#;
    let timeout = std::time::Duration::from_secs(20);
    let (_temp, _source_path, mut mir_child) =
        run_aura_source_with_timeout("aura-function-value-default-trap", source, timeout);
    let mir_status = wait_with_timeout(&mut mir_child, timeout).unwrap_or_else(|| {
        mir_child.kill().expect("failed to kill timed out aura run");
        panic!("aura run timed out after {:?}", timeout);
    });
    let mir = mir_child
        .wait_with_output()
        .expect("failed to collect aura run output");
    assert!(!mir_status.success());

    let (_temp, _source_path, mut direct_child) = build_direct_source_with_timeout(
        "aura-function-value-default-trap-direct",
        source,
        timeout,
    );
    let direct_status = wait_with_timeout(&mut direct_child, timeout).unwrap_or_else(|| {
        direct_child
            .kill()
            .expect("failed to kill timed out direct run");
        panic!("direct run timed out after {:?}", timeout);
    });
    let direct = direct_child
        .wait_with_output()
        .expect("failed to collect direct run output");
    assert!(!direct_status.success());

    for stderr in [&mir.stderr, &direct.stderr] {
        let stderr = String::from_utf8_lossy(stderr);
        assert!(
            stderr.contains("error[AU4004]: division by zero"),
            "default trap should preserve the public diagnostic, stderr was:\n{stderr}"
        );
        assert!(
            stderr
                .contains("Aura call chain (innermost first): default_trap at 2:33 -> main at 5:1"),
            "default trap should preserve the public function frame, stderr was:\n{stderr}"
        );
        assert!(
            !stderr.contains("::__default_"),
            "implementation-only default thunk leaked into stderr:\n{stderr}"
        );
    }
}

#[test]
fn runtime_selected_task_target_traps_name_the_chosen_target_and_spawn_ancestry() {
    let source = r#"
def explode(value: int32) -> int32:
    return 1 // value

def safe(value: int32) -> int32:
    return value

def choose(should_explode: bool) -> def(int32) -> int32:
    return explode if should_explode else safe

def main() -> int32:
    selected = choose(true)
    with group = TaskGroup():
        task = group.start(selected, 0)
    return 0
"#;
    let timeout = std::time::Duration::from_secs(20);
    let (_temp, _source_path, mut mir_child) =
        run_aura_source_with_timeout("aura-function-value-task-trap", source, timeout);
    let mir_status = wait_with_timeout(&mut mir_child, timeout).unwrap_or_else(|| {
        mir_child.kill().expect("failed to kill timed out aura run");
        panic!("aura run timed out after {:?}", timeout);
    });
    let mir = mir_child
        .wait_with_output()
        .expect("failed to collect aura run output");
    assert!(!mir_status.success());

    let (_temp, _source_path, mut direct_child) =
        build_direct_source_with_timeout("aura-function-value-task-trap-direct", source, timeout);
    let direct_status = wait_with_timeout(&mut direct_child, timeout).unwrap_or_else(|| {
        direct_child
            .kill()
            .expect("failed to kill timed out direct run");
        panic!("direct run timed out after {:?}", timeout);
    });
    let direct = direct_child
        .wait_with_output()
        .expect("failed to collect direct run output");
    assert!(!direct_status.success());

    for stderr in [&mir.stderr, &direct.stderr] {
        let stderr = String::from_utf8_lossy(stderr);
        assert!(
            stderr.contains("error[AU4004]: division by zero"),
            "chosen task target should preserve its trap, stderr was:\n{stderr}"
        );
        assert!(
            stderr.contains("Aura task entry: explode at 2:1"),
            "task entry should name the runtime-selected target, stderr was:\n{stderr}"
        );
        assert!(
            stderr.contains(
                "Aura task ancestry (youngest first): explode spawned from main at 14:"
            ),
            "task ancestry should name the runtime-selected target and spawn site, stderr was:\n{stderr}"
        );
        assert!(
            !stderr.contains("safe at ") && !stderr.contains("function value"),
            "task diagnostic leaked a static alternate or placeholder target:\n{stderr}"
        );
    }
}
