use std::fs;
use std::io::Write;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{self, Command};

use aura_compiler::{
    analyze_path_source, check_path, check_path_with_source, complete_path_source,
    emit_host_native_object_with_metadata, lower_path_to_checked_mir, lower_path_to_mir,
    lower_path_with_source_to_mir, parse_source,
    run_checked_mir_entry_with_stdout_sink_and_program_args,
    run_path_with_source_and_stdout_sink_and_program_args,
    run_path_with_stdout_sink_and_program_args, update_git_dependencies_in_working_dir,
    CheckedMirModule, Diagnostic, MirModule, StructuredDiagnostic, Value,
};
use serde_json::Value as JsonValue;

struct Input {
    path: String,
    source: String,
    from_stdin: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BuildBackend {
    Auto,
    Direct,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SelectedBuildBackend {
    Direct,
    MirRuntime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiagnosticFormat {
    Human,
    Json,
}

#[derive(Debug)]
struct BuildOutcome {
    selected: SelectedBuildBackend,
    fallback_reason: Option<String>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        print_usage_and_exit(2);
    };
    match command.as_str() {
        "help" | "--help" | "-h" => print_usage_and_exit(0),
        "version" | "--version" | "-V" => print_version_and_exit(),
        "lsp" => handle_lsp_service(),
        "new" => handle_new_command(args.collect()),
        "upgrade" => handle_upgrade_command(args.collect()),
        "fmt" => handle_fmt_command(args.collect()),
        "test" => handle_test_command(args.collect()),
        "deps" => {
            let remaining = args.collect::<Vec<_>>();
            handle_deps_command(remaining);
        }
        "check" => {
            let (diagnostic_format, input_args) = parse_diagnostic_format(args.collect::<Vec<_>>());
            let input = read_input(&mut input_args.into_iter());
            let result = if input.from_stdin {
                check_path_with_source(Path::new(&input.path), &input.source)
            } else {
                check_path(Path::new(&input.path))
            };
            match result {
                Ok(_) => match diagnostic_format {
                    DiagnosticFormat::Human => write_stdout("ok\n"),
                    DiagnosticFormat::Json => {
                        write_stdout("{\"schema_version\":1,\"diagnostics\":[]}\n")
                    }
                },
                Err(error) => {
                    emit_diagnostic(diagnostic_format, &input.path, &input.source, &error);
                    process::exit(1);
                }
            }
        }
        "run" => {
            let remaining = args.collect::<Vec<_>>();
            let delimiter = remaining.iter().position(|argument| argument == "--");
            let (input_args, program_args) = match delimiter {
                Some(index) => (&remaining[..index], &remaining[index + 1..]),
                None => (remaining.as_slice(), &[][..]),
            };
            let (diagnostic_format, input_args) = parse_diagnostic_format(input_args.to_vec());
            let (run_backend, input_args) = parse_run_backend(input_args);
            let input = read_input(&mut input_args.into_iter());
            let mut json_native_fallback = None;
            if run_backend != RunBackend::Mir {
                let mut native_progress = Vec::new();
                match run_through_native_backend(
                    &input,
                    program_args,
                    run_backend,
                    diagnostic_format == DiagnosticFormat::Human,
                    diagnostic_format == DiagnosticFormat::Json,
                    &mut native_progress,
                ) {
                    NativeRunOutcome::Ran(code) => {
                        if diagnostic_format == DiagnosticFormat::Json
                            && !native_progress.is_empty()
                        {
                            eprintln!(
                                "{}",
                                serde_json::json!({
                                    "schema_version": 1,
                                    "progress": native_progress,
                                })
                            );
                        }
                        process::exit(code);
                    }
                    NativeRunOutcome::Failed(message) => {
                        let mut error = Diagnostic::new(message);
                        if diagnostic_format == DiagnosticFormat::Json {
                            for progress in native_progress {
                                error = error.with_note(progress);
                            }
                        }
                        emit_diagnostic(diagnostic_format, &input.path, &input.source, &error);
                        process::exit(1);
                    }
                    NativeRunOutcome::Diagnostic(mut error) => {
                        if diagnostic_format == DiagnosticFormat::Json {
                            for progress in native_progress {
                                error = error.with_note(progress);
                            }
                        }
                        emit_diagnostic(diagnostic_format, &input.path, &input.source, &error);
                        process::exit(1);
                    }
                    NativeRunOutcome::StructuredDiagnostic(mut error) => {
                        debug_assert_eq!(diagnostic_format, DiagnosticFormat::Json);
                        error.notes.extend(native_progress);
                        emit_structured_diagnostic(*error);
                        process::exit(1);
                    }
                    NativeRunOutcome::FellBack(reason) => {
                        if diagnostic_format == DiagnosticFormat::Json {
                            json_native_fallback = Some((native_progress, reason));
                        } else {
                            eprintln!(
                                "aura: direct backend unavailable; using the MIR runtime:\n{}",
                                reason
                            );
                        }
                    }
                }
            }
            let stdout_sink = std::sync::Arc::new(|chunk: &str| write_stdout(chunk));
            let result = if input.from_stdin {
                run_path_with_source_and_stdout_sink_and_program_args(
                    Path::new(&input.path),
                    &input.source,
                    stdout_sink,
                    program_args.to_vec(),
                )
            } else {
                run_path_with_stdout_sink_and_program_args(
                    Path::new(&input.path),
                    stdout_sink,
                    program_args.to_vec(),
                )
            };
            match result {
                Ok(output) => {
                    if let Some((progress, reason)) = json_native_fallback {
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "schema_version": 1,
                                "progress": progress,
                                "fallback": {
                                    "from": "direct",
                                    "to": "mir",
                                    "reason": reason,
                                },
                            })
                        );
                    }
                    if let Value::Int(code) = output.value {
                        process::exit(code.as_i128().unwrap_or(1) as i32);
                    }
                }
                Err(mut error) => {
                    if let Some((progress, reason)) = json_native_fallback {
                        for message in progress {
                            error = error.with_note(message);
                        }
                        error = error.with_note(format!(
                            "direct backend unavailable; MIR fallback reason: {reason}"
                        ));
                    }
                    emit_diagnostic(diagnostic_format, &input.path, &input.source, &error);
                    process::exit(1);
                }
            }
        }
        "build" => {
            let remaining = args.collect::<Vec<_>>();
            let (diagnostic_format, remaining) = parse_diagnostic_format(remaining);
            let (output_path, backend, input_args) = parse_build_args(remaining);
            let input = read_input(&mut input_args.into_iter());
            let result = if input.from_stdin {
                lower_path_with_source_to_mir(Path::new(&input.path), &input.source)
            } else {
                lower_path_to_mir(Path::new(&input.path))
            };
            match result {
                Ok(mir) => {
                    let mut build_progress = Vec::new();
                    let build = {
                        let mut progress_reporter = NativeProgressReporter {
                            report_human: diagnostic_format == DiagnosticFormat::Human,
                            messages: &mut build_progress,
                            wait_reported: false,
                            rebuild_reported: false,
                        };
                        build_binary_with_backend(
                            &input.path,
                            &input.source,
                            &mir,
                            &output_path,
                            backend,
                            || progress_reporter.wait(),
                        )
                    };
                    match build {
                        Ok(outcome) => {
                            if let Some(reason) = outcome.fallback_reason {
                                eprintln!(
                                    "aura: direct backend failed; using MIR runtime fallback:\n{}",
                                    reason
                                );
                            }
                            eprintln!(
                                "aura: built `{}` with {} backend",
                                output_path.display(),
                                match outcome.selected {
                                    SelectedBuildBackend::Direct => "direct",
                                    SelectedBuildBackend::MirRuntime => "MIR runtime",
                                }
                            );
                        }
                        Err(message) => {
                            let mut error = Diagnostic::new(message);
                            if diagnostic_format == DiagnosticFormat::Json {
                                for progress in build_progress {
                                    error = error.with_note(progress);
                                }
                            }
                            emit_diagnostic(diagnostic_format, &input.path, &input.source, &error);
                            process::exit(1);
                        }
                    }
                }
                Err(error) => {
                    emit_diagnostic(diagnostic_format, &input.path, &input.source, &error);
                    process::exit(1);
                }
            }
        }
        "ast" => {
            let input = read_input(&mut args);
            match parse_source(&input.source) {
                Ok(module) => {
                    write_stdout(&format!("{:#?}\n", module));
                }
                Err(error) => {
                    eprintln!("{}", render_error(&input.path, &input.source, &error));
                    process::exit(1);
                }
            }
        }
        "ast-json" => {
            let input = read_input(&mut args);
            match parse_source(&input.source) {
                Ok(module) => {
                    match serde_json::to_string_pretty(&module) {
                        Ok(json) => write_stdout(&json),
                        Err(error) => {
                            eprintln!("failed to serialize AST to JSON: {}", error);
                            process::exit(1);
                        }
                    }
                    write_stdout("\n");
                }
                Err(error) => {
                    eprintln!("{}", render_error(&input.path, &input.source, &error));
                    process::exit(1);
                }
            }
        }
        "mir" => {
            let input = read_input(&mut args);
            let result = if input.from_stdin {
                lower_path_with_source_to_mir(Path::new(&input.path), &input.source)
            } else {
                lower_path_to_mir(Path::new(&input.path))
            };
            match result {
                Ok(module) => {
                    write_stdout(&format!("{:#?}\n", module));
                }
                Err(error) => {
                    eprintln!("{}", render_error(&input.path, &input.source, &error));
                    process::exit(1);
                }
            }
        }
        "analyze" => {
            let input = read_input(&mut args);
            let analysis = analyze_path_source(Path::new(&input.path), &input.source);
            match serde_json::to_string(&analysis) {
                Ok(json) => write_stdout(&json),
                Err(error) => {
                    eprintln!("failed to serialize analysis to JSON: {}", error);
                    process::exit(1);
                }
            }
            write_stdout("\n");
        }
        "complete" => {
            let remaining = args.collect::<Vec<_>>();
            let (line, character, trigger, input_args) = parse_complete_args(remaining);
            let input = read_input(&mut input_args.into_iter());
            match complete_path_source(
                Path::new(&input.path),
                &input.source,
                line,
                character,
                trigger,
            ) {
                Ok(completions) => {
                    match serde_json::to_string(&completions) {
                        Ok(json) => write_stdout(&json),
                        Err(error) => {
                            eprintln!("failed to serialize completions to JSON: {}", error);
                            process::exit(1);
                        }
                    }
                    write_stdout("\n");
                }
                Err(error) => {
                    eprintln!("{}", render_error(&input.path, &input.source, &error));
                    process::exit(1);
                }
            }
        }
        _ => print_usage_and_exit(2),
    }
}

const DEFAULT_UPGRADE_INSTALLER_URL: &str = "https://johnolafenwa.github.io/Aura/install.sh";
const MAX_UPGRADE_INSTALLER_BYTES: u64 = 1024 * 1024;

struct UpgradeWorkspace {
    path: PathBuf,
}

impl Drop for UpgradeWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn handle_upgrade_command(args: Vec<String>) {
    if !args.is_empty() {
        eprintln!("`aura upgrade` does not accept arguments");
        process::exit(2);
    }

    if let Err(error) = run_upgrade() {
        eprintln!("Aura upgrade failed: {error}");
        process::exit(1);
    }
    write_stdout("Aura upgrade complete\n");
}

fn run_upgrade() -> Result<(), String> {
    let installer_url = std::env::var("AURA_UPGRADE_INSTALLER_URL")
        .unwrap_or_else(|_| DEFAULT_UPGRADE_INSTALLER_URL.to_string());
    let workspace = create_upgrade_workspace()
        .map_err(|error| format!("could not create a private temporary directory: {error}"))?;
    let installer_path = workspace.path.join("install.sh");

    write_stdout(&format!(
        "Downloading the Aura installer from {installer_url}\n"
    ));
    let download = Command::new("curl")
        .args(["-fsSL", &installer_url, "-o"])
        .arg(&installer_path)
        .status()
        .map_err(|error| format!("could not start curl; install curl and try again: {error}"))?;
    if !download.success() {
        return Err(format!(
            "could not download the installer (curl exited with {download})"
        ));
    }

    let metadata = fs::symlink_metadata(&installer_path)
        .map_err(|error| format!("could not inspect the downloaded installer: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("the downloaded installer is not a regular file".to_string());
    }
    if metadata.len() == 0 || metadata.len() > MAX_UPGRADE_INSTALLER_BYTES {
        return Err(format!(
            "the downloaded installer has an invalid size of {} bytes",
            metadata.len()
        ));
    }

    let mut install = Command::new("sh");
    install.arg(&installer_path);
    if std::env::var_os("AURA_INSTALL_PREFIX").is_none() {
        if let Some(prefix) = active_install_prefix() {
            install.env("AURA_INSTALL_PREFIX", prefix);
        }
    }
    let status = install
        .status()
        .map_err(|error| format!("could not start the installer with `sh`: {error}"))?;
    if !status.success() {
        return Err(format!("the installer exited with {status}"));
    }
    Ok(())
}

fn active_install_prefix() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let bin_directory = executable.parent()?;
    if bin_directory.file_name()?.to_str()? != "bin" {
        return None;
    }
    bin_directory.parent().map(Path::to_path_buf)
}

fn create_upgrade_workspace() -> io::Result<UpgradeWorkspace> {
    let root = std::env::temp_dir();
    for attempt in 0..16_u32 {
        let path = root.join(format!(
            "aura-upgrade-{}-{}-{attempt}",
            std::process::id(),
            system_time_nanos()
        ));
        match create_private_directory(&path) {
            Ok(()) => return Ok(UpgradeWorkspace { path }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique upgrade workspace",
    ))
}

fn handle_new_command(args: Vec<String>) {
    let [path] = args.as_slice() else {
        eprintln!("usage: aura new <project-path>");
        process::exit(2);
    };
    let project = PathBuf::from(path);
    if project.exists() {
        eprintln!(
            "refusing to overwrite existing path `{}`",
            project.display()
        );
        process::exit(1);
    }
    let Some(package_name) = project.file_name().and_then(|name| name.to_str()) else {
        eprintln!("project path must end in a valid UTF-8 package name");
        process::exit(2);
    };
    let valid_name = package_name.chars().enumerate().all(|(index, character)| {
        character.is_ascii_alphanumeric() || character == '_' || (character == '-' && index > 0)
    }) && package_name
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic());
    if !valid_name {
        eprintln!(
            "package name `{package_name}` must start with a letter and contain only ASCII letters, digits, `_`, or `-`"
        );
        process::exit(2);
    }
    let manifest_name = package_name.replace('-', "_");

    let source_dir = project.join("src");
    let tests_dir = project.join("tests");
    if let Err(error) =
        fs::create_dir_all(&source_dir).and_then(|()| fs::create_dir_all(&tests_dir))
    {
        eprintln!("failed to create `{}`: {error}", source_dir.display());
        process::exit(1);
    }
    let manifest =
        format!("[package]\nname = \"{manifest_name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n");
    if let Err(error) = fs::write(project.join("Aura.toml"), manifest)
        .and_then(|()| fs::write(project.join(".gitignore"), "target/\n"))
        .and_then(|()| {
            fs::write(
                source_dir.join("main.au"),
                "def main() -> int32:\n    print(\"Hello from Aura\")\n    return 0\n",
            )
        })
        .and_then(|()| {
            fs::write(
                tests_dir.join("smoke.au"),
                "def main() -> int32:\n    return 0\n",
            )
        })
    {
        let _ = fs::remove_dir_all(&project);
        eprintln!(
            "failed to create Aura project `{}`: {error}",
            project.display()
        );
        process::exit(1);
    }
    write_stdout(&format!("created `{}`\n", project.display()));
}

fn handle_fmt_command(args: Vec<String>) {
    let mut check_only = false;
    let mut inputs = Vec::new();
    for argument in args {
        if argument == "--check" {
            check_only = true;
        } else if argument.starts_with('-') {
            eprintln!("unknown aura fmt option `{argument}`");
            process::exit(2);
        } else {
            inputs.push(PathBuf::from(argument));
        }
    }
    if inputs.is_empty() {
        inputs.push(PathBuf::from("."));
    }
    let paths = collect_aura_source_paths(&inputs).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    let mut changed = Vec::new();
    for path in paths {
        let source = fs::read_to_string(&path).unwrap_or_else(|error| {
            eprintln!("failed to read `{}`: {error}", path.display());
            process::exit(1);
        });
        let formatted = format_aura_source(&source);
        if let Err(error) = parse_source(&formatted) {
            eprintln!(
                "{}",
                error.render_with_source(&path.display().to_string(), &formatted)
            );
            process::exit(1);
        }
        if source != formatted {
            changed.push(path.clone());
            if !check_only {
                fs::write(&path, formatted).unwrap_or_else(|error| {
                    eprintln!("failed to write `{}`: {error}", path.display());
                    process::exit(1);
                });
            }
        }
    }
    if check_only && !changed.is_empty() {
        for path in changed {
            eprintln!("would format `{}`", path.display());
        }
        process::exit(1);
    }
}

fn format_aura_source(source: &str) -> String {
    // Triple-quoted string contents are exact source data, including trailing
    // spaces and tabs on physical lines. Until the formatter has a token-aware
    // whitespace pass, preserve the entire file whenever one is present.
    if source.contains("\"\"\"") || source.contains("'''") {
        let mut formatted = source.trim_end_matches('\r').to_string();
        if !formatted.is_empty() && !formatted.ends_with('\n') {
            formatted.push('\n');
        }
        return formatted;
    }
    let mut formatted = source
        .lines()
        .map(|line| line.trim_end_matches([' ', '\t', '\r']))
        .collect::<Vec<_>>()
        .join("\n");
    if !formatted.is_empty() {
        formatted.push('\n');
    }
    formatted
}

