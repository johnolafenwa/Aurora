# Conformance

Aura keeps the language reference and implementation aligned through executable conformance layers. This page identifies which tests substantiate each part of the specification and what a conforming implementation is expected to do.

## Conforming Programs And Implementations

A **conforming Aura program** uses only syntax and APIs defined by this Manual and satisfies all static rules.

A **conforming Aura implementation**:

- accepts every conforming program within documented implementation limits
- rejects programs that violate a MUST-level lexical, grammatical, name, type, ownership, or entrypoint rule
- preserves the observable evaluation and cleanup behavior defined by the Manual
- produces the specified typed outcomes or runtime failures
- provides the maintained public API surface
- does not expose proposal-only constructs as accepted 0.3 language features

Exact diagnostic prose is normative only where a fixture or this Manual explicitly requires it. A conforming implementation otherwise needs a clear diagnostic with an accurate source location and the same stable `AU####` code; message wording may differ without changing that code's documented meaning.

## Executable Reference dict

| Reference area | Primary executable evidence |
| --- | --- |
| UTF-8, indentation, tokens, literals, escapes | `crates/aura-compiler/src/lexer_tests.rs` |
| Delimiter continuation, ignored continuation indentation, expression-match layout islands, trailing-comma/backslash/single-line-string boundaries, and pairing diagnostics | focused lexer/parser tests; `newline_continuation*` and delimiter parse/run fixtures; `examples/basics/multiline_expressions.au`; compiler-bridge and extension indentation tests; and the MIR/direct parity matrix |
| Parenthesized tuple values/types, recursive assignment/loop targets and patterns, function returns, left-to-right capture, recursive Copy, whole-source non-Copy moves, shared leaf provenance, copy-only constant indexing, canonical rendering, same-type recursive structural `==`/`!=` with non-consuming reads, retained ordering rejection, and mutable-writeback rejections | focused lexer/parser/sema/MIR/native/runtime tests; the `tuple_structural_equality`, `tuple_equality_contextual_literals`, and retained `tuple_ordering_rejected` fixtures; other `tuple_*` parse/check/run fixtures; `examples/basics/tuples.au`; the executable `docs/manual/tuples.md` fence; the compiler-bridge tuple equality/ordering regression; and the MIR/direct parity matrix |
| grammar and parser limits | `crates/aura-compiler/src/parser_tests.rs`, `tests/fixtures/parse-pass`, `tests/fixtures/parse-fail` |
| FFI v0 package authorization and root dependency reports; bodyless extern/opaque grammar; fixed-width scalar, null-empty and pointer-length view ABI; same-length mutable copy-in/out and post-call writeback; opaque ownership and non-Transfer rules; direct-call-only process-global lookup; Unix MIR/direct parity; LSP/editor support; and reserved callbacks/raw pointers/variadics | focused lexer/parser/package/sema/analysis/MIR/native/runtime tests; `crates/aura-compiler/tests/ffi_frontend.rs`; `crates/aura/tests/ffi_acceptance.rs`; `examples/packages/ffi_getpid`; the executable [FFI v0](/manual/ffi) block; language-server recovery/compiler-bridge tests; extension grammar/snippet tests; and the forced backend parity matrix |
| Conditional-expression precedence, exact-bool conditions, arm unification/context, lazy condition-first selection, conservative branch moves, analysis, and backend parity | focused conditional parser/sema/MIR/analysis tests; `conditional_expression_*` check/parse fixtures; `conditional_expressions` run fixture and example; compiler-bridge coverage; and the MIR/direct parity matrix |
| Membership containers and delegation, chained-comparison precedence, at-most-once operand evaluation, short-circuiting, conservative chain checking, and backend parity | `comparison_chains_keep_every_operator_at_one_precedence_level`, `membership_tests_read_supported_containers_and_reject_the_rest`, and `comparison_chains_evaluate_each_operand_once_and_short_circuit`; `membership_*` check-fail and `membership_and_comparison_chains` check-pass fixtures; the `membership_and_comparison_chains` run fixture and example; five Python-shaped acceptance fixtures; and the MIR/direct parity matrix |
| `enumerate`/`zip` loop-form recognition, operand domain, bare shared default, `int64` positions, shortest-operand `zip` termination, shadowing, function-wide per-loop binding-slot isolation under heterogeneous binding-name reuse, and backend parity | `enumerate_and_zip_iterate_in_lockstep_over_the_bare_loop_default`, `heterogeneous_ordinary_for_bindings_use_distinct_scoped_typed_slots`, `every_ordinary_for_form_uses_a_fresh_scoped_target_slot`, and `ordinary_for_target_scope_starts_after_iterable_evaluation`; `enumerate_requires_indexable_iterable` and `zip_rejects_ownership_modifiers` check-fail fixtures; the reversed heterogeneous `enumerate_and_zip` fixture; `tuple_for_pattern_queue` for recursive target reuse; `list_mut_iteration` for fallthrough/`continue`/`break`/explicit-return writeback; the maintained example; and the MIR/direct parity matrix |
| `len` delegation domain; shared `int64` type and value across `len(x)` and `x.len()` for str, list, dict, and set; `str.byte_len()` as an `int64` UTF-8-byte count; `str` rendering equality with `print` and f-strings; and the reservation of both names | focused semantic, call-surface, MIR, native-codegen, runtime, analysis, and language-server tests; the `len_requires_a_len_member` check-fail fixture; the `len_and_str` run fixture and example; two Python-shaped acceptance fixtures; and the MIR/direct parity matrix |
| Accepted ADR-0043 unified `int64` index domain: direct indices, slices, range bounds and yields, enumerate positions, Array coordinates, selection indices, collection lengths/search positions/capacity arguments, scoped lossless widening of fixed-width integer position values, exact `list[int64]` coordinate containers, and target-stable rejection outside that scope | `index_domain_positions_contextually_type_literals_as_int64`, `index_domain_accepts_default_int64_variables`, and focused semantic/MIR/native/runtime/analysis tests; `index_domain_int64_contract`, `index_domain_lossless_widening`, `index_domain_rejects_uint64`, and boundary fixtures; `index_domain_zero_cast_idioms`, which pins `values[values.len() - 1]`, `range(values.len())`, and enumerate-index-back-into-list without casts; `examples/collections/list_polish.au`; and the forced MIR/direct parity matrix |
| Accepted ADR-0044 canonical `list`/`dict`/`set`/`str` surface: homogeneous literals, exact constructors and method signatures, first-match list search, stable natural/key sorting, typed dictionary absence and eager snapshots, loud/silent set removal, capacity control, exact ownership and evaluation order, `AU4008`/`AU4003`/`AU4005` failures, analysis/editor exposure, and backend parity | focused parser, call-surface, semantic, MIR, native, runtime-value, analysis, language-server, and extension tests; `canonical_collection_surface`, collection capacity, obligation, missing-value, and ownership fixture families; `examples/collections/list_basics.au`, `list_polish.au`, `dict_basics.au`, and `set_basics.au`; executable Types and Collections Manual blocks; the clean-surface identity gate; and the forced MIR/direct parity matrix |
| names, types, calls, traits, patterns, moves, and borrows | `crates/aura-compiler/src/sema_tests.rs`, `tests/fixtures/check-pass`, `tests/fixtures/check-fail` |
| integer `/` rejection, floor division/remainder, exact float-context integer literals, `.to_float()`, and shortest-roundtrip float printing | lexer/parser/integer/runtime-value unit tests plus `integer_true_division_*`, `floor_division_and_modulo`, `float_context_integer_literals`, `integer_to_float_rounding`, and `float_shortest_roundtrip_printing` fixtures |
| Accepted ADR-0047 decimal separators and hexadecimal/binary/octal integer literals; exact-width bitwise operators; checked, wrapping, and saturating shifts; exact count typing/ranges; compound-store ordering; and MIR/direct parity | focused lexer/parser/sema/integer/MIR/native/runtime tests; literal and shift failure fixtures; the `bitwise_power` runtime fixture; `examples/numbers/bit_packing.au`; the executable Expressions Manual block; language-server and extension coverage; and the forced MIR/direct parity matrix |
| Accepted ADR-0048 scalar math constants and functions: exact immutable `float64` constant bits, generic once-initialized module storage, exact function signatures, checked `int64` rounding conversions, IEEE-754 identity and exceptional-value classification, finite overflow and domain diagnostics, left-to-right once-only evaluation, and MIR/direct parity | `math_namespace_exposes_exact_generic_float64_constants`, `math_namespace_exposes_the_exact_float64_function_contract`, `math_host_builtins_follow_the_ratified_finite_contract`, `math_host_builtins_classify_every_exception_family`, `math_analysis_exposes_qualified_and_aliased_constant_details`, `math_analysis_completes_and_hovers_the_exact_public_surface`, and focused semantic tests; the `math_module_constants`, `math_module_functions`, and `math_log_domain` fixtures; `examples/numbers/scalar_math.au`; the executable [Math Module](/manual/math) block; compiler-bridge coverage; and the forced MIR/direct parity matrix |
| Accepted ADR-0048 power, `round`, and `divmod`: right-associative precedence, exact numeric types, checked integer results, ties-to-even conversion, paired floor quotient/remainder, runtime classification, one-time evaluation, and MIR/direct parity | focused parser/sema/integer/runtime-value/MIR/native tests; `numeric_round_divmod`, `bitwise_power`, and numeric failure fixtures; `examples/numbers/bit_packing.au`; the executable Expressions Manual block; language-server and extension coverage; and the forced MIR/direct parity matrix |
| Accepted ADR-0046 exact triple-quoted and raw string forms plus static f-string format specifications: delimiter and escape rules, no whitespace normalization, Unicode-scalar width and string precision, sign-aware numeric zero padding, type-directed numeric codes, binary32 identity, left-to-right evaluation, focused raw/triple-f diagnostics, formatter preservation, and MIR/direct parity | focused lexer/parser/sema/runtime-value/MIR/native tests; `string_literal_forms_and_format_specs`, `fstring_zero_padding`, and `fstring_*` rejection fixtures; `examples/strings/literal_forms_and_formatting.au`; executable lexical and expression reference blocks; compiler-bridge and extension grammar/snippet tests; and the forced MIR/direct parity matrix |
| Accepted ADR-0049 match guards, or-patterns, and top-level catch-all bindings: exact-`bool` guards, left-to-right alternatives, identical alternative bindings, guarded-arm reachability and exhaustiveness, delayed `match own` extraction, mutable candidate writeback before false continuation/failure/trap and every selected-arm exit, complete-scrutinee binding capabilities, and MIR/direct parity; class patterns are formally deferred to a future dedicated ADR | focused parser/sema/MIR/analysis tests; the `match_guard*`, `match_own*`, `match_mut_guard*`, `match_root_binding_patterns`, `or_pattern*`, guarded-exhaustiveness, and focused class-pattern-rejection fixtures; `examples/enums/match_guards_and_or_patterns.au`; the executable Enums and Match Manual block; compiler-bridge tests; and the forced MIR/direct parity matrix |
| Accepted ADR-0050 module constants: inferred/annotated/public declarations, declaration-order scope, dependency-first and source-ordered eager once-only initialization, one defining storage identity, Copy versus shared non-Copy reads, move/mutation rejection, guarded re-entry, package visibility, analysis/editor exposure, cleanup, and MIR/direct parity | focused parser/sema/MIR/native/runtime/analysis tests; `module_constant*` check/run fixtures; multi-module and package dependency tests; `examples/modules/constants.au`; executable Names and Scopes, Statements, and Ownership Manual blocks; language-server compiler-bridge tests; and the forced MIR/direct parity matrix |
| signed-i128-nanosecond Duration literals, exact two-limb direct ABI, constructors, checked arithmetic, `FloorDiv`, comparison, conversion, rendering, and invalid host timers | `duration_literals_scale_to_nonnegative_i128_nanoseconds_at_each_unit_boundary`, parser/MIR/native-codegen/runtime unit tests, `docs/manual/concurrency.md#aura-1`, Duration run/failure fixtures, native-runtime FFI tests, and the MIR/direct parity matrix |
| persistent descriptor registrations, heap-ordered deadlines, direct Queue/task-completion/blocking-pool wakeups, wait-epoch race containment, one-winner cleanup, and event-or-deadline idle blocking without a periodic tick | `runtime_reactor` unit tests including `timers_fire_at_the_earliest_deadline_and_preserve_equal_deadline_order`, `one_persistent_fd_registration_aggregates_waiters_and_narrows_interest`, and `waker_coalescing_does_not_lose_inbox_entries_and_ready_is_deduplicated`; runtime-value direct-wakeup and cleanup tests; `scheduler_model` lost-wake/stale-epoch/one-winner state-space tests; `scheduler_mixed_wakeups_complete_in_mir_and_direct_backends`; `scripts/stress-scheduler.sh`; the contractual `scripts/bench-scalable-runtime.py` after-reactor run; and the MIR/direct parity matrix |
| `yield_now` cooperative scheduling, zero-argument typing, explicit ready-set requeue, unit result, and backend parity | focused call-surface, semantic, MIR-runtime, native-codegen, analysis, language-server, and extension tests; the `yield_now` check/run fixtures; `examples/concurrency/yield_now.au`; and the forced MIR/direct parity matrix |
| compiler-inserted scheduling checks on every ordinary and `continue` loop backedge; exit-path bypass; no implicit cancellation check; amortized function-local MIR/native fuel; sequential-program elision; and timer/Queue/socket progress on both backends | focused MIR lowering, MIR-runtime, native-codegen, and validation tests; `loop_backedge_safepoints_prevent_timer_and_queue_starvation`; the loopback-socket safepoint regression; the `sleeper_vs_hot_loop.au` scalable-runtime workload; the contractual starvation benchmark; and the forced MIR/direct parity matrix |
| Accepted ADR-0032 guarded 512 KiB default task stacks, exact `int64` 256 KiB..64 MiB collision-free overrides with 256 KiB reserved for measured shallow tasks, page rounding without clamping, and off-coroutine HTTP/TLS/WebSocket protocol steps | focused call-surface, semantic, MIR, native-codegen, scheduler-allocation, protocol-service, recursion, language-server, and both-backend CLI tests; maintained loopback HTTP, TLS, and WebSocket round trips; the scalable-runtime same-process baseline/parked-task measurements; and the MIR/direct parity matrix |
| unique mutable scheduler ownership; owned nested-start admission with synchronous preparation failure and safe immediate waits; internal FIFO admission without a public scheduling-order promise; teardown cancellation and observer wakeup; MIR/Rust unwind; and exact-once direct child/root stack-reset containment | `nested_spawns_are_fifo_and_an_immediate_child_wait_is_safe`, `nested_stack_allocation_failure_is_synchronous_and_does_not_enqueue_a_task`, `lightweight_scheduler_teardown_cancels_abandoned_tasks_and_runs_cleanup_once`, `pure_rust_abandoned_task_unwinds_owned_values_once_at_teardown`, `direct_cleanup_can_spawn_a_child_before_the_parent_is_retired`, and `generated_root_cleanup_runs_once_on_forced_exit_and_not_on_normal_return` in `runtime_value_tests`; the direct-root, unstarted-task, started-task, and normal-completion ownership tests in `native_runtime_tests`; the event-multiset oracle in `scheduler_nested_spawns.au`; `nested_scheduler_spawns_preserve_outcomes_cleanup_and_backend_parity`; and the raw-scheduler-alias rejection in `scripts/check-hygiene.sh` |
| Accepted ADR-0033 structural Transfer, owned Copy snapshots, explicit/concrete generic task targets, Queue constructor/send payload enforcement, conditional Task Copy, static single-consumer observation, `AU3008` boundary diagnostics, `AU3009` duplication diagnostics, and atomic one-winner runtime defense | `task_boundaries_accept_structurally_transferable_values_and_results`, `task_boundary_diagnostics_explain_the_exact_nested_non_transfer_reason`, `task_transfer_checks_use_the_concrete_generic_specialization`, `queue_transport_requires_transfer_payloads_but_handle_only_methods_do_not`, `owned_builtin_snapshots_are_transfer_but_live_authority_is_not`, `task_target_explicit_specialization_and_contextual_defaults_are_concrete`, `task_capture_materializes_copy_snapshots_but_not_noncopy_shared_views`, `task_result_observation_rights_follow_repeatability`, and `clone_producing_operations_cannot_duplicate_task_observation_rights`; runtime-value, MIR-runtime, and native-runtime claim tests; the `task_transfer_*`, `queue_transfer_*`, and `task_result_*` check fixtures; `task_transfer_runtime_matrix.au`; its MIR/direct CLI parity test; compiler-service/LSP evidence; and the forced backend parity matrix |
| Pinned-worker multicore task execution, available-core default, provisional positive `AURA_WORKERS` override and exact `AU4006` rejection, stable spawn-time affinity across yield/timer/Queue waits, no migration or work stealing, cross-worker Queue/Task wakeups, per-task cancellation/diagnostic isolation, and MIR/direct parity with unspecified scheduling/output order | `lightweight_worker_count_defaults_and_rejects_invalid_overrides`, `lightweight_tasks_are_pinned_across_yield_timer_and_queue_waits`, `lightweight_workers_make_cpu_progress_concurrently`, task-context isolation tests, the `multicore_queue_task_matrix` run fixture, its MIR/direct CLI parity test, and the forced backend parity matrix |
| Accepted ADR-0035 blocking-I/O worker configuration, optional pending-only queue capacity, exact explicit counts, compatible unbounded default, FIFO scheduler-aware admission, pre-acceptance timeout/cancellation, accepted-job abandonment, lazy all-or-nothing startup, fatal pre-user-code `AU4006` validation, resolver-saturation recovery, default-parallel watchdog stability, and MIR/direct/standalone parity | `runtime_config` decoding tests; focused `BlockingIoPool` lifecycle, capacity, FIFO, race, abandonment, and injected-resolver tests in `runtime_value_tests`; forced-backend and standalone configuration/admission tests in `crates/aura/tests/cli.rs`; and the MIR/direct parity matrix |
| Accepted ADR-0036 typed runtime call frames and task ancestry, once-only pre-cleanup capture, per-frame source paths, human-note synthesis, additive diagnostic-schema-v1/LSP propagation, native private trap transport, normal-status distinction, and exact MIR/direct parity without frame masking | diagnostic, MIR-runtime, native-runtime, native-codegen, CLI cold/warm/concurrent-cache, LSP bridge, run-fail fixture, and complete forced-backend parity tests |
| deterministic xoshiro256** seeding/output, unbiased half-open integers, 53-bit floats, Fisher-Yates writeback, rendering, unavailable equality, direct and transitive no-clone ownership, inferred generic and trait clone-safety contracts, and OS-secure integer/byte boundaries | `src/randomness.rs`; `random_rng_clone_safety_defers_generic_obligations_to_use_sites`, `imported_rng_clone_obligations_and_qualified_wrapper_identity_survive_namespaces`, and focused trait/operator/`From` semantic tests; `random_deterministic_sequences`, `random_projected_shuffle`, `random_render`, `equality_rng_direct`, `random_transitive_clone_rejected`, `random_secure_smoke`, `random_invalid_*`, `random_secure_bytes_request_ceiling`, and `random_secure_bytes_request_ceiling_i64_max` fixtures; verified clone-safety examples in `docs/manual/generics-and-traits.md`; native-runtime FFI and language-server tests; and the MIR/direct parity matrix |
| `list[uint8]` bytes, strict UTF-8 conversion, lowercase/mixed-case hex, canonical padded base64, typed malformed-input offsets, raw SHA-256, shared inputs, and output-size preflights | `src/bytes_codec.rs` unit tests; `bytes_codecs_and_hashing`, `bytes_typed_errors`, and reserved-encoding fixtures; `examples/bytes/codecs_and_hashing.au`; the executable `docs/manual/bytes.md` fence; language-server tests; allocation-boundary tests; and the MIR/direct parity matrix |
| Accepted ADR-0045 assertion introspection: exact operand types, once-only left-to-right comparison and membership evaluation, shared-dispatch eligibility, bounded typed operand captures, lazy once-only messages, exact default/custom/empty/whitespace text, `AU4001` keyword span, operand and cleanup precedence, schema-1 structured fields, top-level scripts, and no stripping | `assert_*` parse/check/run fixtures and compiler unit tests; assertion CLI tests for forced MIR/direct execution and function-level `aura test`; `examples/basics/assertions.au`; the executable `docs/manual/assertions.md` fence; language-server and extension packaging tests; and the MIR/direct parity matrix |
| Accepted ADR-0045 test runner: source-order function and file-level discovery, canonical names, literal case-sensitive `-k` after expansion, zero-match success, per-case isolated setup/case/teardown ordering, teardown-after-trap and structured secondary failure, one-time ordered parameter registration with capture-free functions, output capture, timeouts, normalized paths, and schema-versioned ordered JSON records | the `aura_test_*` CLI family covering discovery, filters, JSON, hooks, checked-module reuse, FFI, parameter registration, capture rejection, labels, and timeouts; `aura_test_maintained_assertions_example_pins_the_runner_contract`; `examples/basics/assertions.au`; Tutorial 23; the normative CLI and Tooling testing contract; and ADR-0045's completion matrix |
| Application-level HTTP retry composition: retry only `503`, deterministic seed-42 jitter, exponential `Duration` backoff, final-attempt no-RNG/no-sleep behavior, explicit deadlines, and scoped resource cleanup | `examples/agents/retrying_network_worker.au` and `retrying_network_worker_runs_with_computed_backoff_on_both_backends` in `crates/aura/tests/cli.rs`, which pin the exact seven-request loopback trace on the MIR and forced-direct backends |
| Callable-powered list algorithms: stable mutable natural/key sorting, once-only left-to-right key evaluation before mutation, trap-before-mutation, eager shared map/filter traversal, owned results, source retention, exact shared callback capabilities, and filter clone safety | `list_algorithm_callbacks`, `list_sort_requires_mut_receiver`, `list_sort_rejects_non_orderable`, `list_map_callback_requires_shared`, `list_filter_rng_clone_safety`, and builtin-collision fixtures; focused semantic, MIR, runtime, native, analysis, and LSP tests; `examples/collections/list_algorithms.au`; the executable Collections Manual block; and the MIR/direct parity matrix |
| `control.retry`: immediate first attempt, every-Err retry policy, exact attempt budget, doubling Duration backoff, zero-delay sleep elision, no post-final sleep/multiply, exact last-error return, worker-trap/overflow/cancellation propagation, and both-backend behavior | `control_retry_surface` parse/check fixtures; `control_retry_basics` run fixture; focused compiler/runtime tests; `crates/aura/tests/control_retry.rs`; `examples/agents/retry_with_backoff.au`; the executable Control-Plane Manual block; and the MIR/direct parity matrix |
| Accepted ADR-0037 expression closures: contextual bare/`mut`/`own` parameters, expression-only bodies, Copy snapshots, non-Copy move-at-creation, repeatable reads, consuming single-use calls, read-only environments, shared-capability rejection, structural Transfer, cleanup, and MIR/direct parity | lambda parser/check/run/failure fixture families; focused semantic, MIR, runtime, native-codegen, analysis, completion, language-server, and extension tests; `examples/basics/closures.au`; the executable Closures Manual block; and the forced MIR/direct parity matrix |
| Accepted ADR-0039 eager owned comprehensions: list/set/dictionary forms, exact-Boolean filters, progressive non-leaking target scope, nested outer-major order, left-to-right filters, dictionary key-before-value replacement, every bare iterable including Queue receive ownership, explicit clone/move behavior, ADR-0037 capture interaction, generator/modifier teaching diagnostics, cleanup, and MIR/direct parity | comprehension parser/check/run/failure fixture families; focused semantic, MIR, runtime, analysis, completion, language-server, and extension tests; `examples/collections/comprehensions.au`; the executable Collections Manual block; and the forced MIR/direct parity matrix |
| Accepted ADR-0040 owned list/str slices under the unified ADR-0043 index domain: all omitted endpoint forms, `int64` endpoints with scoped lossless widening, once-only negative normalization, no clamping, invalid/reversed `AU4003`, Unicode-scalar str O(n) behavior, list clone-safety and task-repeatability, retained evaluate-once order, source/result independence, reserved step/assignment `AU2005`, str integer-index rejection, and MIR/direct parity | slice parser/check/run/failure fixture families; focused semantic, MIR, runtime, analysis, completion, language-server, and extension tests; `examples/collections/slices.au`; the executable Collections Manual block; and the forced MIR/direct parity matrix |
| Accepted ADR-0041 contiguous numeric Arrays and explicit integer modes: four exact dtypes, rank-at-least-one row-major owned storage, zero dimensions, constructor/count checks, multidimensional indexing, first-axis copy slices, exact-shape/scalar kernels, float-only division, deterministic row-major map/reductions, `float64` mean, checked/wrapping/saturating integer behavior, `AU4002` checked Array overflow and `AU4004` floating Array zero-divisor failures, `AU4003` coordinate/slice bounds, `AU4005` allocation/element-count failures, `AU4007` shape/rank/count/empty-reduction failures, and no array-shape broadcasting/views/promotion/accelerator | numeric-array semantic/MIR/runtime/native/parity fixture families, including synchronized checked-overflow and floating-zero-divisor diagnostics; compiler analysis, language-server, and extension protocol tests; `examples/numbers/numeric_arrays.au`; the executable Numeric Arrays Manual block; the tested `bench-numeric-arrays.py` raw/summary evidence protocol; and the forced MIR/direct parity matrix |
| recursive JSON parse/dump semantics, exact numeric classification, typed parse errors, deterministic formatting, accessors, ownership, and resource limits | JSON codec/runtime-value unit tests, including exact materialized-node boundaries and deterministic allocation-failure injection; `json_dynamic_values`, JSON ownership and run-fail fixtures; `examples/json/dynamic_values.au`; the executable `docs/manual/json.md` fence; language-server tests; and the MIR/direct parity matrix |
| dict duplicate-key replacement, key-before-value effects, indexed-read/simple-write ownership, and missing-key traps | `dict_literal_duplicate_keys`, `dict_index_non_copy_requires_explicit_clone`, `dict_index_assignment_consumes_noncopy_key`, and `dict_index_missing_key` fixtures plus the MIR/native parity matrix |
| Supplied/default order and named enum-argument source order with declaration-slot binding | `explicit_and_default_argument_order` plus the MIR/native parity matrix |
| Copy-value capture, immediate f-string rendering, and receiver-before-argument effects | `left_to_right_value_snapshotting` plus the MIR/native parity matrix |
| Compound binary dispatch for root/projected targets, copy-target capture, retained non-copy `AU3002`, and copy-only list/dict indexed targets | `operator_traits`, `left_to_right_value_snapshotting`, `compound_noncopy_target_rejects_rhs_mutation`, `list_compound_assignment_noncopy_element_rejected`, and `dict_compound_assignment_noncopy_value_rejected` fixtures plus the MIR/native parity matrix |
| Dedicated `AU3005`/`AU3006` indexed ownership codes, `AU3003` mutable-receiver classification, and `AU2005` str-constructor guidance | `list_index_non_copy_requires_explicit_clone`, `dict_index_non_copy_requires_explicit_clone`, `list_compound_assignment_noncopy_element_rejected`, `dict_compound_assignment_noncopy_value_rejected`, `immutable_mutating_method`, and `string_constructor_not_supported` fixtures plus the compiler-bridge tests |
| Clone-safety-aware `AU3005` indexed-read guidance, so the recommended recovery is never rejected in turn by `AU3007` | `random_list_index_requires_transfer`, `random_transitive_dict_index_requires_transfer`, `generic_list_index_clone_safety_guidance`, and `generic_dict_index_clone_safety_guidance` fixtures, the `random_index_remove_transfers_ownership` transfer fixture, and the compiler-bridge propagation test |
| Dedicated `AU2007` builtin function redefinition code, distinct from the `AU2006` builtin method collision | `builtin_function_names_cannot_be_redefined` fixture |
| Access-kind-specific `AU3002` recovery help, naming the read, mutation, or consumption that actually conflicts | `nested_consume_and_borrow_same_call`, `call_own_then_projected_copy_read_rejected`, and `binary_left_borrow_rejects_later_mutation` fixtures |
| Current class-field-default callable limit | `class_field_default_user_function_not_supported` fixture |
| Retained non-copy binary/index/method-receiver/call-argument/indexed-assignment borrows, nested-consumption containment, `AU3002` overlap rejection, and no hidden deep clone | `binary_left_borrow_rejects_later_mutation`, `projected_binary_left_borrow_rejects_later_mutation`, `index_base_borrow_rejects_index_mutation`, `indexed_assignment_target_rejects_index_mutation`, `method_receiver_borrow_rejects_nested_argument_mutation`, `retained_receiver_nested_consumption_repro`, `retained_argument_nested_consumption_repro`, `method_receiver_rejects_nested_argument_consumption`, and `retained_parameter_rejects_nested_argument_consumption` fixtures |
| Declaration-stable call/operator passing, directional exclusive-access checks, and the distinct task-capture boundary | `generic_borrow_specialization_retains_copy_argument`, `call_borrow_mut_then_copy_read_rejected`, `call_own_then_projected_copy_read_rejected`, `trait_operator_borrow_mut_receiver_requires_mutable`, `trait_operator_copy_left_retains_borrow`, `trait_operator_own_receiver_moves_value`, `trait_operator_own_receiver_rejects_rhs_read`, `trait_operator_own_rhs_moves_value`, `trait_unary_operator_own_receiver_moves_value`, `operator_trait_value_receiver_snapshot`, `task_capture_snapshots_copy_arguments`, and `task_group_receiver_rejects_owned_variadic_capture` fixtures plus the MIR/native parity matrix |
| Accepted ADR-0017 one-time list/set own-iteration selection and Queue handle capture without source-binding retargeting | `own_iteration_captures_collection`, `queue_iteration_captures_handle`, and the MIR/native parity matrix |
| Queue receive-item ownership, accepted bare iteration, rejected `own`/`mut` iteration modifiers, and progress after an earlier producer completes while CPU burners exceed the default worker count | `queue_bare_iteration_ownership`, `queue_own_iteration_rejected`, the maintained `mut` rejection fixtures, `check_and_direct_backend_reject_queue_iteration_modifiers`, and `queue_iteration_consumers_complete_with_more_cpu_burners_than_default_workers` on MIR and direct |
| Accepted ADR-0034 typed heterogeneous `select`: exact inference, ownership, cancellation-first/lowest-index arbitration, atomic registration, one winner, loser cleanup, cross-worker wakeups, nested generic payload typing, and MIR/direct parity | `typed_select_*` compiler/runtime tests, `select_*` fixtures and CLI tests, the maintained select example, compiler-bridge/editor coverage, and the forced-backend parity matrix |
| Builtin trait-method no-shadowing across every builtin target, inherited-default containment, and direct builtin precedence | `builtin_queue_trait_method_collision`, `builtin_task_inherited_trait_method_collision`, `builtin_task_group_trait_method_collision`, `builtin_list_trait_method_collision`, `builtin_string_trait_method_collision`, and `builtin_file_trait_method_collision` fixtures plus `builtin_method_names_cannot_be_shadowed_on_any_builtin_target` and `direct_backend_prefers_builtin_handle_member_if_collision_reaches_mir` |
| Fixed 256 MiB filesystem, 64 MiB stream/TLS-configuration, and 16 MiB incoming HTTP limits | injectable-limit and sparse-file tests in `src/runtime_value_tests.rs` plus MIR/forced-direct filesystem and HTTP tests in `crates/aura/tests/cli.rs` |
| module and package resolution, module aliases, per-entry from-import aliases, canonical target identity, alias visibility/collisions, and alias-aware analysis | `crates/aura-compiler/tests/modules.rs`, `tests/packages.rs`, `src/package_tests.rs`, the `import_aliases` parity fixture, `examples/modules/import_aliases.au`, and compiler-service/LSP alias tests |
| MIR semantics and runtime behavior | `src/mir_tests.rs`, `src/mir_runtime_tests.rs`, `tests/fixtures/run-pass`, `tests/fixtures/run-fail` |
| native semantics and resource ABI | `src/native_codegen_tests.rs`, `src/native_runtime_tests.rs`, `tests/native_runtime_ffi.rs` |
| MIR/native observable equivalence | `crates/aura/tests/backend_parity.rs` |
| CLI, entrypoints, diagnostics, and installed builds | `crates/aura/tests/cli.rs`, `crates/aura/tests/packages.rs` |
| analysis, completion, hover, definitions, invalidation | `tools/aura-language-server/test` |
| maintained examples | compiler example smoke tests and CLI product tests |

