pub mod analysis;
pub mod ast;
mod builtin_modules;
pub(crate) mod bytes_codec;
pub mod call;
pub mod diag;
pub mod ffi;
pub mod integer;
pub(crate) mod json_codec;
pub mod lexer;
pub mod limits;
pub mod mir;
pub mod mir_runtime;
mod native_codegen;
mod native_runtime;
mod package;
pub mod parser;
mod randomness;
pub(crate) mod runtime_config;
mod runtime_reactor;
pub mod runtime_value;
pub mod sema;

use std::fs;
use std::path::{Path, PathBuf};
use std::{collections::BTreeMap, collections::BTreeSet, collections::HashMap};

pub use analysis::{
    analyze_path_source, analyze_program, analyze_source, complete_path_source, complete_source,
    AnalysisCompletion, AnalysisOutput,
};
pub use diag::{
    AssertionOperand, Diagnostic, Result, RuntimeCallFrame, RuntimeSourceSpan, RuntimeTaskFrame,
    Span, StructuredDiagnostic, StructuredEdit, StructuredRuntimeCallFrame,
    StructuredRuntimeSourceSpan, StructuredRuntimeTaskFrame, StructuredSpan,
};
pub use mir::{lower as lower_to_mir, MirModule};
pub use mir_runtime::{
    run as run_mir, run_entry_with_stdout_sink_and_program_args as run_mir_entry,
    run_serialized_mir, run_with_stdout_sink as run_mir_with_stdout_sink,
    run_with_stdout_sink_and_program_args as run_mir_with_stdout_sink_and_program_args, StdoutSink,
};
pub use native_codegen::{
    emit_host_object as emit_host_native_object,
    emit_host_object_with_metadata as emit_host_native_object_with_metadata,
};
pub use runtime_value::{RunOutput, Value};

/// MIR whose imports, package graph, and FFI permissions were checked through
/// the manifest-rooted path loader.
///
/// The inner module is deliberately private so caller-supplied MIR cannot be
/// promoted into the trusted runtime route.
pub struct CheckedMirModule(MirModule);

#[cfg(test)]
fn timing_limit_for_hosted_ci(
    local_limit: std::time::Duration,
    hosted_ci: bool,
) -> std::time::Duration {
    if hosted_ci {
        local_limit.saturating_mul(4)
    } else {
        local_limit
    }
}

#[cfg(test)]
pub(crate) fn hosted_ci_timing_limit(local_limit: std::time::Duration) -> std::time::Duration {
    timing_limit_for_hosted_ci(local_limit, std::env::var_os("GITHUB_ACTIONS").is_some())
}

#[doc(hidden)]
pub const INTERNAL_DIAGNOSTIC_FD_ENV: &str = "AURA_INTERNAL_DIAGNOSTIC_FD";
#[doc(hidden)]
pub const INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV: &str = "AURA_INTERNAL_DIAGNOSTIC_SIGNAL_FD";
#[doc(hidden)]
pub const INTERNAL_DIAGNOSTIC_SIGNAL_MARKER: u8 = 0x01;
#[doc(hidden)]
pub const MAX_INTERNAL_DIAGNOSTIC_BYTES: usize = 1024 * 1024;

/// Version of the compiler's exported semantic interface.
///
/// Every persisted artifact or long-lived tooling cache that can contain
/// compiler semantic metadata must bind this value. Bump it whenever the
/// meaning or representation of checked source changes incompatibly.
pub const SEMANTIC_INTERFACE_SCHEMA_VERSION: u32 = 6;

/// Lowercase hexadecimal SHA-256 of `bytes`, for content-addressed identities.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = bytes_codec::sha256_bytes(bytes).expect("SHA-256 output always fits its buffer");
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}
pub use sema::{ImportedBinding, ModuleContext, ModuleNamespace, Program};

use ast::{ImportKind, Item};
pub use package::DependencyUpdateResult;
use package::PackageGraph;