fn collect_aura_source_paths(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    fn visit(path: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
        if path.is_file() {
            if path.extension().and_then(|extension| extension.to_str()) == Some("au") {
                paths.push(path.to_path_buf());
            }
            return Ok(());
        }
        if !path.is_dir() {
            return Err(format!(
                "Aura source path `{}` does not exist",
                path.display()
            ));
        }
        for entry in fs::read_dir(path)
            .map_err(|error| format!("failed to read `{}`: {error}", path.display()))?
        {
            let entry = entry.map_err(|error| {
                format!("failed to read entry under `{}`: {error}", path.display())
            })?;
            let child = entry.path();
            let name = child
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if child.is_dir()
                && (name.starts_with('.') || matches!(name, "target" | "node_modules"))
            {
                continue;
            }
            visit(&child, paths)?;
        }
        Ok(())
    }

    let mut paths = Vec::new();
    for input in inputs {
        visit(input, &mut paths)?;
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn handle_test_command(args: Vec<String>) {
    let mut timeout_ms = 30_000u64;
    let mut filter = None;
    let mut format = TestReportFormat::Human;
    let mut inputs = Vec::new();
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        if argument == "--timeout-ms" {
            timeout_ms = args
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or_else(|| {
                    eprintln!("aura test --timeout-ms requires a positive integer");
                    process::exit(2);
                });
        } else if argument == "-k" {
            if filter.is_some() {
                eprintln!("aura test -k may appear only once");
                process::exit(2);
            }
            let value = args.next().unwrap_or_else(|| {
                eprintln!("aura test -k requires a non-empty substring");
                process::exit(2);
            });
            if value.is_empty() {
                eprintln!("aura test -k requires a non-empty substring");
                process::exit(2);
            }
            filter = Some(value);
        } else if argument == "--format" {
            let value = args.next().unwrap_or_else(|| {
                eprintln!("aura test --format requires `json`");
                process::exit(2);
            });
            if value != "json" || format == TestReportFormat::Json {
                eprintln!("aura test --format accepts `json` exactly once");
                process::exit(2);
            }
            format = TestReportFormat::Json;
        } else if argument.starts_with('-') {
            eprintln!("unknown aura test option `{argument}`");
            process::exit(2);
        } else {
            inputs.push(PathBuf::from(argument));
        }
    }
    let inputs = if inputs.is_empty() {
        vec![PathBuf::from("tests")]
    } else {
        inputs
    };
    let paths = collect_aura_source_paths(&inputs).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    if paths.is_empty() {
        eprintln!("no Aura test files found");
        process::exit(1);
    }

    let mut records = Vec::new();
    let mut discovery_outputs = Vec::new();
    for path in paths {
        let source = fs::read_to_string(&path).unwrap_or_else(|error| {
            eprintln!("failed to read `{}`: {error}", path.display());
            process::exit(1);
        });
        let rendered = normalized_test_display_path(&path);

        let checked = match lower_path_to_checked_mir(&path) {
            Ok(module) => module,
            Err(error) => {
                records.push(TestRecord::failed_diagnostic(
                    rendered.clone(),
                    rendered,
                    0,
                    error,
                    source,
                ));
                continue;
            }
        };
        let syntax = parse_source(&source)
            .expect("a source module that lowered successfully must still parse successfully");
        let checked = std::sync::Arc::new(checked);

        let hooks = match discover_test_hooks(&syntax) {
            Ok(hooks) => hooks,
            Err(error) => {
                records.push(TestRecord::failed_diagnostic(
                    rendered.clone(),
                    rendered,
                    0,
                    error,
                    source,
                ));
                continue;
            }
        };
        let discovery = match discover_test_cases(&path, checked.clone(), &syntax, timeout_ms) {
            Ok(discovery) => discovery,
            Err(failure) => {
                let registration_name = failure.name.clone();
                records.push(TestRecord::from_execution(
                    registration_name,
                    rendered,
                    source,
                    failure.execution,
                ));
                continue;
            }
        };
        let declared_tests = discovery.declared_tests;
        let cases = discovery.cases;
        discovery_outputs.extend(discovery.outputs);
        let cases = if !declared_tests {
            vec![DiscoveredTestCase {
                name: rendered.clone(),
                entry: None,
            }]
        } else {
            cases
        };
        for case in cases {
            if !test_name_matches_filter(&case.name, filter.as_deref()) {
                continue;
            }
            let execution = run_test_case(
                &path,
                checked.clone(),
                case.entry,
                hooks.clone(),
                timeout_ms,
            );
            records.push(TestRecord::from_execution(
                case.name,
                rendered.clone(),
                source.clone(),
                execution,
            ));
        }
    }

    let passed = records
        .iter()
        .filter(|record| record.failure.is_none())
        .count();
    let failed = records.len() - passed;
    match format {
        TestReportFormat::Human => {
            for output in &discovery_outputs {
                write_stdout(&output.stdout);
            }
            for record in &records {
                record.write_human();
            }
            write_stdout(&format!("{passed} passed; {failed} failed\n"));
        }
        TestReportFormat::Json => {
            let tests = records.iter().map(TestRecord::json).collect::<Vec<_>>();
            let mut report = serde_json::json!({
                "schema_version": 1,
                "summary": {
                    "selected": records.len(),
                    "passed": passed,
                    "failed": failed,
                },
                "tests": tests,
            });
            if !discovery_outputs.is_empty() {
                report["discovery"] = JsonValue::Array(
                    discovery_outputs
                        .iter()
                        .map(TestDiscoveryOutput::json)
                        .collect(),
                );
            }
            write_stdout(&format!("{report}\n"));
        }
    }
    if failed > 0 {
        process::exit(1);
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TestReportFormat {
    Human,
    Json,
}

#[derive(Clone, Default)]
struct TestHooks {
    setup: Option<String>,
    teardown: Option<String>,
}

struct DiscoveredTestCase {
    name: String,
    entry: Option<String>,
}

struct DiscoveryFailure {
    name: String,
    execution: TestExecution,
}

struct TestDiscovery {
    declared_tests: bool,
    cases: Vec<DiscoveredTestCase>,
    outputs: Vec<TestDiscoveryOutput>,
}

struct TestDiscoveryOutput {
    name: String,
    file: String,
    stdout: String,
}

impl TestDiscoveryOutput {
    fn json(&self) -> JsonValue {
        serde_json::json!({
            "name": self.name,
            "file": self.file,
            "stdout": self.stdout,
        })
    }
}

fn test_name_matches_filter(name: &str, filter: Option<&str>) -> bool {
    filter.is_none_or(|filter| name.contains(filter))
}

fn normalized_test_display_path(path: &Path) -> String {
    let absolute = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let display = std::env::current_dir()
        .ok()
        .and_then(|cwd| fs::canonicalize(cwd).ok())
        .and_then(|cwd| absolute.strip_prefix(cwd).ok().map(Path::to_path_buf))
        .unwrap_or(absolute);
    display.to_string_lossy().replace('\\', "/")
}

fn discover_test_hooks(syntax: &aura_compiler::ast::Module) -> Result<TestHooks, Diagnostic> {
    use aura_compiler::ast::{AssignTarget, Item, Stmt};

    for constant in &syntax.constants {
        if matches!(constant.name.as_str(), "setup" | "teardown") {
            return Err(Diagnostic::coded_at(
                "AU2999",
                constant.span,
                format!("test hook `{}` must be a module function", constant.name),
            ));
        }
    }
    for item in &syntax.items {
        if !matches!(item, Item::Function(_)) && matches!(item.name(), "setup" | "teardown") {
            return Err(Diagnostic::coded_at(
                "AU2999",
                match item {
                    Item::Class(decl) => decl.span,
                    Item::Enum(decl) => decl.span,
                    Item::ExternFunction(decl) => decl.name_span,
                    Item::ExternOpaqueClass(decl) => decl.name_span,
                    Item::Trait(decl) => decl.span,
                    Item::Impl(decl) => decl.span,
                    Item::Function(_) => unreachable!("function items were excluded"),
                },
                format!("test hook `{}` must be a module function", item.name()),
            ));
        }
    }
    for statement in &syntax.top_level_stmts {
        let (name, span) = match statement {
            Stmt::Assign(assign) => match &assign.target {
                AssignTarget::Name(name) => (name.as_str(), assign.span),
                _ => continue,
            },
            Stmt::Destructure(destructure) => match destructure.target.name() {
                Some(name) => (name, destructure.span),
                None => continue,
            },
            _ => continue,
        };
        if matches!(name, "setup" | "teardown") {
            return Err(Diagnostic::coded_at(
                "AU2999",
                span,
                format!("test hook `{name}` must be a module function"),
            ));
        }
    }

    let mut hooks = TestHooks::default();
    for function in syntax.items.iter().filter_map(|item| match item {
        Item::Function(function) => Some(function),
        _ => None,
    }) {
        let slot = match function.name.as_str() {
            "setup" => &mut hooks.setup,
            "teardown" => &mut hooks.teardown,
            _ => continue,
        };
        if function.receiver.is_some()
            || !function.params.is_empty()
            || !is_none_type_ref(&function.return_type)
        {
            return Err(Diagnostic::coded_at(
                "AU2999",
                function.span,
                format!(
                    "test hook `{}` must be parameterless and return `None`",
                    function.name
                ),
            ));
        }
        *slot = Some(function.name.clone());
    }
    Ok(hooks)
}

fn discover_test_cases(
    path: &Path,
    checked: std::sync::Arc<CheckedMirModule>,
    syntax: &aura_compiler::ast::Module,
    timeout_ms: u64,
) -> Result<TestDiscovery, Box<DiscoveryFailure>> {
    use aura_compiler::ast::Item;

    let rendered = normalized_test_display_path(path);
    let mut cases = Vec::new();
    let mut outputs = Vec::new();
    let test_functions = syntax
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) if function.name.starts_with("test_") => Some(function),
            _ => None,
        })
        .collect::<Vec<_>>();
    let declared_tests = !test_functions.is_empty();
    for function in test_functions {
        let base_name = format!("{rendered}::{}", function.name);
        if !function.params.is_empty() {
            return Err(discovery_reason(
                base_name,
                format!("test function `{}` must be parameterless", function.name),
            ));
        }
        if is_none_type_ref(&function.return_type) {
            cases.push(DiscoveredTestCase {
                name: base_name,
                entry: Some(function.name.clone()),
            });
            continue;
        }
        if !is_test_registration_type_ref(&function.return_type) {
            return Err(discovery_reason(
                base_name,
                format!(
                    "test function `{}` must return `None` or `list[(str, def() -> None)]`",
                    function.name
                ),
            ));
        }

        let registration = run_test_entry_with_timeout(
            path,
            checked.clone(),
            Some(function.name.clone()),
            timeout_ms,
        );
        let registration_stdout = registration.stdout.clone();
        let output = match registration.failure {
            None => registration.value.expect("a successful entry has a value"),
            Some(failure) => {
                return Err(Box::new(DiscoveryFailure {
                    name: base_name,
                    execution: TestExecution {
                        failure: Some(failure),
                        secondary_failure: None,
                        stdout: registration.stdout,
                        duration_ms: registration.duration_ms,
                    },
                }));
            }
        };
        if !registration_stdout.is_empty() {
            outputs.push(TestDiscoveryOutput {
                name: base_name.clone(),
                file: rendered.clone(),
                stdout: registration_stdout,
            });
        }
        let Value::Vec(entries) = output else {
            return Err(discovery_reason(
                base_name,
                "test registration did not return a list".to_string(),
            ));
        };
        let mut labels = std::collections::BTreeSet::new();
        for entry in entries.elements {
            let Value::Tuple(mut tuple) = entry else {
                return Err(discovery_reason(
                    base_name,
                    "test registration entries must be `(str, def() -> None)` tuples".to_string(),
                ));
            };
            if tuple.elements.len() != 2 {
                return Err(discovery_reason(
                    base_name,
                    "test registration entries must contain exactly a label and case".to_string(),
                ));
            }
            let case_value = tuple.elements.pop().expect("tuple length checked");
            let label_value = tuple.elements.pop().expect("tuple length checked");
            let Value::String(label) = label_value else {
                return Err(discovery_reason(
                    base_name,
                    "test registration labels must be strings".to_string(),
                ));
            };
            if label.is_empty() {
                return Err(discovery_reason(
                    base_name,
                    "test registration labels must be non-empty".to_string(),
                ));
            }
            if !labels.insert(label.clone()) {
                return Err(discovery_reason(
                    base_name,
                    format!("duplicate test registration label `{label}`"),
                ));
            }
            let Value::Function(case) = case_value else {
                return Err(discovery_reason(
                    base_name,
                    "test registration cases must be function values".to_string(),
                ));
            };
            if !is_parameterless_none_callable(&case.signature) {
                return Err(discovery_reason(
                    base_name,
                    format!("registered case `{label}` must be parameterless and return `None`"),
                ));
            }
            cases.push(DiscoveredTestCase {
                name: format!("{base_name}[{label}]"),
                entry: Some(case.name),
            });
        }
    }
    Ok(TestDiscovery {
        declared_tests,
        cases,
        outputs,
    })
}

fn is_none_type_ref(ty: &aura_compiler::ast::TypeRef) -> bool {
    matches!(
        &ty.kind,
        aura_compiler::ast::TypeRefKind::Named { name, args }
            if name == "None" && args.is_empty()
    )
}

fn is_test_registration_type_ref(ty: &aura_compiler::ast::TypeRef) -> bool {
    use aura_compiler::ast::TypeRefKind;
    let TypeRefKind::Named { name, args } = &ty.kind else {
        return false;
    };
    if name != "list" || args.len() != 1 {
        return false;
    }
    let TypeRefKind::Tuple(elements) = &args[0].kind else {
        return false;
    };
    if elements.len() != 2 {
        return false;
    }
    let label_is_str = matches!(
        &elements[0].kind,
        TypeRefKind::Named { name, args } if name == "str" && args.is_empty()
    );
    let case_is_function = matches!(
        &elements[1].kind,
        TypeRefKind::Function { params, return_type }
            if params.is_empty() && is_none_type_ref(return_type)
    );
    label_is_str && case_is_function
}

fn is_parameterless_none_callable(ty: &aura_compiler::sema::Type) -> bool {
    use aura_compiler::sema::Type;
    match ty {
        Type::Function {
            params,
            return_type,
        } => params.is_empty() && **return_type == Type::Unit,
        _ => false,
    }
}

fn discovery_reason(name: String, reason: String) -> Box<DiscoveryFailure> {
    Box::new(DiscoveryFailure {
        name,
        execution: TestExecution::failed(TestFailure::Reason(reason), 0, String::new()),
    })
}

enum TestFailure {
    Reason(String),
    Diagnostic(Diagnostic),
}

struct TestEntryExecution {
    value: Option<Value>,
    failure: Option<TestFailure>,
    stdout: String,
    duration_ms: u64,
}

struct TestExecution {
    failure: Option<TestFailure>,
    secondary_failure: Option<TestFailure>,
    stdout: String,
    duration_ms: u64,
}

impl TestExecution {
    fn failed(failure: TestFailure, duration_ms: u64, stdout: String) -> Self {
        Self {
            failure: Some(failure),
            secondary_failure: None,
            stdout,
            duration_ms,
        }
    }
}

fn run_test_case(
    path: &Path,
    checked: std::sync::Arc<CheckedMirModule>,
    entry: Option<String>,
    hooks: TestHooks,
    timeout_ms: u64,
) -> TestExecution {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let captured_stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let worker_stdout = captured_stdout.clone();
    let started_at = std::time::Instant::now();
    let started = std::thread::Builder::new()
        .name(format!("aura-test-{}", path.display()))
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let mut primary = None;
            let mut secondary = None;
            if let Some(setup) = hooks.setup.as_deref() {
                let stage = run_test_entry(
                    &checked,
                    Some(setup),
                    Some(test_stdout_sink(worker_stdout.clone())),
                );
                primary = stage.failure;
            }
            if primary.is_none() {
                let stage = run_test_entry(
                    &checked,
                    entry.as_deref(),
                    Some(test_stdout_sink(worker_stdout.clone())),
                );
                primary = stage.failure;
            }
            if let Some(teardown) = hooks.teardown.as_deref() {
                let stage = run_test_entry(
                    &checked,
                    Some(teardown),
                    Some(test_stdout_sink(worker_stdout)),
                );
                if let Some(teardown_failure) = stage.failure {
                    if primary.is_some() {
                        secondary = Some(teardown_failure);
                    } else {
                        primary = Some(teardown_failure);
                    }
                }
            }
            let _ = sender.send((primary, secondary));
        });
    if let Err(error) = started {
        return TestExecution::failed(
            TestFailure::Reason(format!("failed to start test: {error}")),
            0,
            String::new(),
        );
    }

    match receiver.recv_timeout(std::time::Duration::from_millis(timeout_ms)) {
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => TestExecution::failed(
            TestFailure::Reason(format!("timed out after {timeout_ms}ms")),
            elapsed_millis(started_at),
            snapshot_test_stdout(&captured_stdout),
        ),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => TestExecution::failed(
            TestFailure::Reason("test worker disconnected".to_string()),
            elapsed_millis(started_at),
            snapshot_test_stdout(&captured_stdout),
        ),
        Ok((failure, secondary_failure)) => TestExecution {
            failure,
            secondary_failure,
            stdout: snapshot_test_stdout(&captured_stdout),
            duration_ms: elapsed_millis(started_at),
        },
    }
}

fn test_stdout_sink(stdout: std::sync::Arc<std::sync::Mutex<String>>) -> aura_compiler::StdoutSink {
    std::sync::Arc::new(move |chunk: &str| {
        stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_str(chunk);
    })
}

fn snapshot_test_stdout(stdout: &std::sync::Arc<std::sync::Mutex<String>>) -> String {
    stdout
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn run_test_entry(
    checked: &CheckedMirModule,
    entry: Option<&str>,
    stdout_sink: Option<aura_compiler::StdoutSink>,
) -> TestEntryExecution {
    let result = run_checked_mir_entry_with_stdout_sink_and_program_args(
        checked,
        entry,
        stdout_sink,
        Vec::new(),
    );
    match result {
        Ok(output) => {
            let failure = match &output.value {
                Value::Int(code) if code.as_i128() != Some(0) => {
                    Some(TestFailure::Reason("non-zero main return".to_string()))
                }
                _ => None,
            };
            TestEntryExecution {
                value: Some(output.value),
                failure,
                stdout: String::new(),
                duration_ms: 0,
            }
        }
        Err(error) => TestEntryExecution {
            value: None,
            failure: Some(TestFailure::Diagnostic(error)),
            stdout: String::new(),
            duration_ms: 0,
        },
    }
}

fn run_test_entry_with_timeout(
    path: &Path,
    checked: std::sync::Arc<CheckedMirModule>,
    entry: Option<String>,
    timeout_ms: u64,
) -> TestEntryExecution {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let path = path.to_path_buf();
    let captured_stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let worker_stdout = captured_stdout.clone();
    let started_at = std::time::Instant::now();
    let started = std::thread::Builder::new()
        .name(format!("aura-test-registration-{}", path.display()))
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let _ = sender.send(run_test_entry(
                &checked,
                entry.as_deref(),
                Some(test_stdout_sink(worker_stdout)),
            ));
        });
    if let Err(error) = started {
        return TestEntryExecution {
            value: None,
            failure: Some(TestFailure::Reason(format!(
                "failed to start test registration: {error}"
            ))),
            stdout: snapshot_test_stdout(&captured_stdout),
            duration_ms: 0,
        };
    }
    match receiver.recv_timeout(std::time::Duration::from_millis(timeout_ms)) {
        Ok(mut execution) => {
            execution.stdout = snapshot_test_stdout(&captured_stdout);
            execution.duration_ms = elapsed_millis(started_at);
            execution
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => TestEntryExecution {
            value: None,
            failure: Some(TestFailure::Reason(format!(
                "test registration timed out after {timeout_ms}ms"
            ))),
            stdout: snapshot_test_stdout(&captured_stdout),
            duration_ms: elapsed_millis(started_at),
        },
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => TestEntryExecution {
            value: None,
            failure: Some(TestFailure::Reason(
                "test registration worker disconnected".to_string(),
            )),
            stdout: snapshot_test_stdout(&captured_stdout),
            duration_ms: elapsed_millis(started_at),
        },
    }
}

fn elapsed_millis(started_at: std::time::Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

struct TestRecord {
    name: String,
    file: String,
    source: String,
    duration_ms: u64,
    stdout: String,
    failure: Option<TestFailure>,
    secondary_failure: Option<TestFailure>,
}

impl TestRecord {
    fn from_execution(
        name: String,
        file: String,
        source: String,
        execution: TestExecution,
    ) -> Self {
        Self {
            name,
            file,
            source,
            duration_ms: execution.duration_ms,
            stdout: execution.stdout,
            failure: execution.failure,
            secondary_failure: execution.secondary_failure,
        }
    }

    fn failed_diagnostic(
        name: String,
        file: String,
        duration_ms: u64,
        diagnostic: Diagnostic,
        source: String,
    ) -> Self {
        Self {
            name,
            file,
            source,
            duration_ms,
            stdout: String::new(),
            failure: Some(TestFailure::Diagnostic(diagnostic)),
            secondary_failure: None,
        }
    }

    fn write_human(&self) {
        if !self.stdout.is_empty() {
            write_stdout(&self.stdout);
        }
        match &self.failure {
            None => write_stdout(&format!("ok {}\n", self.name)),
            Some(TestFailure::Reason(reason)) => eprintln!("FAILED {} ({reason})", self.name),
            Some(TestFailure::Diagnostic(error)) => eprintln!(
                "FAILED {}\n{}",
                self.name,
                error.render_with_source(&self.file, &self.source)
            ),
        }
        if let Some(secondary) = &self.secondary_failure {
            match secondary {
                TestFailure::Reason(reason) => {
                    eprintln!("teardown also failed for {} ({reason})", self.name)
                }
                TestFailure::Diagnostic(error) => eprintln!(
                    "teardown also failed for {}\n{}",
                    self.name,
                    error.render_with_source(&self.file, &self.source)
                ),
            }
        }
    }

    fn json(&self) -> JsonValue {
        let mut record = serde_json::json!({
            "name": self.name,
            "file": self.file,
            "outcome": if self.failure.is_some() { "failed" } else { "passed" },
            "duration_ms": self.duration_ms,
        });
        match &self.failure {
            None => {}
            Some(TestFailure::Reason(reason)) => record["reason"] = JsonValue::from(reason.clone()),
            Some(TestFailure::Diagnostic(error)) => {
                record["diagnostic"] = serde_json::to_value(error.structured(&self.file))
                    .expect("structured diagnostics are JSON serializable");
            }
        }
        if !self.stdout.is_empty() {
            record["stdout"] = JsonValue::from(self.stdout.clone());
        }
        if let Some(secondary) = &self.secondary_failure {
            let mut rendered = serde_json::json!({"stage": "teardown"});
            match secondary {
                TestFailure::Reason(reason) => {
                    rendered["reason"] = JsonValue::from(reason.clone());
                }
                TestFailure::Diagnostic(error) => {
                    rendered["diagnostic"] = serde_json::to_value(error.structured(&self.file))
                        .expect("structured diagnostics are JSON serializable");
                }
            }
            record["secondary"] = rendered;
        }
        record
    }
}

fn handle_lsp_service() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    for line in stdin.lock().lines() {
        let response = match line {
            Ok(line) => lsp_response_for_line(&line),
            Err(error) => serde_json::json!({
                "id": JsonValue::Null,
                "semantic_interface_version": aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION,
                "error": format!("failed to read LSP compiler request: {error}")
            }),
        };
        let write_result = serde_json::to_writer(&mut writer, &response)
            .map_err(io::Error::other)
            .and_then(|_| writer.write_all(b"\n"))
            .and_then(|_| writer.flush());
        if let Err(error) = write_result {
            if error.kind() == io::ErrorKind::BrokenPipe {
                return;
            }
            eprintln!("failed to write LSP compiler response: {error}");
            process::exit(1);
        }
    }
}