The exact repository gate is `npm run ci`. It runs formatting, Rust tests, backend parity, language-server and extension tests, compiler and LSP coverage gates, this reference check, the documentation build, dependency audits, Clippy with warnings denied, and repository hygiene.

## Fixture Classes

The compiler fixture directories have distinct contracts:

- `parse-pass`: source MUST form a valid AST; later static checking is not implied.
- `parse-fail`: source MUST be rejected during lexing or parsing with the stored diagnostic.
- `check-pass`: source MUST parse and satisfy the static semantics.
- `check-fail`: source MUST parse and then be rejected by static checking with the stored diagnostic.
- `run-pass`: source MUST check and produce the stored standard output through the maintained execution path.
- `run-fail`: source MUST check far enough to reach the intended runtime failure and produce the stored diagnostic behavior.

Regression tests supplement fixtures when a case needs multiple files, temporary packages, local sockets, processes, timing, cancellation, or comparison of execution backends.

## Backend Equivalence

Aura 0.3 has two maintained semantic runtime representations:

- `aura run` lowers checked source to MIR and executes it in the MIR runtime.
- `aura build --backend direct` lowers checked source to native code through the direct backend and links the native runtime.
- the default `aura build --backend auto` first attempts direct emission and may instead build a native launcher containing serialized MIR plus the MIR runtime.