#[cfg(coverage)]
#[doc(hidden)]
pub mod native_runtime_coverage {
    pub use super::native_runtime::aura_direct_tag_value_type;
    pub use super::native_runtime::DIRECT_VALUE_LIVE_COUNT;
    pub use super::native_runtime::{
        aura_direct_arg_buffer_new, aura_direct_arg_buffer_store_owned, aura_direct_array_binary,
        aura_direct_array_clone, aura_direct_array_fill_in_place, aura_direct_array_from_vec,
        aura_direct_array_full, aura_direct_array_get, aura_direct_array_index,
        aura_direct_array_len, aura_direct_array_map, aura_direct_array_reduce,
        aura_direct_array_set_in_place, aura_direct_array_set_index_in_place,
        aura_direct_array_shape, aura_direct_array_slice, aura_direct_array_zeros,
        aura_direct_binary_value, aura_direct_binary_value_at, aura_direct_box_bool,
        aura_direct_box_f64, aura_direct_box_i32, aura_direct_box_i64, aura_direct_box_u64,
        aura_direct_cast_float_to_integer, aura_direct_cast_integer_to_float,
        aura_direct_cast_integer_to_integer, aura_direct_cast_value, aura_direct_channel_new,
        aura_direct_channel_recv, aura_direct_channel_send_timeout_value,
        aura_direct_channel_try_send, aura_direct_close_value, aura_direct_coverage_clone_value,
        aura_direct_duration_from_i64, aura_direct_duration_literal, aura_direct_duration_to_float,
        aura_direct_enum_variant, aura_direct_file_close, aura_direct_file_flush,
        aura_direct_file_read_all, aura_direct_file_write_all, aura_direct_fs_append_string,
        aura_direct_fs_create, aura_direct_fs_create_dir, aura_direct_fs_open,
        aura_direct_fs_read_dir, aura_direct_function_default_binder, aura_direct_function_thunk,
        aura_direct_function_value, aura_direct_host_builtin, aura_direct_http_listener_accept,
        aura_direct_http_listener_close, aura_direct_http_listener_local_addr,
        aura_direct_http_response_bytes, aura_direct_http_response_headers,
        aura_direct_http_response_reason, aura_direct_http_response_status,
        aura_direct_http_response_text, aura_direct_instance_get_field, aura_direct_instance_new,
        aura_direct_integer_to_float, aura_direct_integer_width_binary, aura_direct_io_flush,
        aura_direct_io_write, aura_direct_map_clear_in_place, aura_direct_map_contains_key,
        aura_direct_map_empty, aura_direct_map_extend_in_place, aura_direct_map_get,
        aura_direct_map_index, aura_direct_map_is_empty, aura_direct_map_items,
        aura_direct_map_keys, aura_direct_map_len, aura_direct_map_remove_in_place,
        aura_direct_map_set_in_place, aura_direct_map_set_index_in_place, aura_direct_map_values,
        aura_direct_monotonic_time_ms, aura_direct_net_connect, aura_direct_net_http_listen,
        aura_direct_net_http_request_bytes_timeout, aura_direct_net_listen,
        aura_direct_net_udp_bind, aura_direct_net_unix_connect, aura_direct_net_unix_listen,
        aura_direct_net_websocket_connect, aura_direct_net_websocket_listen,
        aura_direct_process_child_close, aura_direct_process_child_stderr,
        aura_direct_process_child_stdin, aura_direct_process_child_stdout,
        aura_direct_process_child_wait, aura_direct_process_child_wait_ok,
        aura_direct_process_child_wait_or_none, aura_direct_process_completed_check,
        aura_direct_process_completed_status, aura_direct_process_completed_stderr,
        aura_direct_process_completed_stderr_bytes, aura_direct_process_completed_stdout,
        aura_direct_process_completed_stdout_bytes, aura_direct_process_completed_success,
        aura_direct_process_null, aura_direct_process_pipe, aura_direct_process_pipe_close,
        aura_direct_process_pipe_flush, aura_direct_process_pipe_read_all,
        aura_direct_process_pipe_read_bytes, aura_direct_process_pipe_write_all,
        aura_direct_process_pipe_write_bytes, aura_direct_process_run, aura_direct_process_start,
        aura_direct_random_secure_bytes, aura_direct_random_secure_int, aura_direct_release_value,
        aura_direct_rng_new, aura_direct_rng_next_float, aura_direct_rng_next_int,
        aura_direct_rng_shuffle, aura_direct_select, aura_direct_set_contains,
        aura_direct_set_empty, aura_direct_set_index_option, aura_direct_set_insert_in_place,
        aura_direct_set_is_empty, aura_direct_set_len, aura_direct_set_remove_in_place,
        aura_direct_sleep_ms, aura_direct_sleep_value, aura_direct_sleep_value_void,
        aura_direct_string_byte_len, aura_direct_string_len, aura_direct_string_literal,
        aura_direct_string_slice, aura_direct_tcp_listener_accept, aura_direct_tcp_listener_close,
        aura_direct_tcp_listener_local_addr, aura_direct_tcp_stream_close,
        aura_direct_tcp_stream_flush, aura_direct_tcp_stream_local_addr,
        aura_direct_tcp_stream_peer_addr, aura_direct_tcp_stream_read_all,
        aura_direct_tcp_stream_read_exact, aura_direct_tcp_stream_shutdown_read,
        aura_direct_tcp_stream_shutdown_write, aura_direct_tcp_stream_write_all,
        aura_direct_tcp_stream_write_bytes, aura_direct_tuple_element, aura_direct_tuple_new,
        aura_direct_tuple_take_element, aura_direct_udp_datagram_address,
        aura_direct_udp_datagram_bytes, aura_direct_udp_datagram_text,
        aura_direct_udp_socket_close, aura_direct_udp_socket_local_addr,
        aura_direct_udp_socket_recv, aura_direct_udp_socket_recv_from,
        aura_direct_udp_socket_send_bytes, aura_direct_unary_value, aura_direct_unary_value_at,
        aura_direct_unbox_bool, aura_direct_unbox_f64, aura_direct_unbox_i64,
        aura_direct_unbox_int64, aura_direct_unbox_u64, aura_direct_unix_listener_accept,
        aura_direct_unix_listener_close, aura_direct_unix_stream_close,
        aura_direct_unix_stream_read_exact, aura_direct_unix_stream_write_all,
        aura_direct_value_as_condition, aura_direct_variant_payload,
        aura_direct_vec_clear_in_place, aura_direct_vec_contains, aura_direct_vec_empty,
        aura_direct_vec_extend_in_place, aura_direct_vec_get, aura_direct_vec_index,
        aura_direct_vec_index_option, aura_direct_vec_insert_in_place, aura_direct_vec_is_empty,
        aura_direct_vec_len, aura_direct_vec_pop_in_place, aura_direct_vec_push_in_place,
        aura_direct_vec_remove_in_place, aura_direct_vec_reverse_in_place,
        aura_direct_vec_set_in_place, aura_direct_vec_set_index_in_place, aura_direct_vec_slice,
        aura_direct_vec_swap_in_place, aura_direct_vec_take_index_in_place, aura_direct_wait_all,
        aura_direct_wait_all_timeout_value, aura_direct_wait_any,
        aura_direct_wait_any_timeout_value, aura_direct_websocket_close,
        aura_direct_websocket_listener_accept, aura_direct_websocket_listener_local_addr,
        aura_direct_websocket_recv_bytes, aura_direct_websocket_recv_text,
        aura_direct_websocket_send_bytes, aura_direct_websocket_send_text, aura_direct_yield_now,
        OpaqueValue,
    };
}

pub fn parse_source(source: &str) -> Result<ast::Module> {
    parser::parse(source)
}

pub fn check_source(source: &str) -> Result<Program> {
    let module = parse_source(source)?;
    reject_source_only_ffi(&module)?;
    check_module_with_builtin_imports(module)
}

fn reject_source_only_ffi(module: &ast::Module) -> Result<()> {
    if module
        .items
        .iter()
        .any(|item| matches!(item, Item::ExternFunction(_) | Item::ExternOpaqueClass(_)))
    {
        return Err(Diagnostic::coded(
            "AU2999",
            "FFI declarations require a manifest-rooted Aura package; add `Aura.toml` with `[package] allow_ffi = true` and use a path-based compiler API",
        ));
    }
    Ok(())
}

pub fn run_source(source: &str) -> Result<RunOutput> {
    let program = check_source(source)?;
    let mir = lower_to_mir(&program);
    run_mir(&mir)
}

pub fn run_source_with_stdout_sink(source: &str, stdout_sink: StdoutSink) -> Result<RunOutput> {
    let program = check_source(source)?;
    let mir = lower_to_mir(&program);
    run_mir_with_stdout_sink(&mir, Some(stdout_sink))
}

pub fn run_path_with_source(path: &Path, source: &str) -> Result<RunOutput> {
    let program = check_path_with_source(path, source)?;
    let mir = lower_to_mir(&program);
    mir_runtime::run_trusted(&mir)
}

pub fn run_path_with_source_and_stdout_sink(
    path: &Path,
    source: &str,
    stdout_sink: StdoutSink,
) -> Result<RunOutput> {
    let program = check_path_with_source(path, source)?;
    let mir = lower_to_mir(&program);
    mir_runtime::run_with_stdout_sink_trusted(&mir, Some(stdout_sink))
}

pub fn run_path_with_source_and_stdout_sink_and_program_args(
    path: &Path,
    source: &str,
    stdout_sink: StdoutSink,
    program_args: Vec<String>,
) -> Result<RunOutput> {
    let program = check_path_with_source(path, source)?;
    let mir = lower_to_mir(&program);
    mir_runtime::run_with_stdout_sink_and_program_args_trusted(
        &mir,
        Some(stdout_sink),
        program_args,
    )
}

pub fn lower_source_to_mir(source: &str) -> Result<MirModule> {
    let program = check_source(source)?;
    Ok(lower_to_mir(&program))
}