fn lsp_response_for_line(line: &str) -> JsonValue {
    let request = match serde_json::from_str::<JsonValue>(line) {
        Ok(request) => request,
        Err(error) => {
            return serde_json::json!({
                "id": JsonValue::Null,
                "semantic_interface_version": aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION,
                "error": format!("invalid LSP compiler request JSON: {error}")
            });
        }
    };
    let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
    let result = lsp_result_for_request(&request);
    match result {
        Ok(result) => serde_json::json!({
            "id": id,
            "semantic_interface_version": aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION,
            "result": result
        }),
        Err(error) => serde_json::json!({
            "id": id,
            "semantic_interface_version": aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION,
            "error": error
        }),
    }
}

fn lsp_result_for_request(request: &JsonValue) -> Result<JsonValue, String> {
    let request_schema = request
        .get("semantic_interface_version")
        .and_then(JsonValue::as_u64);
    if request_schema != Some(u64::from(aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION)) {
        let reported = request_schema
            .map(|version| version.to_string())
            .unwrap_or_else(|| "<missing>".to_string());
        return Err(format!(
            "Aura semantic schema mismatch: client reported `{reported}`, compiler requires `{}`",
            aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION
        ));
    }
    let method = request
        .get("method")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "LSP compiler request requires string field `method`".to_string())?;
    let path = request
        .get("path")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "LSP compiler request requires string field `path`".to_string())?;
    let source = request
        .get("source")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "LSP compiler request requires string field `source`".to_string())?;

    match method {
        "analyze" => serde_json::to_value(analyze_path_source(Path::new(path), source))
            .map_err(|error| format!("failed to serialize compiler analysis: {error}")),
        "complete" => {
            let line = request
                .get("line")
                .and_then(JsonValue::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    "completion request requires non-negative integer field `line`".to_string()
                })?;
            let character = request
                .get("character")
                .and_then(JsonValue::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    "completion request requires non-negative integer field `character`".to_string()
                })?;
            let trigger = request
                .get("trigger")
                .and_then(JsonValue::as_str)
                .and_then(|value| value.chars().next());
            let completions =
                complete_path_source(Path::new(path), source, line, character, trigger)
                    .map_err(|error| error.render_with_source(path, source))?;
            serde_json::to_value(completions)
                .map_err(|error| format!("failed to serialize compiler completions: {error}"))
        }
        _ => Err(format!("unknown LSP compiler request method `{method}`")),
    }
}

fn handle_deps_command(args: Vec<String>) -> ! {
    let Some(subcommand) = args.first() else {
        print_usage_and_exit(2);
    };
    if subcommand != "update" || args.len() > 2 {
        print_usage_and_exit(2);
    }

    let target_package = args.get(1).map(String::as_str);
    let current_dir = std::env::current_dir().unwrap_or_else(|error| {
        eprintln!("failed to determine current directory: {}", error);
        process::exit(1);
    });

    match update_git_dependencies_in_working_dir(&current_dir, target_package) {
        Ok(result) => {
            if result.updated_packages.is_empty() {
                write_stdout("Aura.lock is already up to date\n");
            } else {
                for package in result.updated_packages {
                    write_stdout(&format!("updated {}\n", package));
                }
            }
            process::exit(0);
        }
        Err(error) => {
            eprintln!(
                "{}",
                error.render_with_source(&current_dir.display().to_string(), "")
            );
            process::exit(1);
        }
    }
}

fn parse_complete_args(args: Vec<String>) -> (usize, usize, Option<char>, Vec<String>) {
    let mut line = None;
    let mut character = None;
    let mut trigger = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--line" => {
                index += 1;
                line = Some(
                    args.get(index)
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or_else(|| print_usage_and_exit(2)),
                );
                index += 1;
            }
            "--character" => {
                index += 1;
                character = Some(
                    args.get(index)
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or_else(|| print_usage_and_exit(2)),
                );
                index += 1;
            }
            "--trigger" => {
                index += 1;
                trigger = Some(
                    args.get(index)
                        .and_then(|value| value.chars().next())
                        .unwrap_or_else(|| print_usage_and_exit(2)),
                );
                index += 1;
            }
            _ => break,
        }
    }

    (
        line.unwrap_or_else(|| print_usage_and_exit(2)),
        character.unwrap_or_else(|| print_usage_and_exit(2)),
        trigger,
        args[index..].to_vec(),
    )
}

/// The backend `aura run` uses to execute a program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunBackend {
    /// Execute the lowered MIR directly.
    Mir,
    /// Build a native binary with the direct backend and execute it.
    Direct,
    /// Prefer the direct backend and fall back to the MIR runtime, reporting
    /// the fallback on stderr.
    Auto,
}

enum NativeRunOutcome {
    /// The native binary ran to completion with this exit code.
    Ran(i32),
    /// The Aura program could not be lowered. Keep the compiler diagnostic
    /// intact rather than degrading it into a host-backend message.
    Diagnostic(Diagnostic),
    /// The native runtime reported an Aura trap over the private structured
    /// channel used by JSON-mode `aura run`.
    StructuredDiagnostic(Box<StructuredDiagnostic>),
    /// The requested backend could not produce a binary.
    Failed(String),
    /// `auto` could not use the direct backend and the MIR runtime should run.
    FellBack(String),
}

#[derive(Debug)]
struct VerifiedNativeLaunchError {
    message: String,
    invalidates_cache: bool,
    post_launch: bool,
}

impl VerifiedNativeLaunchError {
    fn environment(message: String) -> Self {
        Self {
            message,
            invalidates_cache: false,
            post_launch: false,
        }
    }

    fn launch(binary: &Path, error: io::Error) -> Self {
        Self {
            invalidates_cache: native_launch_error_invalidates_cache(&error),
            post_launch: false,
            message: format!(
                "failed to execute the verified direct binary `{}`: {}",
                binary.display(),
                error
            ),
        }
    }

    fn after_launch(message: String) -> Self {
        Self {
            message,
            invalidates_cache: false,
            post_launch: true,
        }
    }
}

/// `aura run`'s default backend.
///
/// This stays on the MIR runtime rather than `auto`. A cold miss costs about
/// 1.3s, and integrity-preserving warm hits measured about 0.81s after reading,
/// hashing, and privately materializing a roughly 57 MB direct hello-world
/// binary. CI and the test suites are also dominated by programs each seen
/// once. The earlier 0.01s resident-path measurement predates per-hit
/// verification and is not a current guarantee.
const DEFAULT_RUN_BACKEND: RunBackend = RunBackend::Mir;

fn parse_run_backend(args: Vec<String>) -> (RunBackend, Vec<String>) {
    let mut backend = DEFAULT_RUN_BACKEND;
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--backend" {
            index += 1;
            let value = args
                .get(index)
                .cloned()
                .unwrap_or_else(|| print_usage_and_exit(2));
            backend = match value.as_str() {
                "mir" => RunBackend::Mir,
                "direct" => RunBackend::Direct,
                "auto" => RunBackend::Auto,
                _ => print_usage_and_exit(2),
            };
            index += 1;
            continue;
        }
        rest.push(args[index].clone());
        index += 1;
    }
    (backend, rest)
}

/// Builds and executes the program through the direct native backend. Under
/// `auto` a build failure is reported as a fallback rather than an error, so
/// the MIR runtime still runs the program.
fn run_through_native_backend(
    input: &Input,
    program_args: &[String],
    backend: RunBackend,
    report_progress: bool,
    capture_structured_diagnostics: bool,
    progress: &mut Vec<String>,
) -> NativeRunOutcome {
    let mir = if input.from_stdin {
        lower_path_with_source_to_mir(Path::new(&input.path), &input.source)
    } else {
        lower_path_to_mir(Path::new(&input.path))
    };
    let mir = match mir {
        Ok(mir) => mir,
        // A compile failure is the program's, not the backend's, so it is
        // reported the same way whichever backend was requested.
        Err(error) => return NativeRunOutcome::Diagnostic(error),
    };

    let caching_available = native_cache_root().is_some();
    let mut launch_invalidated_entry = None;
    let mut progress = NativeProgressReporter {
        report_human: report_progress,
        messages: progress,
        wait_reported: false,
        rebuild_reported: false,
    };
    loop {
        // A stable runtime memo plus a fully verified program entry is enough
        // for a warm launch. The memo reader validates the archive stamp
        // before and after reading (or hashes stable bytes after a harmless
        // restamp), so this optimistic path need not wait behind unrelated
        // Cargo/runtime establishment. Misses and invalid launches still enter
        // the single-writer protocol below.
        if launch_invalidated_entry.is_none() {
            let optimistic_key = native_runtime_identity_for_cache()
                .as_ref()
                .and_then(|runtime_identity| native_cache_key(&mir, runtime_identity));
            if let Some(cached) = optimistic_key
                .as_deref()
                .and_then(peek_cached_native_binary)
            {
                match launch_verified_native_binary(
                    &cached,
                    program_args,
                    capture_structured_diagnostics,
                ) {
                    Ok(outcome) => return native_execution_outcome(outcome),
                    Err(error) if error.invalidates_cache => {
                        launch_invalidated_entry = Some(cached);
                    }
                    Err(error) => {
                        return verified_native_execution_failure(backend, error);
                    }
                }
            }
        }

        // Derive the content key under the short workspace-runtime lease. No
        // cache-key lock is held while this potentially waits, which keeps the
        // lock order acyclic across sibling processes.
        let acquired_runtime_lock = {
            let mut on_wait = || progress.wait();
            acquire_native_runtime_build_lock(Some(&mut on_wait))
        };
        let first_runtime_lock = match acquired_runtime_lock {
            Ok(lock) => lock,
            Err(error) => return native_backend_failure(backend, error),
        };
        let initial_key = native_runtime_identity_for_cache()
            .as_ref()
            .and_then(|runtime_identity| native_cache_key(&mir, runtime_identity));

        if initial_key.is_none() && caching_available {
            progress.rebuild();
            let established = establish_native_runtime_identity_memo();
            drop(first_runtime_lock);
            if let Err(error) = established {
                return native_backend_failure(backend, error);
            }
            // Re-enter through the ordinary key path. The shared memo now
            // supplies the same content identity to every cache root.
            continue;
        }
        drop(first_runtime_lock);

        let mut cache_lock = match initial_key.as_deref() {
            Some(key) => {
                let acquired_cache_lock = {
                    let mut on_wait = || progress.wait();
                    acquire_native_cache_build_lock(key, Some(&mut on_wait))
                };
                match acquired_cache_lock {
                    Ok(lock) => lock,
                    Err(error) => return native_backend_failure(backend, error),
                }
            }
            None => None,
        };

        // A waiter must recheck the runtime identity after obtaining the
        // per-key lease. If Cargo changed the content key meanwhile, release
        // this obsolete key and retry rather than publishing under it.
        let acquired_runtime_lock = {
            let mut on_wait = || progress.wait();
            acquire_native_runtime_build_lock(Some(&mut on_wait))
        };
        let runtime_lock = match acquired_runtime_lock {
            Ok(lock) => lock,
            Err(error) => return native_backend_failure(backend, error),
        };
        let cache_key = native_runtime_identity_for_cache()
            .as_ref()
            .and_then(|runtime_identity| native_cache_key(&mir, runtime_identity));
        if cache_key != initial_key {
            drop(runtime_lock);
            drop(cache_lock);
            continue;
        }

        if let Some(invalidated) = launch_invalidated_entry.take() {
            // A plausible native header can still fail the exec probe. Remove
            // exactly the entry we launched only after reacquiring the writer
            // lock; a concurrent verified replacement is preserved.
            invalidate_cached_native_binary(&invalidated);
        }

        if let Some(cached) = cache_key.as_deref().and_then(cached_native_binary) {
            // Launching is outside the establishment lock. Long-running Aura
            // programs must not prevent unrelated processes from consulting or
            // populating the cache after these bytes have been privately staged.
            drop(runtime_lock);
            drop(cache_lock);
            match launch_verified_native_binary(
                &cached,
                program_args,
                capture_structured_diagnostics,
            ) {
                Ok(outcome) => return native_execution_outcome(outcome),
                Err(error) if error.invalidates_cache => {
                    launch_invalidated_entry = Some(cached);
                    continue;
                }
                Err(error) => {
                    return verified_native_execution_failure(backend, error);
                }
            }
        }

        progress.rebuild();
        let output_path = temporary_run_binary_path(&input.path);
        let build = build_direct_native_binary_with_identity(
            &input.path,
            &input.source,
            &mir,
            &output_path,
            runtime_lock,
        );
        let build = build.and_then(|runtime_identity| {
            let final_key = native_cache_key(&mir, &runtime_identity);
            if final_key != cache_key {
                // Cargo may legitimately replace the runtime with different
                // bytes while establishing it. The shared memo is already
                // updated, so release the obsolete key before waiting for the
                // final content key; old-key waiters will recheck and follow.
                drop(cache_lock.take());
                cache_lock = match final_key.as_deref() {
                    Some(key) => {
                        let mut on_wait = || progress.wait();
                        acquire_native_cache_build_lock(key, Some(&mut on_wait))?
                    }
                    None => None,
                };
            }
            if let Some(key) = final_key {
                if cached_native_binary(&key).is_none() {
                    store_native_binary(&key, &output_path);
                }
            }
            Ok(())
        });
        // Cache establishment and the runtime-identity memo are complete.
        // Release before executing arbitrary user code.
        drop(cache_lock);
        let outcome = match build {
            Ok(()) => {
                launch_native_binary(&output_path, program_args, capture_structured_diagnostics)
                    .map(native_execution_outcome)
                    .unwrap_or_else(|error| native_execution_failure(backend, error))
            }
            Err(reason) => native_backend_failure(backend, reason),
        };
        let _ = fs::remove_file(&output_path);
        return outcome;
    }
}

struct NativeProgressReporter<'a> {
    report_human: bool,
    messages: &'a mut Vec<String>,
    wait_reported: bool,
    rebuild_reported: bool,
}

impl NativeProgressReporter<'_> {
    fn wait(&mut self) {
        if self.wait_reported {
            return;
        }
        self.wait_reported = true;
        self.report("aura: waiting for a concurrent build...");
    }

    fn rebuild(&mut self) {
        if self.rebuild_reported {
            return;
        }
        self.rebuild_reported = true;
        self.report("aura: building native program...");
    }

    fn report(&mut self, message: &str) {
        self.messages.push(message.to_string());
        if self.report_human {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "{message}");
            let _ = stderr.flush();
        }
    }
}

fn native_backend_failure(backend: RunBackend, reason: String) -> NativeRunOutcome {
    match backend {
        RunBackend::Auto => NativeRunOutcome::FellBack(reason),
        _ => NativeRunOutcome::Failed(reason),
    }
}

struct NativeExecutionError {
    message: String,
    post_launch: bool,
}

fn native_execution_failure(backend: RunBackend, error: NativeExecutionError) -> NativeRunOutcome {
    if error.post_launch {
        NativeRunOutcome::Failed(error.message)
    } else {
        native_backend_failure(backend, error.message)
    }
}

fn verified_native_execution_failure(
    backend: RunBackend,
    error: VerifiedNativeLaunchError,
) -> NativeRunOutcome {
    if error.post_launch {
        NativeRunOutcome::Failed(error.message)
    } else {
        native_backend_failure(backend, error.message)
    }
}

fn native_execution_outcome(outcome: NativeExecutionOutcome) -> NativeRunOutcome {
    match outcome {
        NativeExecutionOutcome::Exited(code) => NativeRunOutcome::Ran(code),
        NativeExecutionOutcome::Trapped(diagnostic) => {
            NativeRunOutcome::StructuredDiagnostic(diagnostic)
        }
    }
}

#[derive(Debug)]
enum NativeExecutionOutcome {
    Exited(i32),
    Trapped(Box<StructuredDiagnostic>),
}

struct SpawnedNativeBinary {
    child: process::Child,
    #[cfg(unix)]
    diagnostic_reader: Option<fs::File>,
    #[cfg(unix)]
    diagnostic_signal_reader: Option<fs::File>,
}

fn launch_native_binary(
    binary: &Path,
    program_args: &[String],
    capture_structured_diagnostics: bool,
) -> std::result::Result<NativeExecutionOutcome, NativeExecutionError> {
    let spawned = spawn_native_binary_with_diagnostic_mode(
        binary,
        program_args,
        capture_structured_diagnostics,
    )
    .map_err(|error| NativeExecutionError {
        message: format!(
            "failed to execute the direct binary `{}`: {}",
            binary.display(),
            error
        ),
        post_launch: false,
    })?;
    wait_for_native_binary(spawned).map_err(|error| NativeExecutionError {
        message: format!(
            "failed to collect the direct binary `{}`: {}",
            binary.display(),
            error
        ),
        post_launch: true,
    })
}

#[cfg(all(unix, test))]
fn spawn_verified_native_binary_with_lease(
    binary: &Path,
    program_args: &[String],
    lease: &fs::File,
) -> io::Result<process::Child> {
    use std::os::fd::AsRawFd;

    spawn_unix_native_binary_without_shell_fallback(
        binary,
        program_args,
        Some(lease.as_raw_fd()),
        false,
    )
    .map(|spawned| spawned.child)
}

#[cfg(unix)]
fn spawn_native_binary_with_diagnostic_mode(
    binary: &Path,
    program_args: &[String],
    capture_structured_diagnostics: bool,
) -> io::Result<SpawnedNativeBinary> {
    spawn_unix_native_binary_without_shell_fallback(
        binary,
        program_args,
        None,
        capture_structured_diagnostics,
    )
}

#[cfg(unix)]
fn spawn_verified_native_binary_with_diagnostic_mode(
    binary: &Path,
    program_args: &[String],
    lease: &fs::File,
    capture_structured_diagnostics: bool,
) -> io::Result<SpawnedNativeBinary> {
    use std::os::fd::AsRawFd;

    spawn_unix_native_binary_without_shell_fallback(
        binary,
        program_args,
        Some(lease.as_raw_fd()),
        capture_structured_diagnostics,
    )
}