For the maintained source subset, the paths MUST agree on:

- standard output and integer exit status
- return values and pattern results
- checked arithmetic and collection failures
- move/borrow-sensitive mutation and writeback
- eager comprehension order, ownership, target scope, and partial-result cleanup
- owned list/str slice endpoint order, bounds failures, Unicode scalar
  selection, clone safety, and source/result independence
- `with` cleanup order and primary runtime diagnostics
- complete structured runtime diagnostics, including typed call frames and task ancestry
- task, queue, cancellation, process, filesystem, and network outcomes within platform constraints

The parity matrix executes every eligible runtime fixture through both paths
and compares complete diagnostics without masking backend-specific frame
notes. A fixture may be excluded only through the explicit exclusion list,
with a reason that corresponds to an intentional harness or platform boundary
rather than an unexplained semantic divergence.

## Documentation Conformance

Reference changes are checked by `npm run check:reference`. The gate retains
the normative-page, navigation, grammar-anchor, execution-order, canonical
surface, and deleted-evaluator guards. It inventories every fenced
block in `docs/manual`. Fences labeled `aura` are Aura source. Bash, EBNF,
JSON, text, and TOML fences require an explicit contract, as does every other
fence language. A `python` fence means Python and is never interpreted as Aura.

Every fenced block has a source-hash-pinned contract in
`scripts/reference-integrity.json`:

| Contract | Gate behavior |
| --- | --- |
| `check` | extract the exact block and require `aura check` to succeed with the pinned output |
| `run` | extract the exact block and require `aura run` to produce the exact pinned standard output and standard error |
| `check-fail` | require rejection with the pinned exit status and diagnostic fragment |
| `package-check` | place the exact Aura block in a metadata-pinned local package layout and require `aura check` to succeed without network access |
| `command` | parse one exact Bash command without a shell and execute only the gate's allowlisted side-effect-free `aura check`/`aura run` form for a maintained `examples/*.au` path, with pinned output |
| `illustrative` | do not execute the block; require a specific reason explaining why it is notation, output, a dependent fragment, an unsafe command, or otherwise not a standalone executable unit |

The command contract never invokes a shell, follows pipes or continuations, or
runs build, network, dependency-update, server, or recursive repository-gate
commands. A documented `cargo run -p aura -- ...` prefix is normalized to the
already-built `aura` binary before the allowlisted subcommand runs. The proof is
therefore about the displayed Aura CLI behavior, not Cargo itself. Unsafe or
orchestration-only command blocks remain illustrative with their boundary
stated explicitly.

The source hash makes changes fail closed: editing, replacing, or reordering
fenced blocks requires an explicit review of their contracts. Adding a Manual
page also requires classifying it as a feature page or as a structural page
with a reason. Structural pages organize cross-cutting contracts. Every
feature page MUST contain these non-empty level-two sections:

- `Grammar`
- `Typing Rules`
- `Runtime Semantics`
- `Ownership And Evaluation Order`
- `Diagnostics`
- `Backend Support`
- `Limits And Implementation-Defined Behavior`
- `Status`

The `Diagnostics` section MUST name each applicable stable `AU` code. If a feature introduces no feature-specific diagnostic, it states exactly `No feature-specific diagnostics.` instead. This is an explicit audited claim, not permission to omit general diagnostics that apply to examples on the page.

Every feature page MUST also contain at least one verified fenced example in a
non-illustrative mode. A page cannot satisfy that rule with a stale source hash
or with an explanation-only fragment. This ensures that all current feature
chapters have a live compiler, package, or safe CLI proof rather than
relying only on prose.

The gate reports the total page and all-language fence inventory,
verified-versus-illustrative counts, per-page example counts, every missing
normative section, and every feature page without a verified example before
failing. Its focused Python tests pin all-language fence extraction,
stale-metadata rejection, illustrative-reason enforcement, the feature-section
and executable-example contracts, safe command/package preparation, and
compiler outcome matching.

The documentation build separately checks links and rendering. Language-facing changes still require compiler fixtures or maintained examples as directed by `AGENTS.md`; a checked Manual block proves the documented example's stated outcome, not every edge of the underlying rule.

## Adding Or Changing A Rule

A language or tooling behavior change is complete only when the same pass updates, where relevant:

1. a failing compiler, runtime, CLI, or LSP test
2. the implementation
3. the normative Manual page and grammar when syntax changes
4. the API Index when public APIs change
5. Current Limits when a boundary is added or removed
6. categorized examples and Learn/tutorial material
7. the task board and dated work note

Syntax expansion is frozen for the 0.3 technical-preview release. A new construct therefore needs an explicit compatibility decision rather than being accepted solely because it is easy to parse.

## Deriving A Book

A book may treat this reference as its factual source. It may introduce concepts in a different order, add motivation, diagrams, exercises, and larger examples, or omit advanced details from early chapters. It must preserve these constraints:

- every taught syntax form appears in the complete grammar
- every claimed type or ownership behavior agrees with the static semantics
- every runtime/API claim links back to a maintained contract
- proposal-only features are labeled as future design, not current Aura
- examples are compiled or run as part of the maintained repository surface

This division lets the reference remain precise while the book remains readable.