fn builtin_imports(module: &ast::Module) -> Result<BTreeMap<String, ImportedBinding>> {
    let mut bindings = BTreeMap::new();
    for import in &module.imports {
        match &import.kind {
            ImportKind::Module { path, alias } => {
                if let Some(namespace) = builtin_modules::builtin_module_namespace(path) {
                    if let Some(alias) = alias {
                        insert_aliased_namespace_import(
                            &mut bindings,
                            alias,
                            namespace,
                            import.span,
                        )?;
                    } else {
                        insert_namespace_import(&mut bindings, path, namespace, import.span)?;
                    }
                }
            }
            ImportKind::From { module_path, names } => {
                if builtin_modules::builtin_module_namespace(module_path).is_some() {
                    for imported_name in names {
                        let binding = builtin_modules::builtin_imported_binding(
                            module_path,
                            &imported_name.name,
                            import.span,
                        )?;
                        let local_name =
                            imported_name.alias.as_ref().unwrap_or(&imported_name.name);
                        if bindings.insert(local_name.clone(), binding).is_some() {
                            return Err(Diagnostic::at(
                                imported_name.span,
                                format!("duplicate import binding `{}`", local_name),
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(bindings)
}

fn builtin_module_registry_with_user(
    user_modules: impl IntoIterator<Item = (String, ModuleNamespace)>,
) -> BTreeMap<String, ModuleNamespace> {
    let mut registry = builtin_modules::builtin_module_registry();
    registry.extend(user_modules);
    registry
}

fn check_module_with_builtin_imports(module: ast::Module) -> Result<Program> {
    let imported_bindings = builtin_imports(&module)?;
    let module_registry = builtin_module_registry_with_user(BTreeMap::new());
    sema::check_with_context(
        module,
        ModuleContext {
            module_name: "<main>".to_string(),
            imported_bindings,
            module_registry,
            is_entry_module: true,
        },
    )
}

pub fn lower_path_with_source_to_mir(path: &Path, source: &str) -> Result<MirModule> {
    let program = check_path_with_source(path, source)?;
    Ok(lower_to_mir(&program))
}

pub fn check_path(path: &Path) -> Result<Program> {
    let mut loader = ModuleLoader::new(path)?;
    let program = loader.load_program(path)?;
    loader.write_lockfile()?;
    Ok(program)
}

pub fn check_path_with_source(path: &Path, source: &str) -> Result<Program> {
    check_path_with_source_inner(path, source, true)
}

fn check_path_with_source_without_lockfile(path: &Path, source: &str) -> Result<Program> {
    check_path_with_source_inner(path, source, false)
}

fn check_path_with_source_inner(
    path: &Path,
    source: &str,
    write_lockfile: bool,
) -> Result<Program> {
    let mut loader = ModuleLoader::new_with_source(path, Some(source))?;
    let program = loader.load_program_with_source(path, source)?;
    if write_lockfile {
        loader.write_lockfile()?;
    }
    Ok(program)
}

pub fn run_path(path: &Path) -> Result<RunOutput> {
    let program = check_path(path)?;
    let mir = lower_to_mir(&program);
    mir_runtime::run_trusted(&mir)
}

pub fn run_path_with_stdout_sink(path: &Path, stdout_sink: StdoutSink) -> Result<RunOutput> {
    let program = check_path(path)?;
    let mir = lower_to_mir(&program);
    mir_runtime::run_with_stdout_sink_trusted(&mir, Some(stdout_sink))
}

pub fn run_path_with_stdout_sink_and_program_args(
    path: &Path,
    stdout_sink: StdoutSink,
    program_args: Vec<String>,
) -> Result<RunOutput> {
    run_path_entry_with_stdout_sink_and_program_args(path, None, Some(stdout_sink), program_args)
}

/// Runs one parameterless top-level entry from a manifest-checked path.
///
/// Unlike the arbitrary-MIR execution APIs, this path-based API may execute
/// FFI because package loading has already enforced the root opt-in and
/// dependency-report contract before MIR is lowered.
pub fn run_path_entry_with_stdout_sink_and_program_args(
    path: &Path,
    entry: Option<&str>,
    stdout_sink: Option<StdoutSink>,
    program_args: Vec<String>,
) -> Result<RunOutput> {
    let program = check_path(path)?;
    let mir = lower_to_mir(&program);
    mir_runtime::run_entry_with_stdout_sink_and_program_args_trusted(
        &mir,
        entry,
        stdout_sink,
        program_args,
    )
}

/// Runs one parameterless entry from a previously path-checked MIR module.
///
/// This is the reusable form of [`run_path_entry_with_stdout_sink_and_program_args`]:
/// package loading and semantic checking happen once, while multiple entries
/// may execute through the same trusted module.
pub fn run_checked_mir_entry_with_stdout_sink_and_program_args(
    module: &CheckedMirModule,
    entry: Option<&str>,
    stdout_sink: Option<StdoutSink>,
    program_args: Vec<String>,
) -> Result<RunOutput> {
    mir_runtime::run_entry_with_stdout_sink_and_program_args_trusted(
        &module.0,
        entry,
        stdout_sink,
        program_args,
    )
}

/// Loads, manifest-checks, semantically checks, and lowers a path into opaque
/// MIR that is eligible for repeated trusted entry execution.
pub fn lower_path_to_checked_mir(path: &Path) -> Result<CheckedMirModule> {
    let program = check_path(path)?;
    Ok(CheckedMirModule(lower_to_mir(&program)))
}

pub fn lower_path_to_mir(path: &Path) -> Result<MirModule> {
    let program = check_path(path)?;
    Ok(lower_to_mir(&program))
}

pub fn update_git_dependencies_in_working_dir(
    path: &Path,
    target_package: Option<&str>,
) -> Result<DependencyUpdateResult> {
    package::update_git_dependencies_in_working_dir(path, target_package)
}

#[derive(Clone)]
struct LoadedModule {
    program: Program,
}

struct ModuleLoader {
    package_root: PathBuf,
    package_graph: Option<PackageGraph>,
    cache: HashMap<PathBuf, LoadedModule>,
    stack: Vec<PathBuf>,
}

impl ModuleLoader {
    fn new(entry_path: &Path) -> Result<Self> {
        Self::new_with_source(entry_path, None)
    }

    fn new_with_source(entry_path: &Path, source_override: Option<&str>) -> Result<Self> {
        let absolute_entry = absolutize(entry_path);
        let package_graph = PackageGraph::discover_for_entry(&absolute_entry)?;
        let package_root = if let Some(graph) = &package_graph {
            graph.root_source_root.clone()
        } else {
            infer_package_root(&absolute_entry, source_override)?
        };
        Ok(Self {
            package_root,
            package_graph,
            cache: HashMap::new(),
            stack: Vec::new(),
        })
    }

    fn load_program(&mut self, path: &Path) -> Result<Program> {
        self.load_program_internal(path, None)
    }

    fn load_program_with_source(&mut self, path: &Path, source: &str) -> Result<Program> {
        self.load_program_internal(path, Some(source))
    }

    fn load_program_internal(
        &mut self,
        path: &Path,
        source_override: Option<&str>,
    ) -> Result<Program> {
        let path = absolutize(path);
        if let Some(loaded) = self.cache.get(&path) {
            return Ok(loaded.program.clone());
        }
        if self.stack.contains(&path) {
            return Err(Diagnostic::new(format!(
                "cyclic import involving `{}`",
                path.display()
            )));
        }

        self.stack.push(path.clone());
        let is_entry_module = self.stack.len() == 1;

        let source = if let Some(source) = source_override {
            source.to_string()
        } else {
            fs::read_to_string(&path).map_err(|error| {
                Diagnostic::new(format!("failed to read `{}`: {}", path.display(), error))
            })?
        };
        let display_path = path.display().to_string();
        let module = parse_source(&source)
            .map_err(|error| error.with_render_context(display_path.clone(), source.clone()))?;
        if module
            .items
            .iter()
            .any(|item| matches!(item, Item::ExternFunction(_) | Item::ExternOpaqueClass(_)))
        {
            match &self.package_graph {
                Some(graph) => graph.ensure_ffi_allowed_for_path(&path)?,
                None => {
                    return Err(Diagnostic::coded(
                        "AU2999",
                        format!(
                            "FFI declarations in `{}` require an Aura package manifest; add `Aura.toml` with `[package] allow_ffi = true`",
                            path.display()
                        ),
                    ));
                }
            }
        }
        let module_name = self.module_name_for_path(&path);
        let imported_bindings = self.resolve_imports(&module, &path)?;
        let module_registry = self.build_module_registry();
        let program = sema::check_with_context(
            module,
            ModuleContext {
                module_name,
                imported_bindings,
                module_registry,
                is_entry_module,
            },
        )
        .map_err(|error| error.with_render_context(display_path, source.clone()))?;
        let mut program = program;
        self.qualify_program_imported_modules(&path, &mut program);
        program.source_path = Some(path.display().to_string());

        self.cache.insert(
            path.clone(),
            LoadedModule {
                program: program.clone(),
            },
        );
        if is_entry_module {
            program.constant_init_plan = self.build_constant_init_plan(&path)?;
            self.cache.insert(
                path.clone(),
                LoadedModule {
                    program: program.clone(),
                },
            );
        }
        self.stack.pop();
        Ok(program)
    }

    fn build_constant_init_plan(&self, entry_path: &Path) -> Result<Vec<sema::ConstantInfo>> {
        fn visit(
            loader: &ModuleLoader,
            path: &Path,
            visited: &mut BTreeSet<PathBuf>,
            plan: &mut Vec<sema::ConstantInfo>,
        ) -> Result<()> {
            let path = absolutize(path);
            if !visited.insert(path.clone()) {
                return Ok(());
            }
            let loaded = loader.cache.get(&path).ok_or_else(|| {
                Diagnostic::new(format!(
                    "module constant initialization plan is missing loaded module `{}`",
                    path.display()
                ))
            })?;
            for import in &loaded.program.module.imports {
                let (module_path, selected_names) = match &import.kind {
                    ImportKind::From { module_path, names } => (module_path, Some(names)),
                    ImportKind::Module { path, .. } => (path, None),
                };
                if let Some(namespace) = builtin_modules::builtin_module_namespace(module_path) {
                    if let Some(names) = selected_names {
                        plan.extend(names.iter().filter_map(|imported| {
                            namespace.constants.get(&imported.name).cloned()
                        }));
                    } else {
                        plan.extend(namespace.all_constants.into_values());
                    }
                    continue;
                }
                let dependency = loader.resolve_import_path(&path, module_path)?;
                visit(loader, &dependency, visited, plan)?;
            }
            let mut local = loaded
                .program
                .constants
                .values()
                .filter(|constant| constant.module_name == loaded.program.module_name)
                .cloned()
                .collect::<Vec<_>>();
            local.sort_by_key(|constant| (constant.decl.span.line, constant.decl.span.column));
            plan.extend(local);
            Ok(())
        }

        let mut visited = BTreeSet::new();
        let mut plan = Vec::new();
        visit(self, entry_path, &mut visited, &mut plan)?;
        let mut seen = BTreeSet::new();
        plan.retain(|constant| {
            seen.insert((constant.module_name.clone(), constant.decl.name.clone()))
        });
        Ok(plan)
    }

    fn resolve_imports(
        &mut self,
        module: &ast::Module,
        current_path: &Path,
    ) -> Result<BTreeMap<String, ImportedBinding>> {
        let mut bindings = BTreeMap::new();
        for import in &module.imports {
            match &import.kind {
                ImportKind::From { module_path, names } => {
                    if builtin_modules::builtin_module_namespace(module_path).is_some() {
                        for imported_name in names {
                            let binding = builtin_modules::builtin_imported_binding(
                                module_path,
                                &imported_name.name,
                                import.span,
                            )?;
                            let local_name =
                                imported_name.alias.as_ref().unwrap_or(&imported_name.name);
                            if bindings.insert(local_name.clone(), binding).is_some() {
                                return Err(Diagnostic::at(
                                    imported_name.span,
                                    format!("duplicate import binding `{}`", local_name),
                                ));
                            }
                        }
                        continue;
                    }
                    let imported =
                        self.load_imported_module(current_path, module_path, import.span)?;
                    for imported_name in names {
                        let binding =
                            exported_binding(&imported, &imported_name.name).ok_or_else(|| {
                                let logical_name = module_path.join(".");
                                if local_item_exists(&imported, &imported_name.name) {
                                    Diagnostic::at(
                                        imported_name.span,
                                        format!(
                                            "item `{}` is private in module `{}`",
                                            imported_name.name, logical_name
                                        ),
                                    )
                                } else {
                                    Diagnostic::at(
                                        imported_name.span,
                                        format!(
                                            "module `{}` has no export named `{}`",
                                            logical_name, imported_name.name
                                        ),
                                    )
                                }
                            })?;
                        let local_name =
                            imported_name.alias.as_ref().unwrap_or(&imported_name.name);
                        if bindings.insert(local_name.clone(), binding).is_some() {
                            return Err(Diagnostic::at(
                                imported_name.span,
                                format!("duplicate import binding `{}`", local_name),
                            ));
                        }
                    }
                }
                ImportKind::Module { path, alias } => {
                    if let Some(leaf) = builtin_modules::builtin_module_namespace(path) {
                        if let Some(alias) = alias {
                            insert_aliased_namespace_import(
                                &mut bindings,
                                alias,
                                leaf,
                                import.span,
                            )?;
                        } else {
                            insert_namespace_import(&mut bindings, path, leaf, import.span)?;
                        }
                        continue;
                    }
                    let imported = self.load_imported_module(current_path, path, import.span)?;
                    let leaf = exported_namespace(path, &imported);
                    if let Some(alias) = alias {
                        insert_aliased_namespace_import(&mut bindings, alias, leaf, import.span)?;
                    } else {
                        insert_namespace_import(&mut bindings, path, leaf, import.span)?;
                    }
                }
            }
        }
        Ok(bindings)
    }

    fn build_module_registry(&self) -> BTreeMap<String, ModuleNamespace> {
        builtin_module_registry_with_user(self.cache.values().map(|loaded| {
            let path = loaded
                .program
                .module_name
                .split('.')
                .map(|segment| segment.to_string())
                .collect::<Vec<_>>();
            (
                loaded.program.module_name.clone(),
                exported_namespace(&path, &loaded.program),
            )
        }))
    }

    fn load_imported_module(
        &mut self,
        current_path: &Path,
        module_path: &[String],
        span: Span,
    ) -> Result<Program> {
        let path = self.resolve_import_path(current_path, module_path)?;
        if !path.exists() {
            return Err(Diagnostic::at(
                span,
                format!(
                    "cannot resolve module `{}` at `{}`",
                    module_path.join("."),
                    path.display()
                ),
            ));
        }
        self.load_program(&path)
    }

    fn resolve_import_path(&self, current_path: &Path, module_path: &[String]) -> Result<PathBuf> {
        if let Some(graph) = &self.package_graph {
            return graph.resolve_import_path(current_path, module_path);
        }
        checked_module_path(&self.package_root, module_path)
    }

    fn module_name_for_path(&self, path: &Path) -> String {
        self.package_graph
            .as_ref()
            .and_then(|graph| graph.module_name_for_path(path))
            .unwrap_or_else(|| logical_module_name(&self.package_root, path))
    }

    fn qualify_program_imported_modules(&self, path: &Path, program: &mut Program) {
        let Some(graph) = &self.package_graph else {
            return;
        };
        let Some(package) = graph.source_for_path(path) else {
            return;
        };
        let Some(prefix) = package.external_prefix.as_deref() else {
            return;
        };
        let dependency_aliases = graph.dependency_aliases_for_path(path);
        let dependency_bindings = program
            .module
            .imports
            .iter()
            .filter_map(|import| {
                let ImportKind::Module {
                    path: imported_path,
                    alias,
                } = &import.kind
                else {
                    return None;
                };
                let root = imported_path.first()?;
                dependency_aliases
                    .contains(root)
                    .then(|| alias.as_ref().unwrap_or(root).clone())
            })
            .collect::<BTreeSet<_>>();
        qualify_imported_module_namespaces(
            &mut program.imported_modules,
            prefix,
            &dependency_bindings,
        );
    }

    fn write_lockfile(&self) -> Result<()> {
        if let Some(graph) = &self.package_graph {
            graph.write_lockfile()?;
        }
        Ok(())
    }
}

fn absolutize(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(current) => current.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    if let Ok(canonical) = fs::canonicalize(&absolute) {
        return canonical;
    }

    let mut existing_ancestor = absolute.as_path();
    while !existing_ancestor.exists() {
        let Some(parent) = existing_ancestor.parent() else {
            return absolute;
        };
        existing_ancestor = parent;
    }

    let Ok(canonical_ancestor) = fs::canonicalize(existing_ancestor) else {
        return absolute;
    };
    let Ok(suffix) = absolute.strip_prefix(existing_ancestor) else {
        return absolute;
    };
    if suffix.as_os_str().is_empty() {
        canonical_ancestor
    } else {
        canonical_ancestor.join(suffix)
    }
}

fn infer_package_root(entry_path: &Path, source_override: Option<&str>) -> Result<PathBuf> {
    let entry_dir = entry_path.parent().unwrap_or(Path::new("."));

    let parsed_entry = source_override
        .map(str::to_string)
        .or_else(|| fs::read_to_string(entry_path).ok())
        .and_then(|source| parse_source(&source).ok());

    if let Some(module) = parsed_entry {
        let import_paths = module
            .imports
            .iter()
            .map(|import| match &import.kind {
                ImportKind::From { module_path, .. } => module_path.clone(),
                ImportKind::Module { path, .. } => path.clone(),
            })
            .collect::<Vec<_>>();

        if !import_paths.is_empty() {
            for candidate in entry_dir.ancestors() {
                if import_paths
                    .iter()
                    .all(|import_path| import_exists_from_root(candidate, import_path))
                {
                    return canonicalize_if_exists(candidate);
                }
            }
        }
    }

    canonicalize_if_exists(entry_dir)
}

fn import_exists_from_root(root: &Path, module_path: &[String]) -> bool {
    checked_module_path(root, module_path)
        .map(|path| path.exists())
        .unwrap_or(false)
}

fn logical_module_name(package_root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(package_root).unwrap_or(path);
    let mut without_extension = relative.to_path_buf();
    without_extension.set_extension("");
    without_extension
        .iter()
        .map(|segment| segment.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(".")
}

fn checked_module_path(package_root: &Path, module_path: &[String]) -> Result<PathBuf> {
    let canonical_root = canonicalize_if_exists(package_root)?;
    let mut path = package_root.to_path_buf();
    for segment in module_path {
        path.push(segment);
    }
    path.set_extension("au");
    let canonical = canonicalize_if_exists(&path)?;
    if !canonical.starts_with(&canonical_root) {
        return Err(Diagnostic::new(format!(
            "resolved import path `{}` escapes package source root `{}`",
            canonical.display(),
            canonical_root.display()
        )));
    }
    Ok(canonical)
}

fn canonicalize_if_exists(path: &Path) -> Result<PathBuf> {
    if let Ok(canonical) = fs::canonicalize(path) {
        return Ok(canonical);
    }

    let mut existing_ancestor = path;
    while !existing_ancestor.exists() {
        let Some(parent) = existing_ancestor.parent() else {
            return Ok(path.to_path_buf());
        };
        existing_ancestor = parent;
    }

    let canonical_ancestor = match fs::canonicalize(existing_ancestor) {
        Ok(canonical) => canonical,
        Err(error) => {
            return Err(Diagnostic::new(format!(
                "failed to resolve path `{}`: {}",
                existing_ancestor.display(),
                error
            )));
        }
    };
    let Ok(suffix) = path.strip_prefix(existing_ancestor) else {
        return Ok(path.to_path_buf());
    };
    Ok(if suffix.as_os_str().is_empty() {
        canonical_ancestor
    } else {
        canonical_ancestor.join(suffix)
    })
}

fn qualify_imported_module_namespaces(
    modules: &mut BTreeMap<String, ModuleNamespace>,
    prefix: &str,
    dependency_aliases: &BTreeSet<String>,
) {
    for (name, namespace) in modules.iter_mut() {
        if dependency_aliases.contains(name) {
            continue;
        }
        qualify_namespace_path(namespace, prefix);
    }
}

fn qualify_namespace_path(namespace: &mut ModuleNamespace, prefix: &str) {
    namespace.path = format!("{}.{}", prefix, namespace.path);
    for module in namespace.modules.values_mut() {
        qualify_namespace_path(module, prefix);
    }
}

fn local_item_exists(program: &Program, name: &str) -> bool {
    program.module.items.iter().any(|item| item.name() == name)
        || program
            .module
            .constants
            .iter()
            .any(|item| item.name == name)
}

fn is_builtin_export_type(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "int"
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
            | "str"
            | "Range"
            | "Option"
            | "Result"
            | "Task"
            | "SendError"
            | "TaskGroup"
            | "Duration"
    )
}

fn find_type_namespace_path(
    modules: &BTreeMap<String, ModuleNamespace>,
    name: &str,
    found: &mut Option<String>,
    ambiguous: &mut bool,
) {
    for namespace in modules.values() {
        if namespace.classes.contains_key(name)
            || namespace.all_classes.contains_key(name)
            || namespace.enums.contains_key(name)
            || namespace.all_enums.contains_key(name)
            || namespace.traits.contains_key(name)
            || namespace.all_traits.contains_key(name)
        {
            if let Some(existing) = found {
                if existing != &namespace.path {
                    *ambiguous = true;
                }
            } else {
                *found = Some(namespace.path.clone());
            }
        }
        find_type_namespace_path(&namespace.modules, name, found, ambiguous);
    }
}

fn qualify_export_type(program: &Program, ty: &sema::Type) -> sema::Type {
    match ty {
        sema::Type::Named(name, args) => {
            let qualified_args = args
                .iter()
                .map(|arg| qualify_export_type(program, arg))
                .collect::<Vec<_>>();
            if name.contains('.') || is_builtin_export_type(name) {
                return sema::Type::Named(name.clone(), qualified_args);
            }
            if program.classes.contains_key(name)
                || program.enums.contains_key(name)
                || program.traits.contains_key(name)
                || program.opaque_handles.contains_key(name)
            {
                return sema::Type::Named(
                    format!("{}.{}", program.module_name, name),
                    qualified_args,
                );
            }
            let mut found = None;
            let mut ambiguous = false;
            find_type_namespace_path(&program.imported_modules, name, &mut found, &mut ambiguous);
            if let (Some(path), false) = (found, ambiguous) {
                return sema::Type::Named(format!("{}.{}", path, name), qualified_args);
            }
            sema::Type::Named(name.clone(), qualified_args)
        }
        sema::Type::Tuple(elements) => sema::Type::Tuple(
            elements
                .iter()
                .map(|element| qualify_export_type(program, element))
                .collect(),
        ),
        sema::Type::Function {
            params,
            return_type,
        } => sema::Type::Function {
            params: params
                .iter()
                .map(|param| sema::FunctionParamContract {
                    name: param.name.clone(),
                    ty: qualify_export_type(program, &param.ty),
                    passing: param.passing,
                    has_default: param.has_default,
                    default_erased: param.default_erased,
                })
                .collect(),
            return_type: Box::new(qualify_export_type(program, return_type)),
        },
        sema::Type::Closure {
            params,
            return_type,
            captures,
            call_kind,
        } => sema::Type::Closure {
            params: Box::new(
                params
                    .iter()
                    .map(|param| sema::FunctionParamContract {
                        name: param.name.clone(),
                        ty: qualify_export_type(program, &param.ty),
                        passing: param.passing,
                        has_default: param.has_default,
                        default_erased: param.default_erased,
                    })
                    .collect(),
            ),
            return_type: Box::new(qualify_export_type(program, return_type)),
            captures: Box::new(
                captures
                    .iter()
                    .map(|capture| sema::ClosureCapture {
                        name: capture.name.clone(),
                        ty: qualify_export_type(program, &capture.ty),
                        mode: capture.mode,
                        span: capture.span,
                    })
                    .collect(),
            ),
            call_kind: *call_kind,
        },
        sema::Type::TypeParam(name) => sema::Type::TypeParam(name.clone()),
        sema::Type::Module(path) => sema::Type::Module(path.clone()),
        sema::Type::Unit => sema::Type::Unit,
    }
}

fn qualify_export_type_ref(program: &Program, type_ref: &ast::TypeRef) -> ast::TypeRef {
    let mut qualified = type_ref.clone();
    match &mut qualified.kind {
        ast::TypeRefKind::Tuple(elements) => {
            *elements = elements
                .iter()
                .map(|element| qualify_export_type_ref(program, element))
                .collect();
        }
        ast::TypeRefKind::Function {
            params,
            return_type,
        } => {
            for param in params {
                param.ty = qualify_export_type_ref(program, &param.ty);
            }
            **return_type = qualify_export_type_ref(program, return_type);
        }
        ast::TypeRefKind::Named { name, args } => {
            *args = args
                .iter()
                .map(|arg| qualify_export_type_ref(program, arg))
                .collect();
            if name.contains('.') || name == "str" || is_builtin_export_type(name) {
                return qualified;
            }
            if program.classes.contains_key(name)
                || program.enums.contains_key(name)
                || program.traits.contains_key(name)
            {
                *name = format!("{}.{}", program.module_name, name);
                return qualified;
            }
            let mut found = None;
            let mut ambiguous = false;
            find_type_namespace_path(&program.imported_modules, name, &mut found, &mut ambiguous);
            if let (Some(path), false) = (found, ambiguous) {
                *name = format!("{}.{}", path, name);
            }
        }
    }
    qualified
}

fn qualify_export_bounds(
    program: &Program,
    bounds: &BTreeMap<String, Vec<ast::TypeRef>>,
) -> BTreeMap<String, Vec<ast::TypeRef>> {
    bounds
        .iter()
        .map(|(name, refs)| {
            (
                name.clone(),
                refs.iter()
                    .map(|type_ref| qualify_export_type_ref(program, type_ref))
                    .collect(),
            )
        })
        .collect()
}

fn qualify_function_decl_for_export(
    program: &Program,
    decl: &ast::FunctionDecl,
) -> ast::FunctionDecl {
    let mut qualified = decl.clone();
    qualified.type_param_bounds = qualify_export_bounds(program, &qualified.type_param_bounds);
    qualified.params = qualified
        .params
        .iter()
        .map(|param| {
            let mut qualified_param = param.clone();
            qualified_param.ty = qualify_export_type_ref(program, &qualified_param.ty);
            qualified_param
        })
        .collect();
    qualified.return_type = qualify_export_type_ref(program, &qualified.return_type);
    qualified
}

fn qualify_class_decl_for_export(program: &Program, decl: &ast::ClassDecl) -> ast::ClassDecl {
    let mut qualified = decl.clone();
    qualified.type_param_bounds = qualify_export_bounds(program, &qualified.type_param_bounds);
    qualified.fields = qualified
        .fields
        .iter()
        .map(|field| {
            let mut qualified_field = field.clone();
            qualified_field.ty = qualify_export_type_ref(program, &qualified_field.ty);
            qualified_field
        })
        .collect();
    qualified.methods = qualified
        .methods
        .iter()
        .map(|method| qualify_function_decl_for_export(program, method))
        .collect();
    qualified
}

fn qualify_enum_decl_for_export(program: &Program, decl: &ast::EnumDecl) -> ast::EnumDecl {
    let mut qualified = decl.clone();
    qualified.type_param_bounds = qualify_export_bounds(program, &qualified.type_param_bounds);
    qualified.variants = qualified
        .variants
        .iter()
        .map(|variant| {
            let mut qualified_variant = variant.clone();
            qualified_variant.payloads = qualified_variant
                .payloads
                .iter()
                .map(|payload| {
                    let mut qualified_payload = payload.clone();
                    qualified_payload.ty = qualify_export_type_ref(program, &payload.ty);
                    qualified_payload
                })
                .collect();
            qualified_variant
        })
        .collect();
    qualified
}

fn qualify_trait_decl_for_export(program: &Program, decl: &ast::TraitDecl) -> ast::TraitDecl {
    let mut qualified = decl.clone();
    qualified.methods = qualified
        .methods
        .iter()
        .map(|method| qualify_function_decl_for_export(program, method))
        .collect();
    qualified
}

fn qualify_impl_decl_for_export(program: &Program, decl: &ast::ImplDecl) -> ast::ImplDecl {
    let mut qualified = decl.clone();
    qualified.type_param_bounds = qualify_export_bounds(program, &qualified.type_param_bounds);
    qualified.trait_args = qualified
        .trait_args
        .iter()
        .map(|arg| qualify_export_type_ref(program, arg))
        .collect();
    qualified.for_type = qualify_export_type_ref(program, &qualified.for_type);
    qualified.methods = qualified
        .methods
        .iter()
        .map(|method| qualify_function_decl_for_export(program, method))
        .collect();
    qualified
}

fn qualify_function_info_for_export(
    program: &Program,
    info: &sema::FunctionInfo,
) -> sema::FunctionInfo {
    let mut qualified = info.clone();
    qualified.decl = qualify_function_decl_for_export(program, &qualified.decl);
    qualified.signature.params = qualified
        .signature
        .params
        .iter()
        .map(|ty| qualify_export_type(program, ty))
        .collect();
    qualified.signature.return_type =
        qualify_export_type(program, &qualified.signature.return_type);
    qualified
}

fn qualify_extern_function_info_for_export(
    program: &Program,
    info: &sema::ExternFunctionInfo,
) -> sema::ExternFunctionInfo {
    let mut qualified = info.clone();
    for param in &mut qualified.decl.params {
        param.ty = qualify_export_type_ref(program, &param.ty);
    }
    qualified.decl.return_type = qualify_export_type_ref(program, &qualified.decl.return_type);
    qualified.signature.params = qualified
        .signature
        .params
        .iter()
        .map(|ty| qualify_export_type(program, ty))
        .collect();
    qualified.signature.return_type =
        qualify_export_type(program, &qualified.signature.return_type);
    qualified
}

fn qualify_class_info_for_export(program: &Program, info: &sema::ClassInfo) -> sema::ClassInfo {
    let mut qualified = info.clone();
    qualified.decl = qualify_class_decl_for_export(program, &qualified.decl);
    for field in qualified.fields.values_mut() {
        field.ty = qualify_export_type(program, &field.ty);
    }
    for method in qualified.methods.values_mut() {
        method.decl = qualify_function_decl_for_export(program, &method.decl);
        method.signature.params = method
            .signature
            .params
            .iter()
            .map(|ty| qualify_export_type(program, ty))
            .collect();
        method.signature.return_type = qualify_export_type(program, &method.signature.return_type);
    }
    qualified
}

fn qualify_enum_info_for_export(program: &Program, info: &sema::EnumInfo) -> sema::EnumInfo {
    let mut qualified = info.clone();
    qualified.decl = qualify_enum_decl_for_export(program, &qualified.decl);
    for variant in qualified.variants.values_mut() {
        variant.payloads = variant
            .payloads
            .iter()
            .map(|payload| sema::EnumPayloadFieldInfo {
                name: payload.name.clone(),
                ty: qualify_export_type(program, &payload.ty),
                span: payload.span,
            })
            .collect();
    }
    qualified
}

fn qualify_trait_info_for_export(program: &Program, info: &sema::TraitInfo) -> sema::TraitInfo {
    let mut qualified = info.clone();
    qualified.decl = qualify_trait_decl_for_export(program, &qualified.decl);
    for method in qualified.methods.values_mut() {
        method.decl = qualify_function_decl_for_export(program, &method.decl);
        method.signature.params = method
            .signature
            .params
            .iter()
            .map(|ty| qualify_export_type(program, ty))
            .collect();
        method.signature.return_type = qualify_export_type(program, &method.signature.return_type);
    }
    qualified
}

fn qualify_trait_impl_info_for_export(
    program: &Program,
    info: &sema::TraitImplInfo,
) -> sema::TraitImplInfo {
    let mut qualified = info.clone();
    qualified.decl = qualify_impl_decl_for_export(program, &qualified.decl);
    qualified.trait_args = qualified
        .trait_args
        .iter()
        .map(|ty| qualify_export_type(program, ty))
        .collect();
    qualified.for_type = qualify_export_type(program, &qualified.for_type);
    for method in qualified.methods.values_mut() {
        method.decl = qualify_function_decl_for_export(program, &method.decl);
        method.signature.params = method
            .signature
            .params
            .iter()
            .map(|ty| qualify_export_type(program, ty))
            .collect();
        method.signature.return_type = qualify_export_type(program, &method.signature.return_type);
    }
    qualified
}

fn qualify_constant_info_for_export(
    program: &Program,
    info: &sema::ConstantInfo,
) -> sema::ConstantInfo {
    let mut qualified = info.clone();
    qualified.ty = qualify_export_type(program, &qualified.ty);
    qualified
}

fn exported_binding(program: &Program, name: &str) -> Option<ImportedBinding> {
    if let Some(constant) = program
        .constants
        .get(name)
        .filter(|constant| constant.module_name == program.module_name && constant.decl.public)
    {
        return Some(ImportedBinding::Constant(qualify_constant_info_for_export(
            program, constant,
        )));
    }
    for item in &program.module.items {
        match item {
            Item::ExternFunction(decl) if decl.name == name && decl.public => {
                return program
                    .extern_functions
                    .get(name)
                    .map(|info| qualify_extern_function_info_for_export(program, info))
                    .map(ImportedBinding::ExternFunction);
            }
            Item::ExternOpaqueClass(decl) if decl.name == name && decl.public => {
                return program
                    .opaque_handles
                    .get(name)
                    .cloned()
                    .map(ImportedBinding::OpaqueHandle);
            }
            Item::Function(decl) if decl.name == name && decl.public => {
                return program
                    .functions
                    .get(name)
                    .map(|info| qualify_function_info_for_export(program, info))
                    .map(ImportedBinding::Function);
            }
            Item::Class(decl) if decl.name == name && decl.public => {
                return program
                    .classes
                    .get(name)
                    .map(|info| qualify_class_info_for_export(program, info))
                    .map(ImportedBinding::Class);
            }
            Item::Enum(decl) if decl.name == name && decl.public => {
                return program
                    .enums
                    .get(name)
                    .map(|info| qualify_enum_info_for_export(program, info))
                    .map(ImportedBinding::Enum);
            }
            Item::Trait(decl) if decl.name == name && decl.public => {
                return program
                    .traits
                    .get(name)
                    .map(|info| qualify_trait_info_for_export(program, info))
                    .map(ImportedBinding::Trait);
            }
            _ => {}
        }
    }
    None
}

fn exported_namespace(path: &[String], program: &Program) -> ModuleNamespace {
    let name = path
        .last()
        .cloned()
        .unwrap_or_else(|| program.module_name.clone());
    let mut namespace = ModuleNamespace {
        constants: program
            .constants
            .iter()
            .filter(|(_, info)| info.module_name == program.module_name && info.decl.public)
            .map(|(name, info)| {
                (
                    name.clone(),
                    qualify_constant_info_for_export(program, info),
                )
            })
            .collect(),
        all_constants: program
            .constants
            .iter()
            .filter(|(_, info)| info.module_name == program.module_name)
            .map(|(name, info)| {
                (
                    name.clone(),
                    qualify_constant_info_for_export(program, info),
                )
            })
            .collect(),
        name,
        path: path.join("."),
        source_path: program.source_path.clone(),
        modules: BTreeMap::new(),
        functions: BTreeMap::new(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: program
            .trait_impls
            .iter()
            .map(|info| qualify_trait_impl_info_for_export(program, info))
            .collect(),
        all_functions: program
            .functions
            .iter()
            .map(|(name, info)| {
                (
                    name.clone(),
                    qualify_function_info_for_export(program, info),
                )
            })
            .collect(),
        all_extern_functions: program
            .extern_functions
            .iter()
            .map(|(name, info)| {
                (
                    name.clone(),
                    qualify_extern_function_info_for_export(program, info),
                )
            })
            .collect(),
        all_opaque_handles: program.opaque_handles.clone(),
        all_classes: program
            .classes
            .iter()
            .map(|(name, info)| (name.clone(), qualify_class_info_for_export(program, info)))
            .collect(),
        all_enums: program
            .enums
            .iter()
            .map(|(name, info)| (name.clone(), qualify_enum_info_for_export(program, info)))
            .collect(),
        all_traits: program
            .traits
            .iter()
            .map(|(name, info)| (name.clone(), qualify_trait_info_for_export(program, info)))
            .collect(),
        imported_modules: program.imported_modules.clone(),
        closures: program
            .closures
            .iter()
            .map(|(id, info)| {
                let mut qualified = info.clone();
                qualified.params = qualified
                    .params
                    .iter()
                    .map(|param| sema::FunctionParamContract {
                        name: param.name.clone(),
                        ty: qualify_export_type(program, &param.ty),
                        passing: param.passing,
                        has_default: param.has_default,
                        default_erased: param.default_erased,
                    })
                    .collect();
                qualified.return_type = qualify_export_type(program, &qualified.return_type);
                qualified.captures = qualified
                    .captures
                    .iter()
                    .map(|capture| sema::ClosureCapture {
                        name: capture.name.clone(),
                        ty: qualify_export_type(program, &capture.ty),
                        mode: capture.mode,
                        span: capture.span,
                    })
                    .collect();
                (id.clone(), qualified)
            })
            .collect(),
        comprehensions: program
            .comprehensions
            .iter()
            .map(|(id, info)| {
                let mut qualified = info.clone();
                qualified.result_type = qualify_export_type(program, &qualified.result_type);
                qualified.clauses = qualified
                    .clauses
                    .iter()
                    .map(|clause| sema::ComprehensionClauseInfo {
                        binding_type: qualify_export_type(program, &clause.binding_type),
                        receive_owned: clause.receive_owned,
                    })
                    .collect();
                (id.clone(), qualified)
            })
            .collect(),
    };

    for item in &program.module.items {
        match item {
            Item::ExternFunction(decl) if decl.public => {
                if let Some(info) = program.extern_functions.get(&decl.name) {
                    namespace.extern_functions.insert(
                        decl.name.clone(),
                        qualify_extern_function_info_for_export(program, info),
                    );
                }
            }
            Item::ExternOpaqueClass(decl) if decl.public => {
                if let Some(info) = program.opaque_handles.get(&decl.name) {
                    namespace
                        .opaque_handles
                        .insert(decl.name.clone(), info.clone());
                }
            }
            Item::Function(decl) if decl.public => {
                if let Some(info) = program.functions.get(&decl.name) {
                    namespace.functions.insert(
                        decl.name.clone(),
                        qualify_function_info_for_export(program, info),
                    );
                }
            }
            Item::Class(decl) if decl.public => {
                if let Some(info) = program.classes.get(&decl.name) {
                    namespace.classes.insert(
                        decl.name.clone(),
                        qualify_class_info_for_export(program, info),
                    );
                }
            }
            Item::Enum(decl) if decl.public => {
                if let Some(info) = program.enums.get(&decl.name) {
                    namespace.enums.insert(
                        decl.name.clone(),
                        qualify_enum_info_for_export(program, info),
                    );
                }
            }
            Item::Trait(decl) if decl.public => {
                if let Some(info) = program.traits.get(&decl.name) {
                    namespace.traits.insert(
                        decl.name.clone(),
                        qualify_trait_info_for_export(program, info),
                    );
                }
            }
            _ => {}
        }
    }

    namespace
}

fn insert_namespace_import(
    bindings: &mut BTreeMap<String, ImportedBinding>,
    path: &[String],
    leaf: ModuleNamespace,
    span: Span,
) -> Result<()> {
    if path.is_empty() {
        return Ok(());
    }
    let root_name = path[0].clone();
    let root = bindings.entry(root_name.clone()).or_insert_with(|| {
        ImportedBinding::Module(ModuleNamespace {
            constants: BTreeMap::new(),
            all_constants: BTreeMap::new(),
            name: root_name.clone(),
            path: root_name.clone(),
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
        })
    });
    let ImportedBinding::Module(root_namespace) = root else {
        return Err(Diagnostic::at(
            span,
            format!("duplicate import binding `{}`", root_name),
        ));
    };

    if path.len() == 1 {
        *root_namespace = leaf;
        return Ok(());
    }

    let mut current = root_namespace;
    let mut prefix = root_name.clone();
    for segment in &path[1..path.len() - 1] {
        prefix = format!("{}.{}", prefix, segment);
        current = current
            .modules
            .entry(segment.clone())
            .or_insert_with(|| ModuleNamespace {
                constants: BTreeMap::new(),
                all_constants: BTreeMap::new(),
                name: segment.clone(),
                path: prefix.clone(),
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
            });
    }
    let last = path[path.len() - 1].clone();
    current.modules.insert(last, leaf);
    Ok(())
}

fn insert_aliased_namespace_import(
    bindings: &mut BTreeMap<String, ImportedBinding>,
    alias: &str,
    namespace: ModuleNamespace,
    span: Span,
) -> Result<()> {
    if bindings
        .insert(alias.to_string(), ImportedBinding::Module(namespace))
        .is_some()
    {
        return Err(Diagnostic::at(
            span,
            format!("duplicate import binding `{alias}`"),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