#[cfg(unix)]
fn spawn_unix_native_binary_without_shell_fallback(
    binary: &Path,
    program_args: &[String],
    inherited_lease_fd: Option<std::os::fd::RawFd>,
    capture_structured_diagnostics: bool,
) -> io::Result<SpawnedNativeBinary> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    fn cloexec_pipe() -> io::Result<(fs::File, fs::File)> {
        let mut file_descriptors = [-1; 2];
        // SAFETY: `pipe` initializes both descriptors on success.
        if unsafe { libc::pipe(file_descriptors.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both descriptors were returned by the successful `pipe`.
        let reader = unsafe { fs::File::from_raw_fd(file_descriptors[0]) };
        let writer = unsafe { fs::File::from_raw_fd(file_descriptors[1]) };
        for fd in [reader.as_raw_fd(), writer.as_raw_fd()] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags == -1
                || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok((reader, writer))
    }

    let executable = CString::new(binary.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "executable path contains NUL"))?;
    let mut arguments = Vec::with_capacity(program_args.len() + 1);
    arguments.push(executable.clone());
    for argument in program_args {
        arguments.push(CString::new(argument.as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "program argument contains NUL")
        })?);
    }
    // Store pointer bits as `usize` so the pre-exec closure remains Send +
    // Sync. The `CString` buffers remain owned by `arguments` and therefore
    // stable until `execv` replaces the child process.
    let mut argument_pointers = arguments
        .iter()
        .map(|argument| argument.as_ptr() as usize)
        .collect::<Vec<_>>();
    argument_pointers.push(0);

    let diagnostic_pipe = if capture_structured_diagnostics {
        Some(cloexec_pipe()?)
    } else {
        None
    };
    let diagnostic_signal_pipe = if capture_structured_diagnostics {
        Some(cloexec_pipe()?)
    } else {
        None
    };
    let inherited_diagnostic_fd = diagnostic_pipe
        .as_ref()
        .map(|(_, writer)| writer.as_raw_fd());
    let inherited_diagnostic_signal_fd = diagnostic_signal_pipe
        .as_ref()
        .map(|(_, writer)| writer.as_raw_fd());

    // Build the exact environment used by `execve` in the parent. In
    // particular, strip any caller-supplied value for the private channel so a
    // normal/human launch cannot be tricked into writing to an unrelated fd.
    let internal_data_key = aura_compiler::INTERNAL_DIAGNOSTIC_FD_ENV.as_bytes();
    let internal_signal_key = aura_compiler::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV.as_bytes();
    let mut environment = std::env::vars_os()
        .filter_map(|(key, value)| {
            let key = key.as_os_str().as_bytes();
            if key == internal_data_key || key == internal_signal_key {
                return None;
            }
            let mut entry = Vec::with_capacity(key.len() + value.as_os_str().as_bytes().len() + 1);
            entry.extend_from_slice(key);
            entry.push(b'=');
            entry.extend_from_slice(value.as_os_str().as_bytes());
            CString::new(entry).ok()
        })
        .collect::<Vec<_>>();
    if let Some(fd) = inherited_diagnostic_fd {
        environment.push(
            CString::new(format!(
                "{}={fd}",
                aura_compiler::INTERNAL_DIAGNOSTIC_FD_ENV
            ))
            .expect("the internal diagnostic fd environment entry cannot contain NUL"),
        );
    }
    if let Some(fd) = inherited_diagnostic_signal_fd {
        environment.push(
            CString::new(format!(
                "{}={fd}",
                aura_compiler::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV
            ))
            .expect("the internal diagnostic signal fd environment entry cannot contain NUL"),
        );
    }
    let mut environment_pointers = environment
        .iter()
        .map(|entry| entry.as_ptr() as usize)
        .collect::<Vec<_>>();
    environment_pointers.push(0);

    let mut command = process::Command::new(binary);
    // SAFETY: after fork the closure calls only async-signal-safe `fcntl`,
    // `execve`, and `last_os_error`; all strings and pointer storage were
    // allocated before `pre_exec`. `execve`, unlike `execvp`, never interprets
    // ENOEXEC bytes as a shell script.
    unsafe {
        command.pre_exec(move || {
            let _keep_arguments_alive = &arguments;
            let _keep_environment_alive = &environment;
            if let Some(fd) = inherited_lease_fd {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            if let Some(fd) = inherited_diagnostic_fd {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            if let Some(fd) = inherited_diagnostic_signal_fd {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            libc::execve(
                executable.as_ptr(),
                argument_pointers.as_ptr().cast::<*const libc::c_char>(),
                environment_pointers.as_ptr().cast::<*const libc::c_char>(),
            );
            Err(io::Error::last_os_error())
        });
    }
    let child = command.spawn()?;
    let diagnostic_reader = diagnostic_pipe.map(|(reader, _writer)| reader);
    let diagnostic_signal_reader = diagnostic_signal_pipe.map(|(reader, _writer)| reader);
    Ok(SpawnedNativeBinary {
        child,
        diagnostic_reader,
        diagnostic_signal_reader,
    })
}

#[cfg(not(unix))]
fn spawn_native_binary_without_shell_fallback(
    binary: &Path,
    program_args: &[String],
) -> io::Result<process::Child> {
    process::Command::new(binary).args(program_args).spawn()
}

#[cfg(not(unix))]
fn spawn_native_binary_with_diagnostic_mode(
    binary: &Path,
    program_args: &[String],
    _capture_structured_diagnostics: bool,
) -> io::Result<SpawnedNativeBinary> {
    spawn_native_binary_without_shell_fallback(binary, program_args)
        .map(|child| SpawnedNativeBinary { child })
}

fn wait_for_native_binary(mut spawned: SpawnedNativeBinary) -> io::Result<NativeExecutionOutcome> {
    #[cfg(unix)]
    let diagnostic_reader = spawned.diagnostic_reader.take().map(|reader| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            reader
                .take(
                    u64::try_from(aura_compiler::MAX_INTERNAL_DIAGNOSTIC_BYTES)
                        .unwrap_or(u64::MAX)
                        .saturating_add(1),
                )
                .read_to_end(&mut bytes)?;
            Ok::<_, io::Error>(bytes)
        })
    });
    #[cfg(unix)]
    let diagnostic_signal_reader = spawned.diagnostic_signal_reader.take().map(|reader| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            reader.take(2).read_to_end(&mut bytes)?;
            Ok::<_, io::Error>(bytes)
        })
    });

    let status = spawned.child.wait()?;

    #[cfg(unix)]
    {
        let diagnostic_bytes = match diagnostic_reader {
            Some(reader) => reader
                .join()
                .map_err(|_| io::Error::other("the native diagnostic-channel reader panicked"))??,
            None => Vec::new(),
        };
        let signal_bytes = match diagnostic_signal_reader {
            Some(reader) => reader.join().map_err(|_| {
                io::Error::other("the native diagnostic signal-channel reader panicked")
            })??,
            None => Vec::new(),
        };
        if status.code().is_none() {
            return Err(io::Error::other(
                "native program terminated by a host signal",
            ));
        }
        if signal_bytes.len() > 1
            || signal_bytes
                .first()
                .is_some_and(|marker| *marker != aura_compiler::INTERNAL_DIAGNOSTIC_SIGNAL_MARKER)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid native diagnostic trap-intent marker",
            ));
        }
        let trap_intended = !signal_bytes.is_empty();
        if diagnostic_bytes.len() > aura_compiler::MAX_INTERNAL_DIAGNOSTIC_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "native diagnostic-channel record exceeded the {}-byte limit",
                    aura_compiler::MAX_INTERNAL_DIAGNOSTIC_BYTES
                ),
            ));
        }
        if status.success() && (trap_intended || !diagnostic_bytes.is_empty()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "native diagnostic channel was used during a successful exit",
            ));
        }
        if !trap_intended && diagnostic_bytes.is_empty() {
            return Ok(NativeExecutionOutcome::Exited(
                status.code().expect("signal exits were rejected above"),
            ));
        }
        if trap_intended && diagnostic_bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "native runtime signaled a trap without a diagnostic record",
            ));
        }
        if !trap_intended {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "native runtime emitted a diagnostic record without signaling a trap",
            ));
        }
        let diagnostic = serde_json::from_slice::<StructuredDiagnostic>(&diagnostic_bytes)
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid native diagnostic-channel record: {error}"),
                )
            })?;
        Ok(NativeExecutionOutcome::Trapped(Box::new(diagnostic)))
    }

    #[cfg(not(unix))]
    {
        if status.code().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "native program terminated without an exit status",
            ));
        }
        Ok(NativeExecutionOutcome::Exited(
            status.code().expect("missing statuses were rejected above"),
        ))
    }
}

fn native_launch_error_invalidates_cache(error: &io::Error) -> bool {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::ENOEXEC) {
        return true;
    }
    #[cfg(target_os = "macos")]
    if matches!(
        error.raw_os_error(),
        Some(libc::EBADEXEC)
            | Some(libc::EBADARCH)
            | Some(libc::EBADMACHO)
            | Some(libc::ESHLIBVERS)
    ) {
        return true;
    }
    #[cfg(target_os = "linux")]
    if error.raw_os_error() == Some(libc::ELIBBAD) {
        return true;
    }
    false
}

/// Decides what a native run does when the build or the launch fails. Only
/// `auto` degrades to the MIR runtime; a forced `direct` run reports the
/// failure so a parity or benchmark caller never silently measures the wrong
/// backend.
#[cfg(test)]
fn select_run_outcome(
    backend: RunBackend,
    build: impl FnOnce() -> std::result::Result<(), String>,
    execute: impl FnOnce() -> std::result::Result<i32, String>,
) -> NativeRunOutcome {
    let degrade = |reason: String| match backend {
        RunBackend::Auto => NativeRunOutcome::FellBack(reason),
        _ => NativeRunOutcome::Failed(reason),
    };
    if let Err(reason) = build() {
        return degrade(reason);
    }
    match execute() {
        Ok(code) => NativeRunOutcome::Ran(code),
        Err(reason) => degrade(reason),
    }
}

/// The directory holding cached native binaries. `AURA_CACHE_DIR` overrides
/// the default so a sandbox or a test can keep its own cache.
fn native_cache_root() -> Option<PathBuf> {
    let root = if let Some(explicit) = std::env::var_os("AURA_CACHE_DIR") {
        let explicit = PathBuf::from(explicit);
        (!explicit.as_os_str().is_empty()).then_some(explicit)?
    } else {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        if home.as_os_str().is_empty() {
            return None;
        }
        home.join(".cache").join("aura").join("native")
    };
    create_private_cache_directory_all(&root).ok()?;
    Some(root)
}

const NATIVE_RUNTIME_BUILD_LOCK: &str = ".aura-native-runtime-build.lock";
const NATIVE_CACHE_LOCKS: &str = "locks";

struct NativeBuildLock {
    _file: fs::File,
}

/// Acquires the cross-process single-writer lease for native runtime identity
/// and cache establishment.
///
/// The first non-blocking attempt distinguishes a genuinely uncontended
/// acquisition from a wait, so users hear about the latter before the process
/// blocks in the kernel.
fn acquire_native_cache_build_lock(
    key: &str,
    on_wait: Option<&mut dyn FnMut()>,
) -> std::result::Result<Option<NativeBuildLock>, String> {
    let Some(root) = native_cache_root() else {
        // An unavailable cache retains the historical direct-build fallback.
        // Runtime artifact access is still protected by the shared lease.
        return Ok(None);
    };
    let locks = root.join(NATIVE_CACHE_LOCKS);
    create_private_cache_directory_all(&locks).map_err(|error| {
        format!(
            "failed to create native cache lock directory `{}`: {error}",
            locks.display()
        )
    })?;
    acquire_native_build_lock_at(&locks.join(format!("{key}.lock")), on_wait).map(Some)
}

fn acquire_native_runtime_build_lock(
    on_wait: Option<&mut dyn FnMut()>,
) -> std::result::Result<Option<NativeBuildLock>, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("failed to locate the running aura executable: {error}"))?;
    let directory = match resolve_installed_runtime_artifacts_from_executable(&executable)? {
        // Installed runtime artifacts are immutable inputs. Optional native
        // caching must not become mandatory merely to obtain a runtime lock.
        Some(_) => return Ok(None),
        None => {
            let directory = native_runtime_target_dir();
            fs::create_dir_all(&directory).map_err(|error| {
                format!(
                    "failed to create native runtime target directory `{}` for build locking: {error}",
                    directory.display()
                )
            })?;
            directory
        }
    };
    acquire_native_build_lock_at(&directory.join(NATIVE_RUNTIME_BUILD_LOCK), on_wait).map(Some)
}

fn acquire_native_build_lock_at(
    path: &Path,
    mut on_wait: Option<&mut dyn FnMut()>,
) -> std::result::Result<NativeBuildLock, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|error| {
        format!(
            "failed to open native build lock `{}`: {error}",
            path.display()
        )
    })?;
    let metadata = file.metadata().map_err(|error| {
        format!(
            "failed to inspect native build lock `{}`: {error}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "native build lock `{}` is not a regular file",
            path.display()
        ));
    }

    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(format!(
                "native build lock `{}` must be owned by the current user and not group- or world-writable",
                path.display()
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                format!(
                    "failed to secure native build lock `{}`: {error}",
                    path.display()
                )
            })?;

        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == -1 {
            let error = io::Error::last_os_error();
            if !matches!(
                error.raw_os_error(),
                Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN
            ) {
                return Err(format!(
                    "failed to acquire native build lock `{}`: {error}",
                    path.display()
                ));
            }
            if let Some(on_wait) = on_wait.as_mut() {
                on_wait();
            }
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
                return Err(format!(
                    "failed to wait for native build lock `{}`: {}",
                    path.display(),
                    io::Error::last_os_error()
                ));
            }
        }
        Ok(NativeBuildLock { _file: file })
    }

    #[cfg(not(unix))]
    {
        Ok(NativeBuildLock { _file: file })
    }
}

fn create_private_cache_directory_all(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        // Apply the private mode at each mkdir operation so a permissive
        // process umask cannot expose any newly-created parent component.
        builder.mode(0o700);
    }
    builder.create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::other(format!(
            "native cache path `{}` is not a directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "native cache directory `{}` must be owned by the current user and not group- or world-writable",
                    path.display()
                ),
            ));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    cleanup_stale_native_cache_artifacts(path);
    Ok(())
}

fn cleanup_stale_native_cache_artifacts(directory: &Path) {
    cleanup_stale_native_cache_artifacts_at(directory, system_time_nanos(), native_process_is_live);
}

fn cleanup_stale_native_cache_artifacts_at(
    directory: &Path,
    now_nanos: u128,
    process_is_live: impl Fn(u32) -> bool,
) {
    const STALE_AFTER_NANOS: u128 = 24 * 60 * 60 * 1_000_000_000;
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((owner, created_nanos)) = name
            .to_str()
            .and_then(parse_native_cache_transient_identity)
        else {
            continue;
        };
        if process_is_live(owner) || now_nanos.saturating_sub(created_nanos) < STALE_AFTER_NANOS {
            continue;
        }
        remove_native_cache_path(&entry.path());
    }
}

fn parse_native_cache_transient_identity(name: &str) -> Option<(u32, u128)> {
    let tail = if let Some(tail) = name.strip_prefix(".runtime-identity-") {
        tail
    } else if let Some(tail) = name.strip_prefix(".discard-") {
        let (key, tail) = tail.split_once('-')?;
        if !is_sha256_hex(key) {
            return None;
        }
        tail
    } else {
        let tail = name.strip_prefix('.')?;
        let (key, tail) = tail.split_once('-')?;
        if !is_sha256_hex(key) {
            return None;
        }
        tail
    };
    let (owner, created_nanos) = tail.split_once('-')?;
    if created_nanos.contains('-') {
        return None;
    }
    let owner = owner.parse::<u32>().ok().filter(|owner| *owner > 0)?;
    let created_nanos = created_nanos.parse::<u128>().ok()?;
    Some((owner, created_nanos))
}

fn native_process_is_live(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

fn write_private_cache_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Err(error) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(path);
            return Err(error);
        }
    }
    let result = file.write_all(contents).and_then(|()| file.flush());
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

/// Content-addressed identity of a native build.
///
/// Every input that can change the emitted binary contributes: the cache
/// format, this compiler's version, the host target, the backend, the runtime
/// archive's identity, its ordered native link arguments, and the complete
/// lowered program. Lowering already
/// incorporates the entry source and every resolved dependency source, so
/// hashing the module covers the whole dependency set without walking it
/// again. Returning `None` disables caching rather than risking a key that
/// omits an input.
#[derive(Clone, Debug, Eq, PartialEq)]
struct NativeRuntimeIdentity {
    archive_sha256: String,
    native_link_args: Vec<String>,
}

fn native_cache_key(mir: &MirModule, runtime_identity: &NativeRuntimeIdentity) -> Option<String> {
    native_cache_key_for_semantic_schema(
        mir,
        runtime_identity,
        aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION,
    )
}

fn native_cache_key_for_semantic_schema(
    mir: &MirModule,
    runtime_identity: &NativeRuntimeIdentity,
    semantic_schema_version: u32,
) -> Option<String> {
    let lowered = serde_json::to_vec(mir).ok()?;
    let runtime = native_runtime_identity_material(runtime_identity)?;
    let semantic_schema = semantic_schema_version.to_le_bytes();
    let mut material = Vec::with_capacity(lowered.len() + 256);
    for part in [
        NATIVE_CACHE_FORMAT.as_bytes(),
        semantic_schema.as_slice(),
        env!("CARGO_PKG_VERSION").as_bytes(),
        std::env::consts::ARCH.as_bytes(),
        std::env::consts::OS.as_bytes(),
        b"direct",
        runtime.as_slice(),
    ] {
        material.extend_from_slice(part);
        material.push(0);
    }
    material.extend_from_slice(&lowered);
    Some(aura_compiler::sha256_hex(&material))
}

fn native_runtime_identity_material(identity: &NativeRuntimeIdentity) -> Option<Vec<u8>> {
    serde_json::to_vec(&(identity.archive_sha256.as_str(), &identity.native_link_args)).ok()
}

// The compiler-owned semantic schema version is an additional, independent
// key component, so checked-source changes do not require a cache-container
// format change.
const NATIVE_CACHE_FORMAT: &str = "aura-native-cache-v6";

/// The exact runtime inputs that a warm native-cache lookup may reuse.
///
/// Packaged runs can read both inputs directly. Source-checkout runs reuse only
/// an identity memo written by a completed cold build, avoiding a Cargo query
/// on every hit while never inventing an `unresolved` cache identity.
fn native_runtime_identity_for_cache() -> Option<NativeRuntimeIdentity> {
    // Packaged executables link the runtime installed beside `bin/aura`;
    // source-checkout executables use the current workspace archive. Cache
    // lookup must fingerprint the same selection the builder will make.
    let executable = std::env::current_exe().ok()?;
    let (path, installed_link_args, memo) =
        match resolve_installed_runtime_artifacts_from_executable(&executable) {
            Ok(Some(artifacts)) => (
                artifacts.staticlib,
                Some(artifacts.native_link_args),
                native_cache_root()?.join(RUNTIME_IDENTITY_MEMO),
            ),
            Ok(None) => {
                let target = native_runtime_target_dir();
                (
                    resolve_static_library_path_in_target_dir(target.clone(), current_profile())
                        .ok()?,
                    None,
                    target.join(RUNTIME_IDENTITY_MEMO),
                )
            }
            // An incomplete or invalid installed layout must not be hidden by a
            // cache hit labeled with unrelated workspace artifacts.
            Err(_) => return None,
        };
    let metadata = fs::metadata(&path).ok()?;
    let stamp = runtime_archive_memo_stamp(&path, &metadata)?;
    if let Some(mut identity) = read_runtime_identity_memo(&memo, &stamp) {
        let stamp_after = fs::metadata(&path)
            .ok()
            .and_then(|metadata| runtime_archive_memo_stamp(&path, &metadata));
        if stamp_after.as_deref() == Some(stamp.as_str()) {
            if let Some(link_args) = installed_link_args.as_ref() {
                identity.native_link_args = link_args.clone();
            }
            return Some(identity);
        }
    }
    if installed_link_args.is_none() {
        // Cargo may replace an unchanged archive inode while another cache
        // root is being established. Under the shared runtime lease, compare
        // bytes before treating that cheap-stamp change as a semantic change.
        if let Some(identity) = read_runtime_identity_memo_by_content(&memo, &stamp, &path) {
            return Some(identity);
        }
    }

    // Workspace link arguments are known only after the cold Cargo query.
    // Without its matching memo, disable lookup and let that build establish
    // the authoritative identity. Installed manifests already carry them.
    let native_link_args = installed_link_args?;
    let contents = fs::read(&path).ok()?;
    let identity = NativeRuntimeIdentity {
        archive_sha256: aura_compiler::sha256_hex(&contents),
        native_link_args,
    };
    write_runtime_identity_memo(&memo, &stamp, &identity);
    Some(identity)
}

const RUNTIME_IDENTITY_MEMO: &str = "runtime-identity";
const MAX_RUNTIME_IDENTITY_MEMO_BYTES: u64 = 8192;

fn runtime_archive_memo_stamp(path: &Path, metadata: &fs::Metadata) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        Some(format!(
            "{}:{}:{}:{}:{}:{}:{}:{}",
            path.display(),
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec()
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, metadata);
        // Platforms without a stable file-identity/change token rehash rather
        // than reuse a memo that could alias different archive contents.
        None
    }
}

fn read_runtime_identity_memo(memo: &Path, stamp: &str) -> Option<NativeRuntimeIdentity> {
    let (recorded_stamp, identity) = parse_runtime_identity_memo(memo)?;
    (recorded_stamp == stamp).then_some(identity)
}

fn read_runtime_identity_memo_by_content(
    memo: &Path,
    stamp: &str,
    archive: &Path,
) -> Option<NativeRuntimeIdentity> {
    let (_, identity) = parse_runtime_identity_memo(memo)?;
    let before = fs::metadata(archive)
        .ok()
        .and_then(|metadata| runtime_archive_memo_stamp(archive, &metadata))?;
    let contents = fs::read(archive).ok()?;
    let after = fs::metadata(archive)
        .ok()
        .and_then(|metadata| runtime_archive_memo_stamp(archive, &metadata))?;
    if before != stamp
        || after != stamp
        || aura_compiler::sha256_hex(&contents) != identity.archive_sha256
    {
        return None;
    }
    write_runtime_identity_memo(memo, stamp, &identity);
    Some(identity)
}

fn parse_runtime_identity_memo(memo: &Path) -> Option<(String, NativeRuntimeIdentity)> {
    let recorded = read_limited_regular_file(memo, MAX_RUNTIME_IDENTITY_MEMO_BYTES, false)?;
    let recorded = std::str::from_utf8(&recorded).ok()?;
    let mut lines = recorded.lines();
    let recorded_stamp = lines.next()?;
    let archive_sha256 = lines.next()?;
    let native_link_args = lines.next()?;
    if !is_sha256_hex(archive_sha256) || lines.next().is_some() {
        return None;
    }
    Some((
        recorded_stamp.to_string(),
        NativeRuntimeIdentity {
            archive_sha256: archive_sha256.to_string(),
            native_link_args: serde_json::from_str(native_link_args).ok()?,
        },
    ))
}

fn write_runtime_identity_memos(stamp: &str, identity: &NativeRuntimeIdentity) {
    let executable = std::env::current_exe().ok();
    let installed = executable
        .as_deref()
        .and_then(|path| {
            resolve_installed_runtime_artifacts_from_executable(path)
                .ok()
                .flatten()
        })
        .is_some();
    let authoritative = if installed {
        native_cache_root().map(|root| root.join(RUNTIME_IDENTITY_MEMO))
    } else {
        Some(native_runtime_target_dir().join(RUNTIME_IDENTITY_MEMO))
    };
    if let Some(memo) = authoritative.as_ref() {
        write_runtime_identity_memo(memo, stamp, identity);
    }
    // Keep the cache-local memo for the existing cache layout contract and
    // installed packages. Workspace lookup is authoritative at the target
    // root so sibling cache roots refresh one shared content identity.
    if let Some(cache_memo) = native_cache_root().map(|root| root.join(RUNTIME_IDENTITY_MEMO)) {
        if authoritative.as_ref() != Some(&cache_memo) {
            write_runtime_identity_memo(&cache_memo, stamp, identity);
        }
    }
}

fn establish_native_runtime_identity_memo() -> std::result::Result<NativeRuntimeIdentity, String> {
    let native_runtime = ensure_native_runtime_artifacts()?;
    let before = fs::metadata(&native_runtime.staticlib)
        .ok()
        .and_then(|metadata| runtime_archive_memo_stamp(&native_runtime.staticlib, &metadata))
        .ok_or_else(|| {
            format!(
                "failed to fingerprint Aura runtime library `{}`",
                native_runtime.staticlib.display()
            )
        })?;
    let contents = fs::read(&native_runtime.staticlib).map_err(|error| {
        format!(
            "failed to read Aura runtime library `{}`: {error}",
            native_runtime.staticlib.display()
        )
    })?;
    let after = fs::metadata(&native_runtime.staticlib)
        .ok()
        .and_then(|metadata| runtime_archive_memo_stamp(&native_runtime.staticlib, &metadata))
        .ok_or_else(|| {
            format!(
                "failed to recheck Aura runtime library `{}`",
                native_runtime.staticlib.display()
            )
        })?;
    if before != after {
        return Err(format!(
            "Aura runtime library `{}` changed while its identity was being established",
            native_runtime.staticlib.display()
        ));
    }
    let identity = NativeRuntimeIdentity {
        archive_sha256: aura_compiler::sha256_hex(&contents),
        native_link_args: native_runtime.native_link_args,
    };
    write_runtime_identity_memos(&after, &identity);
    Ok(identity)
}

fn write_runtime_identity_memo(memo: &Path, stamp: &str, identity: &NativeRuntimeIdentity) {
    let Some(parent) = memo.parent() else {
        return;
    };
    if create_private_cache_directory_all(parent).is_err() {
        return;
    }
    let staged = parent.join(format!(
        ".{}-{}-{}",
        RUNTIME_IDENTITY_MEMO,
        std::process::id(),
        system_time_nanos()
    ));
    let Ok(native_link_args) = serde_json::to_string(&identity.native_link_args) else {
        return;
    };
    if write_private_cache_file(
        &staged,
        format!("{stamp}\n{}\n{native_link_args}\n", identity.archive_sha256).as_bytes(),
    )
    .is_err()
    {
        return;
    }
    if fs::rename(&staged, memo).is_err() {
        let _ = fs::remove_file(&staged);
    }
}

/// Cached program binaries live under their own directory so the cache can
/// hold its own bookkeeping without either colliding with a content key.
const NATIVE_CACHE_PROGRAMS: &str = "programs";
const NATIVE_CACHE_PROGRAM: &str = "program";
const NATIVE_CACHE_DIGEST: &str = "program.sha256";
const NATIVE_CACHE_ENTRY_ID: &str = "entry-id";
const MAX_NATIVE_CACHE_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_NATIVE_CACHE_DIGEST_BYTES: u64 = 65;
const MAX_NATIVE_CACHE_ENTRY_ID_BYTES: u64 = 130;

struct VerifiedNativeBinary {
    contents: Vec<u8>,
    entry: PathBuf,
    entry_id: String,
}

/// Returns the cached binary for `key` only after its bytes and native launch
/// shape have been verified.
///
/// An invalid entry is disposable cache state, not a program artifact. It is
/// removed and reported as a miss so the caller rebuilds before executing.
fn cached_native_binary(key: &str) -> Option<VerifiedNativeBinary> {
    inspect_cached_native_binary(key, true)
}

fn peek_cached_native_binary(key: &str) -> Option<VerifiedNativeBinary> {
    inspect_cached_native_binary(key, false)
}

fn inspect_cached_native_binary(
    key: &str,
    invalidate_invalid: bool,
) -> Option<VerifiedNativeBinary> {
    let programs = native_cache_root()?.join(NATIVE_CACHE_PROGRAMS);
    cleanup_stale_native_cache_artifacts(&programs);
    let entry = programs.join(key);
    let candidate = entry.join(NATIVE_CACHE_PROGRAM);
    let digest_path = entry.join(NATIVE_CACHE_DIGEST);
    let entry_metadata = fs::symlink_metadata(&entry).ok()?;
    if !entry_metadata.file_type().is_dir() {
        remove_native_cache_entry_if_unchanged(&entry, None);
        return None;
    }
    // Keep the raw observed identity for exact-entry invalidation even when
    // its embedded key is wrong. If we discarded it here, quarantine would
    // mistake the corrupt entry for a concurrent replacement and restore it.
    let observed_entry_id = read_native_cache_entry_id(&entry);
    let entry_id = observed_entry_id
        .as_ref()
        .filter(|entry_id| {
            entry_id
                .split_once(':')
                .is_some_and(|(entry_key, _)| entry_key == key)
        })
        .cloned();

    let recorded_digest =
        read_limited_regular_file(&digest_path, MAX_NATIVE_CACHE_DIGEST_BYTES, false)
            .and_then(|bytes| parse_native_cache_digest(&bytes).map(str::to_owned));
    let contents = read_limited_regular_file(&candidate, MAX_NATIVE_CACHE_ARTIFACT_BYTES, true);
    let verified = recorded_digest
        .as_deref()
        .zip(entry_id.as_deref())
        .zip(contents.as_deref())
        .is_some_and(|((recorded_digest, _), contents)| {
            recorded_digest == aura_compiler::sha256_hex(contents)
                && native_binary_has_expected_shape(contents)
        });
    if verified {
        Some(VerifiedNativeBinary {
            contents: contents.expect("verified cache entry has program bytes"),
            entry,
            entry_id: entry_id.expect("verified cache entry has an identity"),
        })
    } else if invalidate_invalid {
        remove_native_cache_entry_if_unchanged(&entry, observed_entry_id.as_deref());
        None
    } else {
        None
    }
}

fn read_native_cache_entry_id(entry: &Path) -> Option<String> {
    if !fs::symlink_metadata(entry).ok()?.file_type().is_dir() {
        return None;
    }
    let contents = read_limited_regular_file(
        &entry.join(NATIVE_CACHE_ENTRY_ID),
        MAX_NATIVE_CACHE_ENTRY_ID_BYTES,
        false,
    )?;
    let contents = contents.strip_suffix(b"\n").unwrap_or(&contents);
    let entry_id = std::str::from_utf8(contents).ok()?;
    let (key, nonce) = entry_id.split_once(':')?;
    if !is_sha256_hex(key) || !is_sha256_hex(nonce) {
        return None;
    }
    Some(entry_id.to_string())
}

fn read_limited_regular_file(path: &Path, limit: u64, executable: bool) -> Option<Vec<u8>> {
    let path_metadata = fs::symlink_metadata(path).ok()?;
    if !path_metadata.file_type().is_file()
        || path_metadata.len() > limit
        || (executable && !native_binary_metadata_is_executable(&path_metadata))
    {
        return None;
    }

    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // O_NOFOLLOW closes the metadata/open symlink race. O_NONBLOCK means a
        // racing replacement with a FIFO or device cannot hang the cache hit
        // before the opened handle is checked again below.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file()
        || metadata.len() > limit
        || (executable && !native_binary_metadata_is_executable(&metadata))
    {
        return None;
    }

    let initial_capacity = usize::try_from(metadata.len().min(limit)).ok()?;
    let mut contents = Vec::with_capacity(initial_capacity);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut contents)
        .ok()?;
    (u64::try_from(contents.len()).ok()? <= limit).then_some(contents)
}

fn parse_native_cache_digest(contents: &[u8]) -> Option<&str> {
    let contents = contents.strip_suffix(b"\n").unwrap_or(contents);
    let digest = std::str::from_utf8(contents).ok()?;
    is_sha256_hex(digest).then_some(digest)
}

fn launch_verified_native_binary(
    verified: &VerifiedNativeBinary,
    program_args: &[String],
    capture_structured_diagnostics: bool,
) -> std::result::Result<NativeExecutionOutcome, VerifiedNativeLaunchError> {
    let launch_root = std::env::temp_dir();
    cleanup_stale_verified_native_directories(&launch_root);
    let directory = launch_root.join(format!(
        "aura-verified-native-{}-{}",
        std::process::id(),
        system_time_nanos()
    ));
    create_private_directory(&directory).map_err(|error| {
        VerifiedNativeLaunchError::environment(format!(
            "failed to create private verified-native directory `{}`: {}",
            directory.display(),
            error
        ))
    })?;

    #[cfg(unix)]
    let launch_lease = match create_verified_native_launch_lease(&directory) {
        Ok(lease) => lease,
        Err(error) => {
            remove_private_native_launch(&directory.join(NATIVE_CACHE_PROGRAM), &directory);
            return Err(VerifiedNativeLaunchError::environment(format!(
                "failed to create verified-native launch lease in `{}`: {}",
                directory.display(),
                error
            )));
        }
    };

    let private_binary = directory.join(NATIVE_CACHE_PROGRAM);
    if let Err(error) = write_private_native_binary(&private_binary, &verified.contents) {
        remove_private_native_launch(&private_binary, &directory);
        return Err(VerifiedNativeLaunchError::environment(error));
    }

    // These are the already verified in-memory bytes inside a private
    // directory, so launching this file cannot observe later replacement of
    // the shared cache pathname.
    let child_result = {
        #[cfg(unix)]
        {
            spawn_verified_native_binary_with_diagnostic_mode(
                &private_binary,
                program_args,
                &launch_lease,
                capture_structured_diagnostics,
            )
        }
        #[cfg(not(unix))]
        {
            spawn_native_binary_with_diagnostic_mode(
                &private_binary,
                program_args,
                capture_structured_diagnostics,
            )
        }
    };
    let child = match child_result {
        Ok(child) => child,
        Err(error) => {
            remove_private_native_launch(&private_binary, &directory);
            return Err(VerifiedNativeLaunchError::launch(&private_binary, error));
        }
    };

    let outcome = match wait_for_native_binary(child) {
        Ok(outcome) => outcome,
        Err(error) => {
            // A failed wait is not proof that the child stopped. Leave the
            // directory and its inherited lease for a later safe collector.
            return Err(VerifiedNativeLaunchError::after_launch(format!(
                "failed to wait for verified direct binary `{}`: {}",
                private_binary.display(),
                error
            )));
        }
    };

    // Clean after exit. Removing an executing Mach-O can cause an otherwise
    // successful child to terminate with status 1 on macOS. Interrupted
    // parents are handled by the bounded stale-launch collector on the next
    // verified run.
    remove_private_native_launch(&private_binary, &directory);

    Ok(outcome)
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        builder.mode(0o700);
    }
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // The creation mode prevents exposure; this restores owner access if a
        // restrictive umask removed bits that the launch/store requires.
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

const VERIFIED_NATIVE_LAUNCH_LEASE: &str = ".lease";

#[cfg(unix)]
fn create_verified_native_launch_lease(directory: &Path) -> io::Result<fs::File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let path = directory.join(VERIFIED_NATIVE_LAUNCH_LEASE);
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(true).mode(0o600);
    let file = options.open(&path)?;
    if let Err(error) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        let error = io::Error::last_os_error();
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(file)
}

fn write_private_native_binary(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o700);
    }
    let mut file = options.open(path).map_err(|error| {
        format!(
            "failed to create private verified native binary `{}`: {}",
            path.display(),
            error
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        file.set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(|error| {
                format!(
                    "failed to secure private verified native binary `{}`: {}",
                    path.display(),
                    error
                )
            })?;
    }
    let result = file
        .write_all(contents)
        .and_then(|()| file.flush())
        .map_err(|error| {
            format!(
                "failed to materialize private verified native binary `{}`: {}",
                path.display(),
                error
            )
        });
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn remove_private_native_launch(binary: &Path, directory: &Path) {
    let _ = fs::remove_file(binary);
    let _ = fs::remove_file(directory.join(VERIFIED_NATIVE_LAUNCH_LEASE));
    let _ = fs::remove_dir(directory);
}

fn cleanup_stale_verified_native_directories(root: &Path) {
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("aura-verified-native-") {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_dir() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            if metadata.uid() != unsafe { libc::geteuid() } {
                continue;
            }
        }
        if verified_native_launch_owner_is_live(name) {
            continue;
        }

        #[cfg(unix)]
        match acquire_abandoned_verified_native_launch_lease(&path) {
            Ok(VerifiedNativeLaunchLeaseState::Busy) | Err(_) => continue,
            Ok(VerifiedNativeLaunchLeaseState::Acquired(_lease)) => {
                // Hold the acquired lease until the directory has gone so a
                // concurrent collector cannot race this decision.
                let _ = fs::remove_dir_all(path);
                continue;
            }
            Ok(VerifiedNativeLaunchLeaseState::Missing) => {}
        }
        #[cfg(not(unix))]
        {
            // The maintained native hosts use the lease path above. On other
            // targets cleanup is conservative rather than guessing whether an
            // executable may still be live.
            continue;
        }

        // A lockless directory can only precede lease creation; no child is
        // spawned until the lease exists. Give an interrupted creator a
        // bounded grace period before collecting that incomplete directory.
        let old_enough = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_AFTER);
        if old_enough {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[cfg(unix)]
enum VerifiedNativeLaunchLeaseState {
    Missing,
    Busy,
    Acquired(fs::File),
}

#[cfg(unix)]
fn acquire_abandoned_verified_native_launch_lease(
    directory: &Path,
) -> io::Result<VerifiedNativeLaunchLeaseState> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let path = directory.join(VERIFIED_NATIVE_LAUNCH_LEASE);
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(VerifiedNativeLaunchLeaseState::Missing);
        }
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "verified-native launch lease is not a current-user regular file",
        ));
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(VerifiedNativeLaunchLeaseState::Acquired(file));
    }
    let error = io::Error::last_os_error();
    let raw_error = error.raw_os_error();
    if raw_error == Some(libc::EWOULDBLOCK) || raw_error == Some(libc::EAGAIN) {
        Ok(VerifiedNativeLaunchLeaseState::Busy)
    } else {
        Err(error)
    }
}

fn verified_native_launch_owner_is_live(name: &str) -> bool {
    #[cfg(unix)]
    {
        let Some(pid) = name
            .strip_prefix("aura-verified-native-")
            .and_then(|suffix| suffix.split_once('-').map(|(pid, _)| pid))
            .and_then(|pid| pid.parse::<libc::pid_t>().ok())
        else {
            return true;
        };
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = name;
        true
    }
}

#[cfg(unix)]
fn native_binary_metadata_is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn native_binary_metadata_is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

/// Publishes `built` into the cache under `key`.
///
/// The binary and the digest of the copied bytes are written into a unique
/// temporary directory and then renamed together, so a concurrent run either
/// sees no entry or a complete, self-verifiable one. A failure to publish is
/// not a build failure: the run continues with the binary it already has.
fn store_native_binary(key: &str, built: &Path) {
    let Some(root) = native_cache_root().map(|root| root.join(NATIVE_CACHE_PROGRAMS)) else {
        return;
    };
    if create_private_cache_directory_all(&root).is_err() {
        return;
    }
    let nonce = system_time_nanos();
    let staged = root.join(format!(".{}-{}-{}", key, std::process::id(), nonce));
    if create_private_directory(&staged).is_err() {
        return;
    }
    let staged_binary = staged.join(NATIVE_CACHE_PROGRAM);
    let staged_digest = staged.join(NATIVE_CACHE_DIGEST);
    let staged_entry_id = staged.join(NATIVE_CACHE_ENTRY_ID);
    let copied = fs::copy(built, &staged_binary)
        .ok()
        .and_then(|_| {
            read_limited_regular_file(&staged_binary, MAX_NATIVE_CACHE_ARTIFACT_BYTES, true)
        })
        .filter(|contents| native_binary_has_expected_shape(contents));
    let Some(contents) = copied else {
        remove_native_cache_path(&staged);
        return;
    };
    let digest = aura_compiler::sha256_hex(&contents);
    if write_private_cache_file(&staged_digest, format!("{digest}\n").as_bytes()).is_err() {
        remove_native_cache_path(&staged);
        return;
    }
    let entry_nonce =
        aura_compiler::sha256_hex(format!("{key}:{}:{nonce}", std::process::id()).as_bytes());
    let entry_id = format!("{key}:{entry_nonce}");
    if write_private_cache_file(&staged_entry_id, format!("{entry_id}\n").as_bytes()).is_err() {
        remove_native_cache_path(&staged);
        return;
    }
    if fs::rename(&staged, root.join(key)).is_err() {
        remove_native_cache_path(&staged);
    }
}

fn invalidate_cached_native_binary(verified: &VerifiedNativeBinary) {
    remove_native_cache_entry_if_unchanged(&verified.entry, Some(&verified.entry_id));
}

fn remove_native_cache_entry_if_unchanged(entry: &Path, expected_entry_id: Option<&str>) {
    let Some(parent) = entry.parent() else {
        return;
    };
    let name = entry
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("entry");
    let quarantined = parent.join(format!(
        ".discard-{}-{}-{}",
        name,
        std::process::id(),
        system_time_nanos()
    ));
    if fs::rename(entry, &quarantined).is_err() {
        return;
    }
    let moved_entry_id = read_native_cache_entry_id(&quarantined);
    if moved_entry_id.as_deref() == expected_entry_id {
        remove_native_cache_path(&quarantined);
        return;
    }

    // Another process replaced the original entry before this process could
    // quarantine it. Never delete that replacement. Restore it if the
    // canonical key is still vacant; otherwise leave the quarantined copy for
    // the bounded stale-cache cleanup rather than destroying known-good data.
    if fs::symlink_metadata(entry).is_err() {
        let _ = fs::rename(&quarantined, entry);
    }
}

fn remove_native_cache_path(path: &Path) {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            let _ = fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = fs::remove_file(path);
        }
        Err(_) => {}
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn native_binary_has_expected_shape(contents: &[u8]) -> bool {
    #[cfg(target_os = "macos")]
    {
        let Some(magic) = contents.get(..4) else {
            return false;
        };
        matches!(
            magic,
            b"\xfe\xed\xfa\xce"
                | b"\xce\xfa\xed\xfe"
                | b"\xfe\xed\xfa\xcf"
                | b"\xcf\xfa\xed\xfe"
                | b"\xca\xfe\xba\xbe"
                | b"\xbe\xba\xfe\xca"
                | b"\xca\xfe\xba\xbf"
                | b"\xbf\xba\xfe\xca"
        )
    }
    #[cfg(target_os = "linux")]
    {
        contents.starts_with(b"\x7fELF")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        !contents.is_empty()
    }
}

fn temporary_run_binary_path(source_path: &str) -> PathBuf {
    let stem = Path::new(source_path)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("aura-run");
    let unique = format!(
        "aura-run-{}-{}-{}",
        stem,
        std::process::id(),
        system_time_nanos()
    );
    std::env::temp_dir().join(unique)
}

fn parse_build_args(args: Vec<String>) -> (PathBuf, BuildBackend, Vec<String>) {
    let mut output = None;
    let mut backend = BuildBackend::Auto;
    let mut input_args = Vec::new();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-o" | "--output" => {
                index += 1;
                output = Some(PathBuf::from(
                    args.get(index)
                        .cloned()
                        .unwrap_or_else(|| print_usage_and_exit(2)),
                ));
                index += 1;
            }
            "--backend" => {
                index += 1;
                let value = args
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| print_usage_and_exit(2));
                backend = match value.as_str() {
                    "auto" => BuildBackend::Auto,
                    "direct" => BuildBackend::Direct,
                    _ => print_usage_and_exit(2),
                };
                index += 1;
            }
            _ => {
                input_args.push(args[index].clone());
                index += 1;
            }
        }
    }

    let output = output.unwrap_or_else(|| print_usage_and_exit(2));
    if input_args.is_empty() {
        print_usage_and_exit(2);
    }

    (output, backend, input_args)
}

fn read_input(args: &mut impl Iterator<Item = String>) -> Input {
    let Some(first) = args.next() else {
        print_usage_and_exit(2);
    };

    if first == "--stdin" {
        let Some(virtual_path) = args.next() else {
            print_usage_and_exit(2);
        };
        if args.next().is_some() {
            print_usage_and_exit(2);
        }
        let mut source = String::new();
        if let Err(error) = io::stdin().read_to_string(&mut source) {
            eprintln!("failed to read source from stdin: {}", error);
            process::exit(1);
        }
        return Input {
            path: virtual_path,
            source,
            from_stdin: true,
        };
    }

    if args.next().is_some() {
        print_usage_and_exit(2);
    }

    let path = first;
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("failed to read `{}`: {}", path, error);
            process::exit(1);
        }
    };

    Input {
        path,
        source,
        from_stdin: false,
    }
}

fn parse_diagnostic_format(args: Vec<String>) -> (DiagnosticFormat, Vec<String>) {
    let mut format = DiagnosticFormat::Human;
    let mut remaining = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--format" {
            let value = args
                .get(index + 1)
                .unwrap_or_else(|| print_usage_and_exit(2));
            format = match value.as_str() {
                "human" => DiagnosticFormat::Human,
                "json" => DiagnosticFormat::Json,
                _ => print_usage_and_exit(2),
            };
            index += 2;
        } else {
            remaining.push(args[index].clone());
            index += 1;
        }
    }
    (format, remaining)
}

fn emit_diagnostic(format: DiagnosticFormat, path: &str, source: &str, error: &Diagnostic) {
    match format {
        DiagnosticFormat::Human => eprintln!("{}", render_error(path, source, error)),
        DiagnosticFormat::Json => {
            let report = serde_json::json!({
                "schema_version": 1,
                "diagnostics": [error.structured(path)],
            });
            eprintln!("{}", report);
        }
    }
}

fn emit_structured_diagnostic(error: StructuredDiagnostic) {
    let report = serde_json::json!({
        "schema_version": 1,
        "diagnostics": [error],
    });
    eprintln!("{}", report);
}

fn render_error(path: &str, source: &str, error: &Diagnostic) -> String {
    error.render_with_source(path, source)
}

fn build_binary_with_backend(
    path: &str,
    source: &str,
    mir: &MirModule,
    output_path: &Path,
    backend: BuildBackend,
    mut on_wait: impl FnMut(),
) -> std::result::Result<BuildOutcome, String> {
    select_build_backend(
        backend,
        || build_direct_native_binary(path, source, mir, output_path, Some(&mut on_wait)),
        || build_mir_runtime_binary(path, source, mir, output_path),
    )
}

fn select_build_backend(
    backend: BuildBackend,
    direct: impl FnOnce() -> std::result::Result<(), String>,
    mir_runtime: impl FnOnce() -> std::result::Result<(), String>,
) -> std::result::Result<BuildOutcome, String> {
    match backend {
        BuildBackend::Direct => direct().map(|()| BuildOutcome {
            selected: SelectedBuildBackend::Direct,
            fallback_reason: None,
        }),
        BuildBackend::Auto => match direct() {
            Ok(()) => Ok(BuildOutcome {
                selected: SelectedBuildBackend::Direct,
                fallback_reason: None,
            }),
            Err(direct_error) => match mir_runtime() {
                Ok(()) => Ok(BuildOutcome {
                    selected: SelectedBuildBackend::MirRuntime,
                    fallback_reason: Some(direct_error),
                }),
                Err(mir_error) => Err(format!(
                    "both native build backends failed\n\ndirect backend:\n{direct_error}\n\nMIR runtime backend:\n{mir_error}"
                )),
            },
        },
    }
}

fn build_direct_native_binary(
    path: &str,
    source: &str,
    mir: &MirModule,
    output_path: &Path,
    on_wait: Option<&mut dyn FnMut()>,
) -> std::result::Result<(), String> {
    let runtime_lock = acquire_native_runtime_build_lock(on_wait)?;
    build_direct_native_binary_with_identity(path, source, mir, output_path, runtime_lock)
        .map(|_| ())
}

fn build_direct_native_binary_with_identity(
    path: &str,
    source: &str,
    mir: &MirModule,
    output_path: &Path,
    runtime_lock: Option<NativeBuildLock>,
) -> std::result::Result<NativeRuntimeIdentity, String> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create output directory `{}`: {}",
                parent.display(),
                error
            )
        })?;
    }

    let native_runtime = ensure_native_runtime_artifacts()?;
    let object_bytes = emit_host_native_object_with_metadata(mir, path, source)?;
    let temp_object = temporary_direct_object_path(output_path);
    let temp_staticlib = temporary_direct_staticlib_path(output_path);
    fs::write(&temp_object, object_bytes).map_err(|error| {
        format!(
            "failed to write direct backend object `{}`: {}",
            temp_object.display(),
            error
        )
    })?;
    let archive_stamp_before = fs::metadata(&native_runtime.staticlib)
        .ok()
        .and_then(|metadata| runtime_archive_memo_stamp(&native_runtime.staticlib, &metadata));
    let staticlib_bytes = fs::read(&native_runtime.staticlib).map_err(|error| {
        format!(
            "failed to read Aura runtime library `{}`: {}",
            native_runtime.staticlib.display(),
            error
        )
    })?;
    let archive_stamp_after = fs::metadata(&native_runtime.staticlib)
        .ok()
        .and_then(|metadata| runtime_archive_memo_stamp(&native_runtime.staticlib, &metadata));
    let stable_archive_stamp =
        archive_stamp_before.filter(|before| archive_stamp_after.as_ref() == Some(before));
    // Return the identity of the exact bytes staged for this link. A Cargo
    // refresh can replace the workspace archive after pre-build lookup, so
    // storing under the earlier identity would make the immediate warm run
    // miss and could associate the binary with the wrong runtime.
    let runtime_identity = NativeRuntimeIdentity {
        archive_sha256: aura_compiler::sha256_hex(&staticlib_bytes),
        native_link_args: native_runtime.native_link_args.clone(),
    };
    fs::write(&temp_staticlib, &staticlib_bytes).map_err(|error| {
        format!(
            "failed to stage Aura runtime library `{}` as `{}`: {}",
            native_runtime.staticlib.display(),
            temp_staticlib.display(),
            error
        )
    })?;
    if let Some(stamp) = stable_archive_stamp {
        let current_stamp = fs::metadata(&native_runtime.staticlib)
            .ok()
            .and_then(|metadata| runtime_archive_memo_stamp(&native_runtime.staticlib, &metadata));
        if current_stamp.as_deref() == Some(stamp.as_str()) {
            write_runtime_identity_memos(&stamp, &runtime_identity);
        }
    }
    // The linker consumes the private staticlib snapshot written above. Once
    // that snapshot and its identity memo exist, another process may refresh
    // the workspace runtime without changing this build's inputs.
    drop(runtime_lock);

    let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let mut command = Command::new(cc);
    command
        .arg(&temp_object)
        .arg(&temp_staticlib)
        .arg("-o")
        .arg(output_path);
    for arg in &native_runtime.native_link_args {
        command.arg(arg);
    }
    command.args(user_binary_link_args(std::env::consts::OS));

    let result = command
        .output()
        .map_err(|error| format!("failed to run native linker for direct backend: {}", error));

    let _ = fs::remove_file(&temp_object);
    let _ = fs::remove_file(&temp_staticlib);

    let output = result?;
    if !output.status.success() {
        return Err(format!(
            "direct backend link failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    strip_user_binary(output_path)?;
    Ok(runtime_identity)
}

fn build_mir_runtime_binary(
    path: &str,
    source: &str,
    mir: &MirModule,
    output_path: &Path,
) -> std::result::Result<(), String> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create output directory `{}`: {}",
                parent.display(),
                error
            )
        })?;
    }

    let native_runtime = ensure_native_runtime_artifacts()?;
    let mir_json =
        serde_json::to_vec(mir).map_err(|error| format!("failed to serialize MIR: {}", error))?;
    let launcher_source =
        emit_mir_runtime_launcher_source(&mir_json, path.as_bytes(), source.as_bytes());
    let temp_source = temporary_mir_runtime_source_path(output_path);
    let temp_staticlib = temporary_direct_staticlib_path(output_path);
    write_unique_temp_file(
        &temp_source,
        launcher_source.as_bytes(),
        "MIR runtime launcher source",
    )?;
    let staticlib_bytes = fs::read(&native_runtime.staticlib).or_else(|_| {
        resolve_static_library_path_in_target_dir(native_runtime_target_dir(), current_profile())
            .and_then(|refreshed| {
                fs::read(&refreshed).map_err(|error| {
                    format!(
                        "failed to read Aura runtime library `{}`: {}",
                        refreshed.display(),
                        error
                    )
                })
            })
    })?;
    write_unique_temp_file(
        &temp_staticlib,
        &staticlib_bytes,
        "staged Aura runtime library",
    )?;

    let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let mut command = Command::new(cc);
    command
        .arg(&temp_source)
        .arg(&temp_staticlib)
        .arg("-o")
        .arg(output_path);
    for arg in &native_runtime.native_link_args {
        command.arg(arg);
    }
    command.args(user_binary_link_args(std::env::consts::OS));

    let result = command.output().map_err(|error| {
        format!(
            "failed to run native linker for MIR runtime backend: {}",
            error
        )
    });

    let _ = fs::remove_file(&temp_source);
    let _ = fs::remove_file(&temp_staticlib);

    let output = result?;
    if !output.status.success() {
        return Err(format!(
            "MIR runtime backend link failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    strip_user_binary(output_path)?;
    Ok(())
}

fn user_binary_strip_command(path: &Path) -> Command {
    let mut command = Command::new("strip");
    // Keep globals: package-authorized FFI may resolve process-global symbols.
    command.args(["-S", "-x"]).arg(path);
    command
}

fn strip_user_binary(path: &Path) -> std::result::Result<(), String> {
    if !matches!(std::env::consts::OS, "macos" | "linux") {
        return Ok(());
    }
    let output = user_binary_strip_command(path)
        .output()
        .map_err(|error| format!("failed to strip user binary: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "user binary stripping failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

fn user_binary_link_args(os: &str) -> &'static [&'static str] {
    match os {
        "macos" => &["-Wl,-dead_strip"],
        "linux" => &["-Wl,--gc-sections"],
        _ => &[],
    }
}

fn temporary_direct_object_path(output_path: &Path) -> PathBuf {
    let file_name = output_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("aura-output");
    let unique = format!(
        "aura-direct-object-{}-{}-{}.o",
        file_name,
        std::process::id(),
        system_time_nanos()
    );
    std::env::temp_dir().join(unique)
}

fn temporary_mir_runtime_source_path(output_path: &Path) -> PathBuf {
    let file_name = output_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("aura-output");
    let unique = format!(
        "aura-mir-runtime-{}-{}-{}.c",
        file_name,
        std::process::id(),
        system_time_nanos()
    );
    std::env::temp_dir().join(unique)
}

fn temporary_direct_staticlib_path(output_path: &Path) -> PathBuf {
    let file_name = output_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("aura-output");
    let unique = format!(
        "aura-direct-runtime-{}-{}-{}.a",
        file_name,
        std::process::id(),
        system_time_nanos()
    );
    std::env::temp_dir().join(unique)
}

fn write_unique_temp_file(path: &Path, contents: &[u8], description: &str) -> Result<(), String> {
    write_unique_temp_file_with_writer(path, description, |file| file.write_all(contents))
}

fn write_unique_temp_file_with_writer(
    path: &Path,
    description: &str,
    writer: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            format!(
                "failed to create {} `{}`: {}",
                description,
                path.display(),
                error
            )
        })?;

    let write_result = writer(&mut file).map_err(|error| {
        format!(
            "failed to write {} `{}`: {}",
            description,
            path.display(),
            error
        )
    });
    let flush_result = if write_result.is_ok() {
        file.flush().map_err(|error| {
            format!(
                "failed to flush {} `{}`: {}",
                description,
                path.display(),
                error
            )
        })
    } else {
        Ok(())
    };

    let result = write_result.and(flush_result);
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn emit_mir_runtime_launcher_source(mir_json: &[u8], source_path: &[u8], source: &[u8]) -> String {
    fn render_bytes(name: &str, bytes: &[u8]) -> String {
        let mut rendered = String::new();
        rendered.push_str(&format!("static const uint8_t {}[] = {{", name));
        if bytes.is_empty() {
            rendered.push('0');
        } else {
            for (index, byte) in bytes.iter().enumerate() {
                if index > 0 {
                    rendered.push_str(", ");
                }
                rendered.push_str(&byte.to_string());
            }
        }
        rendered.push_str("};\n");
        rendered
    }

    format!(
        "#include <stddef.h>\n#include <stdint.h>\n\nextern int aura_native_run(const uint8_t*, size_t, const uint8_t*, size_t, const uint8_t*, size_t);\n\n{}{}{}int main(void) {{\n    return aura_native_run(AURA_MIR, {mir_len}, AURA_SOURCE_PATH, {path_len}, AURA_SOURCE, {source_len});\n}}\n",
        render_bytes("AURA_MIR", mir_json),
        render_bytes("AURA_SOURCE_PATH", source_path),
        render_bytes("AURA_SOURCE", source),
        mir_len = mir_json.len(),
        path_len = source_path.len(),
        source_len = source.len(),
    )
}

fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest_dir.parent().and_then(|path| path.parent()) {
        return root.to_path_buf();
    }
    manifest_dir
}

fn system_time_nanos() -> u128 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(_) => 0,
    }
}

struct NativeRuntimeArtifacts {
    staticlib: PathBuf,
    native_link_args: Vec<String>,
}

fn ensure_native_runtime_artifacts() -> std::result::Result<NativeRuntimeArtifacts, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("failed to locate the running aura executable: {}", error))?;
    if let Some(installed) = resolve_installed_runtime_artifacts_from_executable(&executable)? {
        return Ok(installed);
    }

    let staticlib = build_native_runtime_staticlib()?
        .or_else(|| {
            resolve_static_library_path_in_target_dir(
                native_runtime_target_dir(),
                current_profile(),
            )
            .ok()
        })
        .ok_or_else(|| {
            format!(
                "failed to locate compiled Aura runtime library from Cargo artifact output or `{}`",
                repo_root()
                    .join("target")
                    .join(current_profile())
                    .join(static_library_file_name())
                    .display()
            )
        })?;
    if !staticlib.exists() {
        return Err(format!(
            "failed to locate compiled Aura runtime library `{}` after build",
            staticlib.display()
        ));
    }

    let native_link_args = query_native_runtime_link_args()?;

    Ok(NativeRuntimeArtifacts {
        staticlib,
        native_link_args,
    })
}

fn resolve_installed_runtime_artifacts_from_executable(
    executable: &Path,
) -> std::result::Result<Option<NativeRuntimeArtifacts>, String> {
    let Some(prefix) = executable.parent().and_then(Path::parent) else {
        return Ok(None);
    };
    let runtime_dir = prefix.join("lib").join("aura");
    let staticlib = runtime_dir.join(static_library_file_name());
    let manifest = runtime_dir.join("native-link-args.json");
    let staticlib_exists = staticlib.is_file();
    let manifest_exists = manifest.is_file();

    if !staticlib_exists && !manifest_exists {
        return Ok(None);
    }
    if !staticlib_exists || !manifest_exists {
        return Err(format!(
            "incomplete Aura runtime installation in `{}`: expected both `{}` and `{}`",
            runtime_dir.display(),
            staticlib.display(),
            manifest.display()
        ));
    }

    let manifest_bytes = fs::read(&manifest).map_err(|error| {
        format!(
            "failed to read Aura runtime link manifest `{}`: {}",
            manifest.display(),
            error
        )
    })?;
    let native_link_args =
        serde_json::from_slice::<Vec<String>>(&manifest_bytes).map_err(|error| {
            format!(
                "invalid Aura runtime link manifest `{}`: {}",
                manifest.display(),
                error
            )
        })?;
    let native_link_args = validate_native_link_args(native_link_args).map_err(|error| {
        format!(
            "invalid Aura runtime link manifest `{}`: {error}",
            manifest.display()
        )
    })?;

    Ok(Some(NativeRuntimeArtifacts {
        staticlib,
        native_link_args,
    }))
}

fn build_native_runtime_staticlib() -> std::result::Result<Option<PathBuf>, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.current_dir(repo_root());
    configure_native_runtime_cargo(
        &mut command,
        std::env::var_os("LLVM_PROFILE_FILE").is_some(),
    );
    command
        .arg("build")
        .arg("-q")
        .arg("-p")
        .arg("aura-compiler")
        .arg("--lib")
        .arg("--message-format=json-render-diagnostics");
    if current_profile() == "release" {
        command.arg("--release");
    }

    let output = command
        .output()
        .map_err(|error| format!("failed to build Aura runtime artifacts: {}", error))?;

    if !output.status.success() {
        return Err(format!(
            "failed to build Aura runtime artifacts:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(parse_static_library_artifact_path(&output.stdout))
}

fn query_native_runtime_link_args() -> std::result::Result<Vec<String>, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.current_dir(repo_root());
    configure_native_runtime_cargo(
        &mut command,
        std::env::var_os("LLVM_PROFILE_FILE").is_some(),
    );
    command
        .arg("rustc")
        .arg("-q")
        .arg("-p")
        .arg("aura-compiler")
        .arg("--lib");
    if current_profile() == "release" {
        command.arg("--release");
    }
    command.arg("--").arg("--print").arg("native-static-libs");

    let output = command
        .output()
        .map_err(|error| format!("failed to query Aura runtime link args: {}", error))?;
    if !output.status.success() {
        return Err(format!(
            "failed to query Aura runtime link args:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    parse_native_static_libs(&String::from_utf8_lossy(&output.stderr))
}

fn configure_native_runtime_cargo(command: &mut Command, coverage_active: bool) {
    // Cargo's inherited terminal-color setting affects even captured output.
    // Link arguments are machine-readable inputs, so every runtime Cargo
    // subprocess must produce deterministic, uncolored output.
    command.env("CARGO_TERM_COLOR", "never");

    if !coverage_active {
        return;
    }

    for variable in [
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_LLVM_COV",
        "CARGO_LLVM_COV_BUILD_DIR",
        "CARGO_LLVM_COV_SHOW_ENV",
        "CARGO_LLVM_COV_TARGET_DIR",
        "LLVM_PROFILE_FILE",
        "RUSTC_WRAPPER",
        "RUSTDOCFLAGS",
        "RUSTFLAGS",
        "__CARGO_LLVM_COV_RUSTC_WRAPPER",
        "__CARGO_LLVM_COV_RUSTC_WRAPPER_CRATE_NAMES",
        "__CARGO_LLVM_COV_RUSTC_WRAPPER_RUSTFLAGS",
    ] {
        command.env_remove(variable);
    }
    command.env(
        "CARGO_TARGET_DIR",
        repo_root().join("target/native-runtime-uninstrumented"),
    );
}

fn parse_static_library_artifact_path(stdout: &[u8]) -> Option<PathBuf> {
    let stdout = std::str::from_utf8(stdout).ok()?;
    let mut candidate = None;
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        if message.get("reason").and_then(|value| value.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let Some(target) = message.get("target") else {
            continue;
        };
        if target.get("name").and_then(|value| value.as_str()) != Some("aura_compiler") {
            continue;
        }
        let Some(filenames) = message.get("filenames").and_then(|value| value.as_array()) else {
            continue;
        };
        for filename in filenames {
            let Some(path) = filename.as_str() else {
                continue;
            };
            let path = PathBuf::from(path);
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if name.starts_with("libaura_compiler") && name.ends_with(".a") {
                candidate = Some(path);
            }
        }
    }
    candidate
}

fn current_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

fn static_library_file_name() -> &'static str {
    "libaura_compiler.a"
}

#[cfg(test)]
fn resolve_static_library_path(
    root: PathBuf,
    profile: &str,
) -> std::result::Result<PathBuf, String> {
    resolve_static_library_path_in_target_dir(root.join("target"), profile)
}

fn native_runtime_target_dir() -> PathBuf {
    if std::env::var_os("LLVM_PROFILE_FILE").is_some() {
        return repo_root().join("target/native-runtime-uninstrumented");
    }
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(target) if Path::new(&target).is_absolute() => PathBuf::from(target),
        Some(target) => repo_root().join(target),
        None => repo_root().join("target"),
    }
}

fn resolve_static_library_path_in_target_dir(
    target_dir: PathBuf,
    profile: &str,
) -> std::result::Result<PathBuf, String> {
    let primary = target_dir.join(profile).join(static_library_file_name());
    if primary.exists() {
        return Ok(primary);
    }

    let deps_dir = target_dir.join(profile).join("deps");
    let mut candidates = fs::read_dir(&deps_dir)
        .map_err(|error| {
            format!(
                "failed to inspect Aura runtime library directory `{}`: {}",
                deps_dir.display(),
                error
            )
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("libaura_compiler-") && name.ends_with(".a"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        if let Some(candidate) = candidates.pop() {
            return Ok(candidate);
        }
    }
    if !candidates.is_empty() {
        candidates.sort();
        return Err(format!(
            "found multiple hashed Aura runtime archives in `{}` but no canonical `{}`: {}; rebuild the workspace so the current static runtime path is unambiguous",
            deps_dir.display(),
            primary.display(),
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    Err(format!(
        "failed to locate compiled Aura runtime library `{}` or a matching archive in `{}`",
        primary.display(),
        deps_dir.display()
    ))
}

fn strip_ansi_escape_sequences(output: &str) -> String {
    let bytes = output.as_bytes();
    let mut clean = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != 0x1b {
            clean.push(bytes[cursor]);
            cursor += 1;
            continue;
        }

        let escape_start = cursor;
        cursor += 1;
        let Some(introducer) = bytes.get(cursor).copied() else {
            clean.push(0x1b);
            break;
        };
        cursor += 1;
        let sequence_end = match introducer {
            // CSI grammar: parameter bytes, then intermediate bytes, then one
            // final byte. Anything else is malformed and must survive for
            // control-character validation.
            b'[' => {
                while bytes
                    .get(cursor)
                    .is_some_and(|byte| (0x30..=0x3f).contains(byte))
                {
                    cursor += 1;
                }
                while bytes
                    .get(cursor)
                    .is_some_and(|byte| (0x20..=0x2f).contains(byte))
                {
                    cursor += 1;
                }
                bytes
                    .get(cursor)
                    .filter(|&&byte| (0x40..=0x7e).contains(&byte))
                    .map(|_| cursor + 1)
            }
            // OSC can end with BEL or ST. The other ANSI string controls end
            // with ST. Embedded controls or a non-terminating ESC make the
            // sequence malformed rather than content to discard.
            b']' | b'P' | b'X' | b'^' | b'_' => {
                let allow_bel = introducer == b']';
                let payload_start = cursor;
                let mut end = None;
                while cursor < bytes.len() {
                    if bytes[cursor] == 0x1b {
                        if bytes.get(cursor + 1) == Some(&b'\\') {
                            let valid_payload = output
                                .get(payload_start..cursor)
                                .is_some_and(|payload| !payload.chars().any(char::is_control));
                            if valid_payload {
                                end = Some(cursor + 2);
                            }
                        }
                        break;
                    }
                    if bytes[cursor] == 0x07 {
                        if allow_bel {
                            let valid_payload = output
                                .get(payload_start..cursor)
                                .is_some_and(|payload| !payload.chars().any(char::is_control));
                            if valid_payload {
                                end = Some(cursor + 1);
                            }
                        }
                        break;
                    }
                    cursor += 1;
                }
                end
            }
            // A general ANSI escape may contain zero or more intermediate
            // bytes followed by one final byte.
            0x20..=0x2f => {
                while bytes
                    .get(cursor)
                    .is_some_and(|byte| (0x20..=0x2f).contains(byte))
                {
                    cursor += 1;
                }
                bytes
                    .get(cursor)
                    .filter(|&&byte| (0x30..=0x7e).contains(&byte))
                    .map(|_| cursor + 1)
            }
            0x30..=0x7e => Some(cursor),
            _ => None,
        };
        if let Some(sequence_end) = sequence_end {
            cursor = sequence_end;
        } else {
            // Preserve the ESC and rescan the rest as ordinary text. That
            // guarantees malformed input reaches link-argument validation.
            clean.push(bytes[escape_start]);
            cursor = escape_start + 1;
        }
    }
    String::from_utf8(clean).expect("removing ASCII escape sequences preserves UTF-8")
}

fn validate_native_link_args(
    native_link_args: Vec<String>,
) -> std::result::Result<Vec<String>, String> {
    if let Some(argument) = native_link_args
        .iter()
        .find(|argument| argument.chars().any(char::is_control))
    {
        return Err(format!(
            "Aura runtime link argument {argument:?} contains a control character"
        ));
    }
    Ok(native_link_args)
}

fn parse_native_static_libs(output: &str) -> std::result::Result<Vec<String>, String> {
    let output = strip_ansi_escape_sequences(output);
    let native_link_args = output
        .lines()
        .rev()
        .find_map(|line| line.split_once("native-static-libs:"))
        .map(|(_, libs)| {
            libs.split_whitespace()
                .map(|item| item.to_string())
                .collect()
        })
        .unwrap_or_default();
    validate_native_link_args(native_link_args)
}

fn write_stdout(text: &str) {
    let mut stdout = io::stdout().lock();
    if let Err(error) = stdout
        .write_all(text.as_bytes())
        .and_then(|_| stdout.flush())
    {
        if error.kind() == io::ErrorKind::BrokenPipe {
            process::exit(0);
        }
        eprintln!("failed to write to stdout: {}", error);
        process::exit(1);
    }
}

fn usage_text() -> &'static str {
    "usage: aura <check|run> [--format human|json] <file.au>\n\
       or: aura <ast|ast-json|mir|analyze> <file.au>\n\
       or: aura <check|run|ast|ast-json|mir|analyze> --stdin <virtual-path>\n\
       or: aura run [--backend mir|direct|auto] <file.au> [-- <program-args>...]\n\
       or: aura build -o <output> [--backend auto|direct] [--format human|json] <file.au>\n\
       or: aura build -o <output> [--backend auto|direct] [--format human|json] --stdin <virtual-path>\n\
       or: aura complete --line <n> --character <n> [--trigger .] <file.au>\n\
       or: aura complete --line <n> --character <n> [--trigger .] --stdin <virtual-path>\n\
       or: aura lsp\n\
       or: aura new <project-path>\n\
       or: aura fmt [--check] [path ...]\n\
       or: aura test [-k <substring>] [--format json] [--timeout-ms <n>] [path ...]\n\
       or: aura deps update [package]\n\
       or: aura upgrade\n\
       or: aura help\n\
       or: aura version"
}

fn print_usage_and_exit(exit_code: i32) -> ! {
    if exit_code == 0 {
        write_stdout(&format!("{}\n", usage_text()));
    } else {
        eprintln!("{}", usage_text());
    }
    process::exit(exit_code);
}

fn print_version_and_exit() -> ! {
    write_stdout(&format!(
        "aura {}-{} ({})\n",
        env!("CARGO_PKG_VERSION"),
        env!("AURA_BUILD_CHANNEL"),
        env!("AURA_BUILD_COMMIT")
    ));
    process::exit(0);
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use super::{
        acquire_native_build_lock_at, create_private_directory,
        create_verified_native_launch_lease, spawn_native_binary_with_diagnostic_mode,
        spawn_verified_native_binary_with_lease, wait_for_native_binary, NativeExecutionOutcome,
    };
    use super::{
        cleanup_stale_native_cache_artifacts_at, cleanup_stale_verified_native_directories,
        configure_native_runtime_cargo, create_private_cache_directory_all, lsp_response_for_line,
        native_cache_key, native_cache_key_for_semantic_schema, native_execution_failure,
        native_launch_error_invalidates_cache, native_runtime_identity_material,
        parse_native_static_libs, parse_run_backend, parse_static_library_artifact_path,
        query_native_runtime_link_args, remove_native_cache_entry_if_unchanged,
        resolve_installed_runtime_artifacts_from_executable, resolve_static_library_path,
        runtime_archive_memo_stamp, select_build_backend, select_run_outcome,
        verified_native_execution_failure, write_unique_temp_file,
        write_unique_temp_file_with_writer, BuildBackend, NativeExecutionError, NativeRunOutcome,
        NativeRuntimeIdentity, RunBackend, SelectedBuildBackend, VerifiedNativeLaunchError,
        NATIVE_CACHE_ENTRY_ID,
    };

    fn unique_temp_dir(name: &str) -> PathBuf {
        let unique = format!(
            "aura-aura-tests-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).expect("temp dir should exist");
        path
    }

    #[test]
    fn user_binary_link_flags_collect_unused_sections_on_supported_hosts() {
        assert_eq!(super::user_binary_link_args("macos"), &["-Wl,-dead_strip"]);
        assert_eq!(
            super::user_binary_link_args("linux"),
            &["-Wl,--gc-sections"]
        );
        assert!(super::user_binary_link_args("windows").is_empty());
    }

    #[test]
    fn user_binary_strip_command_keeps_global_symbols_for_ffi() {
        let command = super::user_binary_strip_command(std::path::Path::new("program"));
        assert_eq!(command.get_program(), "strip");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec!["-S", "-x", "program"]
        );
    }

    #[test]
    fn stripped_direct_and_mir_launchers_keep_source_frames_without_source_files() {
        let root = unique_temp_dir("stripped-runtime-frames");
        let source =
            "def fail():\n    assert false, \"kept diagnostic\"\n\ndef main():\n    fail()\n";
        let source_path = root.join("removed.au");
        let mir = aura_compiler::lower_source_to_mir(source).unwrap();
        for direct in [true, false] {
            let binary = root.join(if direct { "direct" } else { "mir-launcher" });
            if direct {
                super::build_direct_native_binary(
                    source_path.to_str().unwrap(),
                    source,
                    &mir,
                    &binary,
                    None,
                )
                .unwrap();
            } else {
                super::build_mir_runtime_binary(
                    source_path.to_str().unwrap(),
                    source,
                    &mir,
                    &binary,
                )
                .unwrap();
            }
            assert!(!source_path.exists());
            let output = Command::new(&binary)
                .current_dir(&root)
                .env("CARGO", root.join("no-cargo"))
                .output()
                .unwrap();
            assert!(!output.status.success());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("AU4001"), "{stderr}");
            assert!(stderr.contains("kept diagnostic"), "{stderr}");
            assert!(stderr.contains("fail"), "{stderr}");
            assert!(stderr.contains("main"), "{stderr}");
            assert!(stderr.contains("removed.au"), "{stderr}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolve_static_library_path_prefers_primary_staticlib() {
        let root = unique_temp_dir("primary-staticlib");
        let target = root.join("target").join("debug");
        let deps = target.join("deps");
        fs::create_dir_all(&deps).expect("deps dir should exist");
        let primary = target.join("libaura_compiler.a");
        fs::write(&primary, b"primary").expect("primary staticlib should write");
        fs::write(deps.join("libaura_compiler-old.a"), b"stale hashed archive")
            .expect("hashed archive should write");

        let resolved = resolve_static_library_path(root.clone(), "debug")
            .expect("should resolve runtime library");
        assert_eq!(resolved, primary);

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn runtime_archive_memo_stamp_detects_same_size_same_mtime_replacement() {
        let root = unique_temp_dir("runtime-memo-stamp");
        let archive = root.join("libaura_compiler.a");
        let replacement = root.join("replacement.a");
        fs::write(&archive, b"archive-a").expect("first archive should write");
        let first_metadata = fs::metadata(&archive).expect("first metadata should exist");
        let first_modified = first_metadata
            .modified()
            .expect("first archive should have an mtime");
        let first_stamp = runtime_archive_memo_stamp(&archive, &first_metadata)
            .expect("Unix archive metadata should have a reliable stamp");

        fs::write(&replacement, b"archive-b").expect("replacement archive should write");
        let replacement_file = fs::OpenOptions::new()
            .write(true)
            .open(&replacement)
            .expect("replacement archive should reopen");
        replacement_file
            .set_times(fs::FileTimes::new().set_modified(first_modified))
            .expect("replacement mtime should be restorable");
        drop(replacement_file);
        fs::rename(&replacement, &archive).expect("replacement should install atomically");

        let second_metadata = fs::metadata(&archive).expect("replacement metadata should exist");
        assert_eq!(second_metadata.len(), first_metadata.len());
        assert_eq!(
            second_metadata.modified().expect("replacement mtime"),
            first_modified
        );
        let second_stamp = runtime_archive_memo_stamp(&archive, &second_metadata)
            .expect("Unix replacement should have a reliable stamp");
        assert_ne!(
            second_stamp, first_stamp,
            "file identity or ctime must invalidate a same-size, same-mtime archive memo"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn native_build_lock_rejects_symlinks_and_shared_writable_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = unique_temp_dir("native-build-lock-safety");
        let target = root.join("target");
        let symlink_lock = root.join("symlink.lock");
        fs::write(&target, b"external").expect("symlink target should be writable");
        symlink(&target, &symlink_lock).expect("lock symlink should be creatable");
        let symlink_error = acquire_native_build_lock_at(&symlink_lock, None)
            .err()
            .expect("a lock symlink must be rejected");
        assert!(
            symlink_error.contains("failed to open native build lock"),
            "{symlink_error}"
        );

        let writable_lock = root.join("writable.lock");
        fs::write(&writable_lock, []).expect("writable lock should be creatable");
        fs::set_permissions(&writable_lock, fs::Permissions::from_mode(0o666))
            .expect("writable lock permissions should be configurable");
        let writable_error = acquire_native_build_lock_at(&writable_lock, None)
            .err()
            .expect("a shared-writable lock must be rejected");
        assert!(
            writable_error.contains("not group- or world-writable"),
            "{writable_error}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_runtime_identity_preserves_archive_and_ordered_link_inputs() {
        let identity = |hash: &str, args: &[&str]| NativeRuntimeIdentity {
            archive_sha256: hash.repeat(64),
            native_link_args: args.iter().map(|arg| (*arg).to_string()).collect(),
        };
        let base = native_runtime_identity_material(&identity("a", &["-lone", "-ltwo"]))
            .expect("runtime identity should serialize");

        for changed in [
            identity("b", &["-lone", "-ltwo"]),
            identity("a", &["-ltwo", "-lone"]),
            identity("a", &["-lone", "-ltwo", "-ltwo"]),
            identity("a", &["-lon", "e-ltwo"]),
        ] {
            assert_ne!(
                native_runtime_identity_material(&changed)
                    .expect("changed runtime identity should serialize"),
                base,
                "archive bytes, argument order, duplication, and boundaries must contribute independently"
            );
        }
    }

    #[test]
    fn native_cache_key_independently_tracks_the_semantic_interface_schema() {
        let mir = aura_compiler::lower_source_to_mir("def main() -> int32:\n    return 0\n")
            .expect("cache-key fixture should lower");
        let runtime_identity = NativeRuntimeIdentity {
            archive_sha256: "a".repeat(64),
            native_link_args: vec!["-laura_compiler".to_string()],
        };

        let old_key = native_cache_key_for_semantic_schema(&mir, &runtime_identity, 1)
            .expect("old-schema key should serialize");
        let current_key = native_cache_key_for_semantic_schema(
            &mir,
            &runtime_identity,
            aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION,
        )
        .expect("current-schema key should serialize");
        let production_key =
            native_cache_key(&mir, &runtime_identity).expect("production key should serialize");

        assert_ne!(
            old_key, current_key,
            "identical MIR and runtime inputs must not reuse an artifact across semantic schemas"
        );
        assert_eq!(
            production_key, current_key,
            "every production native key must bind the compiler-owned semantic schema"
        );
    }

    #[test]
    fn lsp_service_rejects_a_client_from_another_semantic_schema() {
        let response = lsp_response_for_line(
            &serde_json::json!({
                "id": 7,
                "semantic_interface_version": 1,
                "method": "analyze",
                "path": "/virtual/main.au",
                "source": "def main():\n    pass\n"
            })
            .to_string(),
        );

        assert_eq!(response["id"], 7);
        assert_eq!(
            response["semantic_interface_version"],
            aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION
        );
        assert!(
            response["error"]
                .as_str()
                .is_some_and(|message| message.contains("semantic schema mismatch")),
            "mismatched clients must receive an explicit schema error: {response}"
        );
    }

    #[test]
    fn every_lsp_service_error_identifies_the_compiler_semantic_schema() {
        let malformed = lsp_response_for_line("not JSON");
        assert_eq!(
            malformed["semantic_interface_version"],
            aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION
        );
        assert!(malformed["error"].is_string());

        let missing_schema = lsp_response_for_line(
            &serde_json::json!({
                "id": 8,
                "method": "analyze",
                "path": "/virtual/main.au",
                "source": "def main():\n    pass\n"
            })
            .to_string(),
        );
        assert_eq!(
            missing_schema["semantic_interface_version"],
            aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION
        );
        assert!(missing_schema["error"]
            .as_str()
            .is_some_and(|message| message.contains("client reported `<missing>`")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_executable_shape_errors_invalidate_native_cache_entries() {
        for raw_error in [
            libc::ENOEXEC,
            libc::EBADEXEC,
            libc::EBADARCH,
            libc::EBADMACHO,
            libc::ESHLIBVERS,
        ] {
            assert!(
                native_launch_error_invalidates_cache(&io::Error::from_raw_os_error(raw_error)),
                "macOS executable-format error {raw_error} must rebuild the cache entry"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn stale_verified_launch_cleanup_never_removes_a_live_owner() {
        let root = unique_temp_dir("live-verified-launch");
        let launch = root.join(format!("aura-verified-native-{}-1", std::process::id()));
        fs::create_dir(&launch).expect("launch directory should exist");
        fs::File::open(&launch)
            .expect("launch directory should open")
            .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
            .expect("launch directory should be backdated");

        cleanup_stale_verified_native_directories(&root);
        assert!(
            launch.is_dir(),
            "age must never override proof that a launch owner is still live"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn verified_launch_lease_survives_parent_handle_until_native_child_exits() {
        let root = unique_temp_dir("inherited-verified-launch-lease");
        let launch = root.join(format!("aura-verified-native-{}-1", libc::pid_t::MAX));
        create_private_directory(&launch).expect("launch directory should be private");
        let lease =
            create_verified_native_launch_lease(&launch).expect("launch lease should exist");
        let mut child = spawn_verified_native_binary_with_lease(
            PathBuf::from("/bin/sleep").as_path(),
            &["1".to_string()],
            &lease,
        )
        .expect("native child should start with the inherited lease");

        // Simulate the aura parent dying after spawn: only the exec'd child
        // retains the open-file-description lock.
        drop(lease);
        cleanup_stale_verified_native_directories(&root);
        assert!(
            launch.is_dir(),
            "cleanup must preserve a launch directory while its native child holds the lease"
        );

        let status = child.wait().expect("native child should remain waitable");
        assert!(status.success());
        cleanup_stale_verified_native_directories(&root);
        assert!(
            !launch.exists(),
            "the abandoned launch directory should be collected after the child releases its lease"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    fn hold_file_descriptors_above_posix_shell_redirection_limit() -> Vec<std::fs::File> {
        (0..16)
            .map(|_| {
                std::fs::File::open("/dev/null")
                    .expect("the high-descriptor regression needs /dev/null")
            })
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn private_native_diagnostic_channel_distinguishes_status_from_a_trap() {
        let ordinary = spawn_native_binary_with_diagnostic_mode(
            PathBuf::from("/bin/sh").as_path(),
            &["-c".to_string(), "exit 1".to_string()],
            true,
        )
        .expect("ordinary nonzero native child should start");
        assert!(matches!(
            wait_for_native_binary(ordinary).expect("ordinary status should collect"),
            NativeExecutionOutcome::Exited(1)
        ));

        let _high_descriptor_guard = hold_file_descriptors_above_posix_shell_redirection_limit();
        let record = r#"{"code":"AU4001","severity":"error","message":"native trap","primary_span":null,"secondary_spans":[],"notes":[],"help":[],"edits":[],"call_frames":[],"task_ancestry":[]}"#;
        let script = format!(
            "printf '\\001' > \"/dev/fd/${}\"; \
             printf '%s' \"$1\" > \"/dev/fd/${}\"; exit 1",
            aura_compiler::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV,
            aura_compiler::INTERNAL_DIAGNOSTIC_FD_ENV,
        );
        let trapped = spawn_native_binary_with_diagnostic_mode(
            PathBuf::from("/bin/sh").as_path(),
            &[
                "-c".to_string(),
                script,
                "aura-channel-test".to_string(),
                record.to_string(),
            ],
            true,
        )
        .expect("diagnostic-writing native child should start");
        match wait_for_native_binary(trapped).expect("valid trap record should collect") {
            NativeExecutionOutcome::Trapped(diagnostic) => {
                assert_eq!(diagnostic.code, "AU4001");
                assert_eq!(diagnostic.message, "native trap");
            }
            NativeExecutionOutcome::Exited(code) => {
                panic!("a valid record must not be mistaken for ordinary status {code}")
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_native_diagnostic_channel_rejects_malformed_multiple_and_oversized_records() {
        let _high_descriptor_guard = hold_file_descriptors_above_posix_shell_redirection_limit();
        let malformed_cases = [
            ("malformed", "not-json".to_string()),
            (
                "multiple",
                concat!(
                    r#"{"code":"AU4001","severity":"error","message":"first","primary_span":null,"secondary_spans":[],"notes":[],"help":[],"edits":[],"call_frames":[],"task_ancestry":[]}"#,
                    r#"{"code":"AU4001","severity":"error","message":"second","primary_span":null,"secondary_spans":[],"notes":[],"help":[],"edits":[],"call_frames":[],"task_ancestry":[]}"#
                )
                .to_string(),
            ),
        ];
        for (label, record) in malformed_cases {
            let script = format!(
                "printf '\\001' > \"/dev/fd/${}\"; \
                 printf '%s' \"$1\" > \"/dev/fd/${}\"; exit 1",
                aura_compiler::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV,
                aura_compiler::INTERNAL_DIAGNOSTIC_FD_ENV,
            );
            let spawned = spawn_native_binary_with_diagnostic_mode(
                PathBuf::from("/bin/sh").as_path(),
                &[
                    "-c".to_string(),
                    script,
                    "aura-channel-test".to_string(),
                    record,
                ],
                true,
            )
            .unwrap_or_else(|error| panic!("{label} native child should start: {error}"));
            let error = wait_for_native_binary(spawned)
                .err()
                .unwrap_or_else(|| panic!("{label} record must be rejected"));
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{label}");
        }

        let script = format!(
            "printf '\\001' > \"/dev/fd/${}\"; \
             dd if=/dev/zero bs={} count=1 of=\"/dev/fd/${}\" 2>/dev/null; exit 1",
            aura_compiler::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV,
            aura_compiler::MAX_INTERNAL_DIAGNOSTIC_BYTES + 1,
            aura_compiler::INTERNAL_DIAGNOSTIC_FD_ENV,
        );
        let spawned = spawn_native_binary_with_diagnostic_mode(
            PathBuf::from("/bin/sh").as_path(),
            &["-c".to_string(), script],
            true,
        )
        .expect("oversized-record native child should start");
        let error = wait_for_native_binary(spawned)
            .err()
            .expect("oversized record must be rejected before JSON decoding");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("exceeded"),
            "oversized-record error should identify the bound: {error}"
        );

        for (label, script) in [
            (
                "missing data after trap intent",
                format!(
                    "printf '\\001' > \"/dev/fd/${}\"; exit 1",
                    aura_compiler::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV
                ),
            ),
            (
                "data without trap intent",
                format!(
                    "printf '%s' \"$1\" > \"/dev/fd/${}\"; exit 1",
                    aura_compiler::INTERNAL_DIAGNOSTIC_FD_ENV
                ),
            ),
        ] {
            let spawned = spawn_native_binary_with_diagnostic_mode(
                PathBuf::from("/bin/sh").as_path(),
                &[
                    "-c".to_string(),
                    script,
                    "aura-channel-test".to_string(),
                    r#"{"code":"AU4001","severity":"error","message":"trap","primary_span":null,"secondary_spans":[],"notes":[],"help":[],"edits":[],"call_frames":[],"task_ancestry":[]}"#.to_string(),
                ],
                true,
            )
            .unwrap_or_else(|error| panic!("{label} child should start: {error}"));
            let error = wait_for_native_binary(spawned)
                .err()
                .unwrap_or_else(|| panic!("{label} must be rejected"));
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{label}");
            assert!(matches!(
                native_execution_failure(
                    RunBackend::Auto,
                    NativeExecutionError {
                        message: error.to_string(),
                        post_launch: true,
                    },
                ),
                NativeRunOutcome::Failed(_)
            ));
        }

        let record = r#"{"code":"AU4001","severity":"error","message":"trap","primary_span":null,"secondary_spans":[],"notes":[],"help":[],"edits":[],"call_frames":[],"task_ancestry":[]}"#;
        for (label, marker, exit_status) in [
            ("record with successful exit", "\\001", 0),
            ("invalid trap marker", "X", 1),
            ("multiple trap markers", "\\001\\001", 1),
        ] {
            let script = format!(
                "printf '{marker}' > \"/dev/fd/${}\"; \
                 printf '%s' \"$1\" > \"/dev/fd/${}\"; exit {exit_status}",
                aura_compiler::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV,
                aura_compiler::INTERNAL_DIAGNOSTIC_FD_ENV,
            );
            let spawned = spawn_native_binary_with_diagnostic_mode(
                PathBuf::from("/bin/sh").as_path(),
                &[
                    "-c".to_string(),
                    script,
                    "aura-channel-test".to_string(),
                    record.to_string(),
                ],
                true,
            )
            .unwrap_or_else(|error| panic!("{label} child should start: {error}"));
            let error = wait_for_native_binary(spawned)
                .err()
                .unwrap_or_else(|| panic!("{label} must be rejected"));
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{label}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_signal_termination_is_a_hard_post_launch_failure() {
        let spawned = spawn_native_binary_with_diagnostic_mode(
            PathBuf::from("/bin/sh").as_path(),
            &["-c".to_string(), "kill -TERM $$".to_string()],
            true,
        )
        .expect("signal-terminating native child should start");
        let error =
            wait_for_native_binary(spawned).expect_err("a signal exit is not an Aura status");
        assert!(error.to_string().contains("host signal"));
        assert!(matches!(
            native_execution_failure(
                RunBackend::Auto,
                NativeExecutionError {
                    message: error.to_string(),
                    post_launch: true,
                },
            ),
            NativeRunOutcome::Failed(_)
        ));
    }

    #[test]
    fn cache_invalidation_does_not_delete_a_replacement_entry() {
        let root = unique_temp_dir("conditional-cache-invalidation");
        let entry = root.join("programs").join("a".repeat(64));
        fs::create_dir_all(&entry).expect("replacement entry should exist");
        let key = "a".repeat(64);
        let old_id = format!("{}:{}", key, "b".repeat(64));
        let replacement_id = format!("{}:{}", key, "c".repeat(64));
        fs::write(
            entry.join(NATIVE_CACHE_ENTRY_ID),
            format!("{replacement_id}\n"),
        )
        .expect("replacement identity should write");

        remove_native_cache_entry_if_unchanged(&entry, Some(&old_id));
        assert!(
            entry.is_dir(),
            "an invalidator holding the old identity must restore the replacement"
        );
        assert_eq!(
            fs::read_to_string(entry.join(NATIVE_CACHE_ENTRY_ID))
                .expect("replacement identity should remain")
                .trim(),
            replacement_id
        );

        remove_native_cache_entry_if_unchanged(&entry, Some(&replacement_id));
        assert!(
            !entry.exists(),
            "an invalidator with the current identity should remove that exact entry"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_cache_cleanup_removes_only_old_dead_exact_transients() {
        const DAY_NANOS: u128 = 24 * 60 * 60 * 1_000_000_000;

        let root = unique_temp_dir("native-cache-transient-cleanup");
        let key = "a".repeat(64);
        let owner = 42;
        let old = 1;
        let now = DAY_NANOS * 2;
        let memo = root.join(format!(".runtime-identity-{owner}-{old}"));
        let publish = root.join(format!(".{key}-{owner}-{old}"));
        let discard = root.join(format!(".discard-{key}-{owner}-{old}"));
        let canonical = root.join(&key);
        let malformed = root.join(format!(".discard-not-a-key-{owner}-{old}"));
        let recent = root.join(format!(".runtime-identity-{owner}-{now}"));
        fs::write(&memo, b"partial memo").expect("memo stage should write");
        fs::create_dir(&publish).expect("publish stage should exist");
        fs::create_dir(&discard).expect("discard stage should exist");
        fs::create_dir(&canonical).expect("canonical entry should exist");
        fs::create_dir(&malformed).expect("malformed lookalike should exist");
        fs::write(&recent, b"recent memo").expect("recent memo stage should write");

        cleanup_stale_native_cache_artifacts_at(&root, now, |_| false);
        for transient in [&memo, &publish, &discard] {
            assert!(
                !transient.exists(),
                "old dead transient `{}` should be removed",
                transient.display()
            );
        }
        for preserved in [&canonical, &malformed, &recent] {
            assert!(
                preserved.exists(),
                "non-transient or recent path `{}` must be preserved",
                preserved.display()
            );
        }

        let live = root.join(format!(".{key}-{owner}-{old}"));
        fs::create_dir(&live).expect("live publish stage should exist");
        cleanup_stale_native_cache_artifacts_at(&root, now, |pid| pid == owner);
        assert!(
            live.is_dir(),
            "a positively live owner must override transient age"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn native_cache_rejects_group_or_world_writable_roots() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = unique_temp_dir("native-cache-trust");
        let writable = root.join("writable");
        fs::create_dir(&writable).expect("cache root should be creatable");
        fs::set_permissions(&writable, fs::Permissions::from_mode(0o777))
            .expect("cache root permissions should be adjustable");
        let error = create_private_cache_directory_all(&writable)
            .expect_err("shared-writable cache roots must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            fs::metadata(&writable)
                .expect("rejected root should remain")
                .permissions()
                .mode()
                & 0o777,
            0o777,
            "rejection must not chmod a caller-owned shared directory"
        );

        let real = root.join("real");
        let linked = root.join("linked");
        fs::create_dir(&real).expect("symlink target should exist");
        symlink(&real, &linked).expect("cache-root symlink should be creatable");
        create_private_cache_directory_all(&linked)
            .expect_err("a cache root symlink must not be trusted as a private directory");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn coverage_builds_isolate_uninstrumented_native_runtime_artifacts() {
        let mut command = Command::new("cargo");
        command.env("CARGO_TERM_COLOR", "always");
        command.env("LLVM_PROFILE_FILE", "coverage.profraw");
        command.env("RUSTC_WRAPPER", "cargo-llvm-cov");
        configure_native_runtime_cargo(&mut command, true);

        let environments = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().to_string(),
                    value.map(|value| value.to_string_lossy().to_string()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(environments.get("LLVM_PROFILE_FILE"), Some(&None));
        assert_eq!(environments.get("RUSTC_WRAPPER"), Some(&None));
        assert_eq!(
            environments
                .get("CARGO_TERM_COLOR")
                .and_then(Option::as_ref)
                .map(String::as_str),
            Some("never")
        );
        assert!(environments
            .get("CARGO_TARGET_DIR")
            .and_then(Option::as_ref)
            .is_some_and(|path| path.ends_with("target/native-runtime-uninstrumented")));
    }

    #[cfg(unix)]
    #[test]
    fn native_link_arg_capture_overrides_inherited_always_color_and_strips_ansi() {
        const CHILD_MARKER: &str = "AURA_NATIVE_LINK_CAPTURE_CHILD";
        if std::env::var_os(CHILD_MARKER).is_some() {
            assert_eq!(
                query_native_runtime_link_args().expect("fake Cargo capture should succeed"),
                vec!["-lc", "-lm"]
            );
            return;
        }

        use std::os::unix::fs::PermissionsExt;

        let root = unique_temp_dir("native-link-color");
        let fake_cargo = root.join("cargo");
        fs::write(
            &fake_cargo,
            b"#!/bin/sh\n\
if [ \"${CARGO_TERM_COLOR:-}\" != never ]; then\n\
  echo \"capture inherited CARGO_TERM_COLOR=${CARGO_TERM_COLOR:-unset}\" >&2\n\
  exit 71\n\
fi\n\
printf 'native-static-libs: -lc\\033[0m -lm\\n' >&2\n",
        )
        .expect("fake Cargo should write");
        fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755))
            .expect("fake Cargo should be executable");

        let output = Command::new(std::env::current_exe().expect("test executable should resolve"))
            .arg("--exact")
            .arg("tests::native_link_arg_capture_overrides_inherited_always_color_and_strips_ansi")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env("CARGO", &fake_cargo)
            .env("CARGO_TERM_COLOR", "always")
            .output()
            .expect("capture regression child should run");
        assert!(
            output.status.success(),
            "capture regression child failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_link_arg_capture_rejects_remaining_control_characters_by_token() {
        for (output, rendered_token) in [
            ("native-static-libs: -lc\0-tainted", "\"-lc\\0-tainted\""),
            ("native-static-libs: -lm\x1b", "\"-lm\\u{1b}\""),
        ] {
            let error = parse_native_static_libs(output)
                .expect_err("non-ANSI control characters must never become linker inputs");
            assert!(error.contains(rendered_token), "{error}");
            assert!(error.contains("control character"), "{error}");
        }
    }

    #[test]
    fn native_link_arg_capture_rejects_malformed_ansi_sequences() {
        for (output, rendered_prefix, rendered_control) in [
            ("native-static-libs: -lc\x1b[\0m", "\"-lc", "\\0"),
            (
                "native-static-libs: -lm\x1b]title\x01\x07",
                "\"-lm",
                "\\u{1}",
            ),
        ] {
            let error = parse_native_static_libs(output)
                .expect_err("malformed ANSI must remain visible to link-arg validation");
            assert!(error.contains(rendered_prefix), "{error}");
            assert!(error.contains(rendered_control), "{error}");
            assert!(error.contains("control character"), "{error}");
        }
    }

    #[test]
    fn native_link_arg_capture_strips_complete_ansi_string_controls() {
        for introducer in ['P', 'X', '^', '_'] {
            let output = format!("native-static-libs: -lc\x1b{introducer}payload\x1b\\ -lm");
            assert_eq!(
                parse_native_static_libs(&output).expect("complete ANSI string should be stripped"),
                vec!["-lc", "-lm"],
                "introducer {introducer:?}"
            );
        }
        for terminator in ["\x07", "\x1b\\"] {
            let output = format!("native-static-libs: -lc\x1b]title{terminator} -lm");
            assert_eq!(
                parse_native_static_libs(&output).expect("complete OSC should be stripped"),
                vec!["-lc", "-lm"]
            );
        }
    }

    #[test]
    fn installed_runtime_artifacts_resolve_relative_to_packaged_executable() {
        let root = unique_temp_dir("installed-runtime");
        let executable = root.join("bin").join("aura");
        let runtime_dir = root.join("lib").join("aura");
        fs::create_dir_all(executable.parent().expect("binary should have a parent"))
            .expect("bin dir should exist");
        fs::create_dir_all(&runtime_dir).expect("runtime dir should exist");
        fs::write(&executable, b"test executable").expect("test executable should write");
        let staticlib = runtime_dir.join("libaura_compiler.a");
        fs::write(&staticlib, b"test runtime").expect("test runtime should write");
        fs::write(
            runtime_dir.join("native-link-args.json"),
            br#"["-framework","Security","-lc"]"#,
        )
        .expect("runtime manifest should write");

        let artifacts = resolve_installed_runtime_artifacts_from_executable(&executable)
            .expect("installed runtime manifest should be valid")
            .expect("installed runtime should resolve");
        assert_eq!(artifacts.staticlib, staticlib);
        assert_eq!(
            artifacts.native_link_args,
            vec!["-framework", "Security", "-lc"]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_backend_parsing_defaults_to_mir_and_accepts_every_selector() {
        assert_eq!(
            parse_run_backend(vec!["main.au".to_string()]),
            (RunBackend::Mir, vec!["main.au".to_string()])
        );
        for (value, expected) in [
            ("mir", RunBackend::Mir),
            ("direct", RunBackend::Direct),
            ("auto", RunBackend::Auto),
        ] {
            assert_eq!(
                parse_run_backend(vec![
                    "--backend".to_string(),
                    value.to_string(),
                    "main.au".to_string()
                ]),
                (expected, vec!["main.au".to_string()]),
                "{value}"
            );
        }
    }

    #[test]
    fn only_auto_run_degrades_to_the_mir_runtime() {
        // A forced direct run reports a build failure instead of silently
        // measuring the MIR runtime.
        assert!(matches!(
            select_run_outcome(
                RunBackend::Direct,
                || Err("no linker".to_string()),
                || panic!("a failed build must not be executed"),
            ),
            NativeRunOutcome::Failed(reason) if reason == "no linker"
        ));
        assert!(matches!(
            select_run_outcome(
                RunBackend::Auto,
                || Err("no linker".to_string()),
                || panic!("a failed build must not be executed"),
            ),
            NativeRunOutcome::FellBack(reason) if reason == "no linker"
        ));

        // A launch failure degrades the same way a build failure does.
        assert!(matches!(
            select_run_outcome(
                RunBackend::Auto,
                || Ok(()),
                || Err("exec failed".to_string()),
            ),
            NativeRunOutcome::FellBack(reason) if reason == "exec failed"
        ));
        assert!(matches!(
            select_run_outcome(RunBackend::Direct, || Ok(()), || Ok(7)),
            NativeRunOutcome::Ran(7)
        ));

        // Once a child has launched, wait and channel failures are execution
        // failures. `auto` must never rerun the Aura program through MIR,
        // whether the child came from a fresh build or a verified cache hit.
        assert!(matches!(
            native_execution_failure(
                RunBackend::Auto,
                NativeExecutionError {
                    message: "malformed channel".to_string(),
                    post_launch: true,
                },
            ),
            NativeRunOutcome::Failed(reason) if reason == "malformed channel"
        ));
        assert!(matches!(
            verified_native_execution_failure(
                RunBackend::Auto,
                VerifiedNativeLaunchError::after_launch("missing record".to_string()),
            ),
            NativeRunOutcome::Failed(reason) if reason == "missing record"
        ));
    }

    #[test]
    fn auto_backend_reports_direct_failure_when_falling_back() {
        let outcome = select_build_backend(
            BuildBackend::Auto,
            || Err("direct failed".to_string()),
            || Ok(()),
        )
        .expect("MIR fallback should succeed");
        assert_eq!(outcome.selected, SelectedBuildBackend::MirRuntime);
        assert_eq!(outcome.fallback_reason.as_deref(), Some("direct failed"));

        let error = select_build_backend(
            BuildBackend::Auto,
            || Err("direct failed".to_string()),
            || Err("MIR failed".to_string()),
        )
        .expect_err("both backend failures should be preserved");
        assert!(error.contains("direct failed"));
        assert!(error.contains("MIR failed"));
    }

    #[test]
    fn forced_direct_backend_never_invokes_fallback() {
        let outcome = select_build_backend(
            BuildBackend::Direct,
            || Ok(()),
            || panic!("forced direct mode must not invoke MIR fallback"),
        )
        .expect("direct backend should succeed");
        assert_eq!(outcome.selected, SelectedBuildBackend::Direct);
        assert!(outcome.fallback_reason.is_none());
    }

    #[test]
    fn resolve_static_library_path_uses_single_hashed_archive_when_primary_missing() {
        let root = unique_temp_dir("single-hashed");
        let deps = root.join("target").join("debug").join("deps");
        fs::create_dir_all(&deps).expect("deps dir should exist");
        let archive = deps.join("libaura_compiler-only.a");
        fs::write(&archive, b"archive").expect("hashed archive should write");

        let resolved = resolve_static_library_path(root.clone(), "debug")
            .expect("should resolve the only hashed runtime library");
        assert_eq!(resolved, archive);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_static_library_path_rejects_ambiguous_hashed_archives() {
        let root = unique_temp_dir("ambiguous-hashed");
        let deps = root.join("target").join("debug").join("deps");
        fs::create_dir_all(&deps).expect("deps dir should exist");
        let first = deps.join("libaura_compiler-first.a");
        fs::write(&first, b"first").expect("first archive should write");
        thread::sleep(Duration::from_millis(10));
        let second = deps.join("libaura_compiler-second.a");
        fs::write(&second, b"second").expect("second archive should write");

        let error = resolve_static_library_path(root.clone(), "debug")
            .expect_err("ambiguous hashed archives should be rejected");
        assert!(
            error.contains("multiple hashed Aura runtime archives"),
            "unexpected error message: {}",
            error
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parse_static_library_artifact_path_prefers_cargo_reported_archive() {
        let stdout = br#"{"reason":"compiler-artifact","target":{"name":"aura_compiler"},"filenames":["/tmp/libaura_compiler-abc123.rlib","/tmp/libaura_compiler-abc123.a"]}
{"reason":"compiler-artifact","target":{"name":"other"},"filenames":["/tmp/libother.a"]}"#;
        let resolved = parse_static_library_artifact_path(stdout)
            .expect("cargo artifact output should expose a static archive");
        assert_eq!(resolved, PathBuf::from("/tmp/libaura_compiler-abc123.a"));
    }

    #[test]
    fn write_unique_temp_file_rejects_existing_paths() {
        let root = unique_temp_dir("unique-temp-file");
        let path = root.join("launcher.c");

        write_unique_temp_file(&path, b"first", "test temp file")
            .expect("first write should create the temp file");
        let error = write_unique_temp_file(&path, b"second", "test temp file")
            .expect_err("existing temp paths should be rejected");
        assert!(error.contains("failed to create"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_unique_temp_file_removes_partial_file_when_write_fails() {
        let root = unique_temp_dir("unique-temp-file-cleanup");
        let path = root.join("launcher.c");

        let error = write_unique_temp_file_with_writer(&path, "test temp file", |file| {
            use std::io::Write;

            file.write_all(b"partial")?;
            Err(io::Error::other("simulated write failure"))
        })
        .expect_err("partial temp files should be cleaned up after write failures");
        assert!(error.contains("failed to write"));
        assert!(
            !path.exists(),
            "failed unique-temp writes should not leave a stale partial file behind"
        );

        let _ = fs::remove_dir_all(root);
    }
}
