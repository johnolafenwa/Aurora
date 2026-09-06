# Aura Examples

This directory contains runnable Aura programs for the current compiler bootstrap.

The examples are organized by topic so they can serve both as quick references and as a companion to the `tutorials/` directory.

Concurrency examples run on Aura's pinned-worker scheduler on both maintained
backends. The runtime uses the host's available cores by default; the
provisional `AURA_WORKERS=<positive integer>` environment override selects a
specific worker count for testing or deployment. A task is assigned when it is
spawned and its coroutine stack never migrates or participates in work
stealing. Queue and Task handles are the maintained cross-worker channels;
every other task capture and result remains an owned, compiler-checked
`Transfer` value. Examples never rely on task scheduling, completion, or
printed-output order unless they explicitly coordinate that order.

## Categories

### `basics/`

- `top_level_script.au`
  - top-level executable statements with inferred bindings
  - prints `156`
- `main_function.au`
  - `main` with an omitted `None` return type
  - prints `16`
- `none_values.au`
  - bare `None` as both the unit type and unit value
  - prints `1`
- `mutable_bindings.au`
  - `mut`, reassignment, and compound assignment
  - prints `5`
- `numbers.au`
  - signed integer floor division, divisor-sign remainder, exact float-context integer literals, floating true division through integer `.to_float()`, exact `as float64`, and shortest-roundtrip printing at the rounded `2^53 + 1` conversion boundary
  - prints:
    - `2`
    - `-3`
    - `2`
    - `-3`
    - `-2`
    - `3.5`
    - `2.0`
    - `true`
    - `true`
    - `42.0`
    - `9007199254740992.0`
- `named_arguments.au`
  - named arguments on functions, instance methods, and associated methods,
    including an explicit `own` constructor parameter
  - prints:
    - `hello, aura`
    - `7`
- `named_builtin_arguments.au`
  - named arguments on supported builtins like `print(...)` and `range(...)`
  - prints `10`
- `default_arguments.au`
  - shared-borrow default parameter values evaluated freshly on each call
  - prints:
    - `hello world`
    - `hello aura`
    - `6`
    - `12`
- `function_values.au`
  - capture-free named function values in bindings, parameters, fields, and
    `list`, including copy semantics, explicit generic specialization, and a
    statically known indirect call that uses a default and a named argument
  - explicit `def(mut T) -> R` and `def(own T) -> R` contracts through fields
    and `list`
  - prints:
    - `2`
    - `3`
    - `6`
    - `5`
    - `5`
    - `12`
    - `11`
    - `21`
    - `2`
    - `owned`
- `closures.au`
  - contextually typed expression lambdas with a Copy snapshot, a repeatable
    read-only non-Copy capture, and a consuming single-use capture
  - prints:
    - `42`
    - `12`
    - `6`
    - `6`
    - `owned`
- `views.au`
  - shared and mutable local views, returned view provenance, mutable
    reborrowing, inferred final-use release, fixed tuple-place identity, and a
    mutable-repeatable loan closure
  - prints:
    - `Ada`
    - `2`
    - `9`
    - `(10, 21)`
    - `(11, 32)`
- `borrow_parameters.au`
  - free-function bare shared and `mut` parameters with caller-visible mutation
  - prints:
    - `41`
    - `42`
    - `42`
- `copy_field_returns.au`
  - owned `int32` copies returned from shared class input
  - prints:
    - `7`
    - `7`
- `copy_return_selection.au`
  - choosing and forwarding owned Copy values
  - prints `7`
- `pass_keyword.au`
  - the `pass` no-op statement in empty classes and functions
  - prints `0`
- `assertions.au`
  - demonstrates comparison and membership assertions whose failure diagnostics capture both operands
  - default and custom assertion statements on exact boolean conditions
  - demonstrates the successful path without evaluating a failure
  - doubles as an `aura test` module with one ordinary case, two registered
    labeled cases, and per-case setup/teardown output
  - `aura test --format json -k '[unicode]' examples/basics/assertions.au`
    demonstrates schema-versioned output and filtering after registration
  - prints:
    - `checking`
    - `all assertions passed`
- `multiline_expressions.au`
  - splits a function signature, grouped arithmetic, calls, list literals, and
    a dictionary literal across physical lines through `()`, `[]`, and `{}`
  - keeps the existing no-trailing-comma and single-line-string rules
  - prints:
    - `80`
    - `20`
- `len_and_str.au`
  - the `int64` results of `str.len()`, `str.byte_len()`, `list.len()`,
    `dict.len()`, and `set.len()`; `len(value) == value.len()`; Unicode scalar
    length versus UTF-8 byte length; and `str(value)` producing the print
    rendering
  - prints:
    - `2`
    - `6`
    - `2`
    - `[alpha, beta]`
- `tuples.au`
  - fixed tuple values and return types, whole-source unpacking, copy-only
    constant indexing, tuple-target iteration, recursive tuple patterns, and
    same-type recursive `==` and `!=` that retain both operands; ordering
    remains rejected
  - prints:
    - `Aura`
    - `7`
    - `20`
    - `ready:2`
    - `done:3`
    - `3`
    - `true`

### `collections/`

- `comprehensions.au`
  - eager owned list, set, and dictionary comprehensions with filters, nested
    outer-major clauses, target-local scope, and ordinary bare-loop ownership
  - prints:
    - `[1, 4, 9, 16]`
    - `[4, 16]`
    - `{3: 30, 4: 40}`
    - `[11, 12, 21, 22]`
- `slices.au`
  - owned list and Unicode-scalar str slices with every omitted-endpoint
    form, negative endpoints, and source/result independence
  - prints:
    - `[20, 30]`
    - `[10, 20]`
    - `[30, 40]`
    - `[10, 20, 30, 40]`
    - `[10, 20, 30, 40]`
    - `[99, 30]`
    - `🎉`
    - `A🎉`
    - `🎉Z`
    - `A🎉Z`
    - `A🎉Z`
- `list_basics.au`
  - list literals, indexed reads, `list[T]` methods, and indexed mutation through `set(...)`
  - prints:
    - `3`
    - `1`
    - `2`
    - `2`
    - `20`
    - `1`
    - `99`
    - `false`
- `list_iteration.au`
  - empty-list construction with `list[T]()`, `extend(...)`, explicit `list[T]`
    annotations, bare shared iteration, and consuming `own` iteration
  - prints:
    - `Ada`
    - `Grace`
    - `2`
    - `9`
- `list_polish.au`
  - negative direct/method indexes, non-copy cloned reads, `mut`
    iteration, cast-free `list.len()` use with `range(...)`, `insert(...)`,
    `swap(...)`, `reverse()`, `extend(...)`,
    `clear()`, and list equality
  - prints:
    - `Ada`
    - `Grace`
    - `true`
    - `false`
    - `4`
    - `1`
    - `14`
    - `13`
    - `12`
    - `11`
    - `true`
    - `100`
    - `true`
    - `true`
- `list_algorithms.au`
  - eager shared `map`/`filter`, stable natural sorting, stable once-per-element
    key sorting, and source retention
  - prints:
    - `[6, 2, 4, 8]`
    - `[2, 4]`
    - `[1, 2, 3, 4]`
    - `[4, 3, 2, 1]`
    - `second`
    - `first`
    - `third`
    - `[3, 1, 2, 4]`
- `dict_basics.au`
  - `dict[K, V]` literals, `update(...)`, tuple-valued `items()`, indexed writes, indexed reads, and typed optional lookup and removal
  - prints:
    - `3`
    - `true`
    - `1`
    - `1`
    - `5`
    - `aura`
    - `3`
    - `3`
    - `3`
    - `3`
    - `true`
- `set_basics.au`
  - `set[T]` literals, shared-borrow iteration, deduplication, and the maintained set method surface
  - prints:
    - `3`
    - `true`
    - `false`
    - `true`
    - `true`
    - `9`
    - `true`
    - `true`
    - `1`

### `classes/`

- `point_distance.au`
  - class fields, member access, functions, and `float64.sqrt()`
  - prints `5.0`
- `default_fields.au`
  - field default values and keyword construction
  - prints:
    - `localhost`
    - `8080`
- `methods.au`
  - shared `self` methods, the explicit `self` synonym, and associated methods
  - prints:
    - `4`
    - `8`
    - `0`
- `mutating_methods.au`
  - `mut self`, field mutation, and compound assignment through `self`
  - prints:
    - `6`
    - `1`
- `copy_class.au`
  - `copy class` for explicit copy semantics on fully copyable fields
  - prints:
    - `1`
    - `2`
- `indirect_recursive.au`
  - recursive fields with `indirect Node?` and optional children
  - prints `2`
- `positional_constructors.au`
  - positional class constructor arguments with optional trailing named fields
  - prints:
    - `1`
    - `2`
    - `7`
    - `9`

### `agents/`

- `control_plane_foundations.au`
  - typed JSON/TOML metadata, path operations, process-local counters, and structured log/trace events
  - prints the artifact path, deterministic JSON, TOML validity, and counter value
- `retry_with_backoff.au`
  - `control.retry` with an immediate first attempt, zero-delay retries,
    eventual success, and exact final-error exhaustion
  - prints:
    - `42`
    - `attempt 2`
    - `3`
    - `2`
- `retrying_network_worker.au`
  - application-level HTTP retry policy over a loopback server: retry only
    `503`, preserve terminal `429`, and return the final `503` without drawing
    jitter or sleeping after the attempt budget is exhausted
  - uses `random.Rng(42)`, exponential `Duration` backoff with deterministic
    jitter, explicit five-second network/task deadlines, and scoped
    `TaskGroup`, worker-owned listener, exchange, and response resources; the
    live listener stays on its owning task while a `Queue[str]` carries its
    transferable bound address to the client task
  - the maintained CLI regression runs the example through both the MIR and
    forced-direct backends and pins seven real loopback requests
  - prints:
    - `recover request 1`
    - `recover retry 4ms`
    - `recover request 2`
    - `recover result 200`
    - `rate request 1`
    - `rate retry 6ms`
    - `rate request 2`
    - `rate result 429`
    - `exhaust request 1`
    - `exhaust retry 3ms`
    - `exhaust request 2`
    - `exhaust retry 5ms`
    - `exhaust request 3`
    - `exhaust result 503`
    - `requests 7`

### `json/`

- `dynamic_values.au`
  - parses a recursive `json.Value`, uses an exact scalar accessor, constructs
    a mixed Object/Array tree, and dumps sorted compact and two-space-indented
    JSON

### `bytes/`

- `codecs_and_hashing.au`
  - converts str to strict UTF-8 bytes and back, encodes and decodes binary
    data with canonical base64, renders lowercase hex, computes raw SHA-256,
    and demonstrates that shared inputs remain reusable
  - prints:
    - `4175726120f09f8c8c`
    - `Aura 🌌`
    - `AAH+/w==`
    - `0001feff`
    - `ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad`
    - `4175726120f09f8c8c`
    - `[0, 1, 254, 255]`

### `control_flow/`

- `boolean_logic.au`
  - boolean operators `and`, `or`, and `not`
  - prints:
    - `ready`
    - `true`
- `if_elif_else.au`
  - boolean conditions and branching
  - prints `high`
- `conditional_expressions.au`
  - Python-style `value if condition else alternative` selection, including
    right-associated nesting
  - prints:
    - `ready`
    - `high`
    - `mid`
    - `low`
- `membership_and_chains.au`
  - `in` and `not in` over `list`, `set`, `dict` keys, and `str` substrings, plus a chained comparison bound check
  - prints:
    - `true`
    - `true`
    - `true`
    - `true`
    - `true`
    - `true`
    - `false`
- `enumerate_and_zip.au`
  - `for` over `enumerate(...)` and `zip(...)`, where `zip` stops at the shorter sequence
  - prints:
    - `0: alpha`
    - `1: beta`
    - `alpha:80`
    - `beta:443`
    - `3`
- `for_range.au`
  - `for` loops over `range(...)`, plus `break` and `continue`
  - prints `7`
  - current bootstrap note: `range(...)` bounds must fit the signed index space used by the compiler/runtime
- `match_literals.au`
  - statement-form `match` over literal `bool`, integer, and `str` cases
  - prints:
    - `negative`
    - `zero`
    - `many`
    - `yes`
    - `no`
    - `repo`
    - `other`
- `while_break_continue.au`
  - loops, `break`, `continue`, and compound assignment
  - prints `ok`

### `enums/`

- `result_match.au`
  - enum declarations, owned payload variants, an explicit `own` parameter,
    and exhaustive consuming `match`
  - prints:
    - `42`
    - `bad`
    - `0`
- `result_option.au`
  - built-in `Result[T, E]` and `Option[T]` values with exhaustive `match`
  - prints:
    - `4`
    - `division by zero`
    - `7`
- `explicit_type_args.au`
  - explicit type arguments on built-in enum constructors like `Result[int32, str].Ok(...)`
  - prints:
    - `7`
    - `bad`
- `constructor_ergonomics.au`
  - keyword payload arguments on enum variants plus bare `Ok(...)` and `Some(...)` constructors with expected types
  - prints:
    - `Status.Count(4)`
    - `7`
    - `9`
- `match_borrow.au`
  - `match ...:` plus unqualified built-in enum variants like `case Ok(value):`
  - prints `ok`
- `match_borrow_mut_fields.au`
  - mutable matching through a field place while a proven-disjoint sibling field changes
  - prints:
    - `9`
    - `11`
- `rich_match.au`
  - multi-payload enum variants, nested patterns, named payload fields, and expression-form `match`
  - prints:
    - `7`
    - `30`
    - `0`
- `match_expression_positions.au`
  - expression-form `match` in binding and argument positions, including nested block-form arm values
  - prints:
    - `1`
    - `10`
    - `3`
    - `20`
- `match_guards_and_or_patterns.au`
  - exact-Boolean guards, binding-compatible enum or-patterns, and top-level
    complete-value bindings
  - prints:
    - `positive`
    - `non-positive`
    - `missing`
    - `4`
- `wildcard_match.au`
  - wildcard `case _:` arms in statement-form `match`
  - prints `2`

### `generics/`

- `box_and_wrapper.au`
  - user-defined generic classes, enums, and an `own T` identity function
  - prints:
    - `7`
    - `ok`
- `generic_method_calls.au`
  - method calls on generic class instances inside an `own` generic function
  - prints `7`
- `generic_constructor_specialization.au`
  - explicit type arguments on class and queue constructors such as `Box[int32](...)`
  - prints `42`
- `bounded_types.au`
  - trait bounds on generic class and enum type parameters
  - prints:
    - `aura`
    - `empty`
- `clone_safety_obligations.au`
  - inferred clone-safety obligations on a generic clone helper and a
    generic-to-generic forwarding helper
  - prints `[1, 2, 3]` twice

### `traits/`

- `greeter.au`
  - trait declarations, `impl Trait for Type`, and bounded generic calls
  - prints:
    - `hello aura`
    - `hello aura`
- `generic_dispatch_multiple_types.au`
  - bounded generic trait dispatch across multiple concrete implementors
  - prints:
    - `dog`
    - `cat`
- `generic_trait_bounds.au`
  - generic trait bounds such as `T: Mapper[int32]` with an owned mapper input
  - prints `20`
- `multiple_bounds.au`
  - bounded generic calls with `T: A + B`
  - prints `9`
- `supertraits.au`
  - supertrait declarations, inherited bounds, and default methods that call through a parent trait
  - prints:
    - `name=aura`
    - `aura`
- `self_parameters.au`
  - trait methods that use `Self` in parameter and return positions
  - prints `9`
- `marker_trait.au`
  - empty marker traits declared with `pass`
  - prints `1`
- `builtin_target_traits.au`
  - trait implementations for builtin targets such as `list[int32]` and `str`, using method names that do not collide with a builtin member
  - prints:
    - `list of 2`
    - `text of 5`
- `specialized_generic_impl.au`
  - specialized trait impls for concrete generic instances
  - prints `hello`
- `specialized_trait_dispatch.au`
  - bounded generic dispatch over specialized generic trait impls
  - prints:
    - `7`
    - `hi`
- `generic_trait_impl.au`
  - generic trait declarations plus generic impl headers with `own T` inputs
  - prints `11`
- `default_trait_methods.au`
  - default trait method bodies with per-impl overrides
  - prints:
    - `name=aura`
    - `team=infra`
- `operator_traits.au`
  - operator traits for `+` and unary `-` through `Add[...]` and `Neg[...]`
  - prints:
    - `6`
    - `8`
    - `-6`
    - `-8`
- `ordering_traits.au`
  - ordering traits with shared right-hand operands and an explicitly consuming
    generic selector
  - prints:
    - `true`
    - `true`
    - `true`
    - `true`
    - `2`
- `trait_associated_factory.au`
  - associated trait methods called through the implementing type name
  - prints `7`
- `clone_safety_contract.au`
  - an inferred clone-safety contract from a generic trait default method,
    preserved through a bounded generic call
  - prints `[4, 5]` twice

### `modules/`

- `constants.au`
  - inferred, annotated, public, and source-ordered module constants beside a local `main`
  - prints:
    - `planner`
    - `5`
- `simple_import.au`
  - local file modules with `import ...`, `from ... import ...`, and `public` module boundaries
  - prints:
    - `10`
    - `2`
- `import_aliases.au`
  - local module and from-import aliases with canonical target identity
  - prints:
    - `10`
    - `2`
- `function_values.au`
  - stores a namespace-qualified imported function, then calls it directly and
    through a `def(int32) -> int32` parameter
  - contextually specializes an imported zero-argument generic function and
    uses the result as a `TaskGroup.start` target
  - prints:
    - `10`
    - `12`
    - `none`
- `namespace_import_types.au`
  - namespace-qualified class construction, enum variants, and qualified `match` arms through `import ...`
  - prints:
    - `4`
    - `true`
    - `1`
- `trait_impl_imports.au`
  - trait impls imported across package modules, including bounded generic calls and direct trait-method use
  - prints:
    - `Ada`
    - `Ada`

Helper modules under `modules/pkg/` support the maintained module examples above and are not standalone entrypoints.

### `packages/`

- `local_path_dependencies/app/`
  - `Aura.toml`, `src/`, a sibling path dependency, and a package-local helper module
  - run it with:
    - `cargo run -p aura -- run examples/packages/local_path_dependencies/app/src/main.au`
  - prints `12`
- `workspace/app/`
  - a workspace-root `Aura.toml`, member packages, and a sibling path dependency resolved through the member package manifest
  - run it with:
    - `cargo run -p aura -- run examples/packages/workspace/app/src/main.au`
  - prints `8`
- `ffi_getpid/`
  - an FFI v0 package with `[package] allow_ffi = true` and a bodyless
    `extern "C" def getpid() -> int32` declaration
  - run it on a Unix-family host through both maintained backends:
    - `cargo run -p aura -- run --backend mir examples/packages/ffi_getpid/src/main.au`
    - `cargo run -p aura -- run --backend direct examples/packages/ffi_getpid/src/main.au`
  - both runs print `true`

Package examples use complete package trees with manifest-rooted entrypoints.
Their committed `Aura.lock` files, where dependency resolution creates one,
are part of the maintained surface. FFI examples additionally pin their
explicit unsafe package authorization.

`Aura.toml` also supports git dependencies with `git`, `rev`, `tag`, or
`branch`, defaulting to `main`. Compiler, CLI, and language-server regressions
exercise them through cached git checkouts, which cannot be represented by a
static in-repo package tree.

### `error_handling/`

- `try_result.au`
  - `try expr` over `Result[T, E]` with propagated errors
  - prints:
    - `6`
    - `division by zero`

### `resources/`

- `with_resource.au`
  - deterministic cleanup with `with` and `close(mut self)`
  - prints:
    - `demo`
    - `closed demo`
    - `done`

### `io/`

- `read_text_file.au`
  - builtin `fs.exists(...)` and `fs.read_to_string(...)` through the maintained file I/O surface
  - prints:
    - `true`
    - `true`
- `bytes_file_io.au`
  - binary file helpers plus `fs.File.read_bytes()` / `write_bytes(...)`
  - prints:
    - `4`
    - `65`
    - `67`
    - `5`
    - `68`
- `process_run.au`
  - shell-free `process.run(..., group=true)`, UTF-8/raw captured stdout/stderr, and `process.Completed.check()`
  - prints:
    - `aura process`
    - `15`
    - `0`
    - `ExitStatus.Exited(0)`
- `process_pipes.au`
  - interactive `process.start(..., group=true)`, `process.Pipe`, and timeout-aware child waiting
  - prints:
    - `ping`
    - `ExitStatus.Exited(0)`
- `process_supervisor.au`
  - `process.supervisor()`, restart policies, restart backoff, and group-aware supervisor shutdown
  - prints:
    - `Option.Some(SupervisorEvent.Restarted(flaky, ExitStatus.Exited(1), 1))`
    - `Option.Some(SupervisorEvent.Exited(flaky, ExitStatus.Exited(1), 1))`
    - `true`
    - `false`
    - `true`
- `tcp_echo.au`
  - builtin `net.listen(...)`, `net.connect(...)`, `TcpListener.accept()`, `TcpStream.read_line()`, `TcpStream.write_all(...)`, and `with` cleanup on network resources
  - prints `echo:ping`
- `tcp_bytes.au`
  - timeout-aware TCP byte reads and writes through `connect_timeout(...)`, `read_exact(...)`, `read_bytes(...)`, and `write_bytes(...)`
  - prints:
    - `4`
    - `116`
- `udp_echo.au`
  - UDP binding, datagram receive/send, and `net.UdpDatagram`
  - prints:
    - `udp:ping`
    - `ping`
- `http_roundtrip.au`
  - maintained HTTP listener/request helpers on the shared evented runtime scheduler plus `HttpExchange` and `HttpResponse`
  - prints:
    - `200`
    - `POST:/hello:body:ok`
- `websocket_roundtrip.au`
  - timeout-aware WebSocket listener/connect helpers on the nonblocking socket runtime
  - prints `ws:hi`
- `unix_tls_roundtrip.au`
  - Unix-only Unix-socket and TLS roundtrip example with an embedded self-signed certificate
  - prints:
    - `unix:ping`
    - `9`

### `concurrency/`

The concurrency examples use only structurally `Transfer` task captures,
results, and Queue payloads. Queue handles are copy values. Task handles are
copyable for repeatable results (copy values, Queue handles, and recursively
repeatable Task handles); observing any other transferable result consumes its
single task-result right on the first attempt.

- `task_group_start.au`
  - structured task startup with `TaskGroup.start(...)`, `Queue[T]().get_or_none()`, and `Task.result_or(...)`
  - the no-timeout `get_or_none()` / `result_or(...)` helpers act as immediate non-blocking checks
  - prints:
    - `2`
    - `4`
    - `6`
- `queue_iteration.au`
  - bare `for value in jobs:` receive iteration, where every item arrives
    owned, until close
  - prints:
    - `1`
    - `2`
- `queue_timeout.au`
  - `Queue.get_or(default, timeout=...)` for the ordinary timeout case
  - prints `timeout`
- `bounded_queue.au`
  - `Queue[T](capacity=...)` and `Queue.put(...)` waiting for bounded-capacity
    space on the pinned-worker scheduler
  - prints:
    - `queued 1`
    - `queued 2`
    - `3`
- `send_result.au`
  - `Queue.put()` returning `Result[None, SendError[T]]`, including `Closed(...)`, `Cancelled(...)`, `TimedOut(...)`, and `Full(...)`
  - prints `7`
- `task_group_start_soon.au`
  - structured background work with `TaskGroup.start_soon(...)`
  - prints `9`
- `task_group_associated_method.au`
  - starting associated methods without `self` through `TaskGroup.start(...)`
  - prints:
    - `5`
    - `7`
- `queue_put_timeout.au`
  - `Queue.put(timeout=...)` with explicit send failure handling
  - prints:
    - `sent`
    - `4`
- `task_group_queue_sum.au`
  - queue-driven coordination inside a `TaskGroup()` scope
  - prints `3`
- `task_group_cancel.au`
  - cooperative cancellation with `group.cancel()` and `cancelled()`
  - prints:
    - `0`
    - `1`
- `yield_now.au`
  - explicit cooperative scheduling between bounded CPU-work chunks; ordinary
    loop backedges also receive compiler-inserted amortized scheduling checks
  - prints three numbered steps for each of `alpha` and `beta`; their exact
    interleaving is intentionally unspecified
- `typed_select.au`
  - typed heterogeneous selection over Queue, Task, and relative-Duration
    sources, including deterministic lowest-index priority
  - prints:
    - `SelectOutcome.Queue(0, QueueReceive.Item(queued))`
    - `TaskResult.Ready(42)`
    - `SelectOutcome.Task(0, TaskResult.Ready(42))`
    - `SelectOutcome.Deadline(0)`
- `task_group_wait_helpers.au`
  - consuming `own` outcome helpers for `TaskResult[T]`, `WaitAny[T]`,
    `WaitAll[T]`, and bounded `Queue[T]` coordination
  - prints:
    - `side-effect`
    - `11`
    - `1`
    - `3`
    - `2`
- `queue_get_timeout.au`
  - short timeout handling through `Queue.get_or_none(timeout=...)`
  - prints `Option.None`
- `queue_get_timeout_named.au`
  - named timeout arguments on `Queue.get_or_none(timeout=...)`
  - prints `Option.None`
- `task_group_wait_helpers.au`
  - `wait_any(...)`, `wait_all(...)`, `Task.result(timeout=...)`, and bounded queue send/receive outcomes
  - prints:
    - `Ok(None)`
    - `Err(Full(2))`
    - `1`
    - `closed`
    - `ready`
    - `1`
    - `6`
    - `8`
    - `6`
- `sleep_builtin.au`
  - blocking sleep with a `Duration` argument
  - prints:
    - `start`
    - `end`
- `minute_duration.au`
  - duration literals with the `m` suffix
  - prints `120000ms`
- `duration_arithmetic.au`
  - signed Duration constructors, runtime `int64` scaling, floor division,
    comparison, and floating unit conversion
  - prints `375ms`, `0.333333ms`, `2500ms`, `true`, `2000.0`, and `1.5`

### `randomness/`

- `deterministic_rng.au`
  - a seeded `random.Rng`, unbiased half-open integer calls, and in-place
    Fisher-Yates shuffle
  - prints:
    - `2`
    - `2`
    - `[3, 5, 4, 1, 2, 0]`

### `numbers/`

- `bit_packing.au`
  - hexadecimal and binary literals, fixed-width masks and shifts, wrapping
    and saturating shift modes, power, ties-to-even `round`, and `divmod`
  - prints:
    - `16744448`
    - `255`
    - `128`
    - `0`
    - `0`
    - `255`
    - `64`
    - `64`
    - `81`
    - `2`
    - `4`
    - `-4`
    - `3`
- `float_sqrt.au`
  - `float64` values and `.sqrt()`
  - prints `9.0`
- `float32_values.au`
  - `float32` values introduced through annotated bindings, parameters, returns, and class fields
  - prints:
    - `3.25`
    - `2.0`
    - `5.0`
- `numeric_casts.au`
  - explicit numeric conversions with `expr as Type`
  - prints:
    - `7`
    - `3.0`
    - `1.25`
    - `2.0`
- `numeric_builtins.au`
  - builtin numeric helpers `abs(...)`, `min(...)`, `max(...)`, plus `sqrt(...)` and `float64.sqrt()`
  - prints:
    - `7`
    - `3.5`
    - `2`
    - `12`
    - `9.0`
    - `9.0`
- `scalar_math.au`
  - exact `float64` constants plus scalar rounding, power, exponential,
    logarithmic, and trigonometric functions from the `math` module
  - prints:
    - `3.141592653589793`
    - `2.718281828459045`
    - `inf`
    - `NaN`
    - `-2`
    - `-1`
    - `-1`
    - `0.125`
    - `1.0`
    - `0.0`
    - `3.0`
    - `3.0`
    - `0.0`
    - `1.0`
    - `0.0`
- `numeric_arrays.au`
  - global contiguous row-major `Array[T]` over the four maintained dtypes
  - construction, multidimensional indexing, mutable replacement,
    first-axis owned slices, mapping, reductions, scalar kernels, and explicit
    wrapping/saturating integer arithmetic
  - prints:
    - `2`
    - `[1, 2, 3, 4]`
    - `[2, 2]`
    - `20`
    - `46`
    - `25.0`
    - `-2147483648`
    - `2147483647`
    - `16.0`
- `uint128_values.au`
  - full-range `uint128` literals and arithmetic through the current runtimes and direct backend
  - prints:
    - `340282366920938463463374607431768211455`
    - `340282366920938463463374607431768211455`
- `unary_minus.au`
  - unary minus for integer and floating-point expressions
  - prints:
    - `-5`
    - `-3.5`
    - `2`

### `strings/`

- `greeting.au`
  - string concatenation and equality
  - prints `hello, aura`
- `string_clone.au`
  - `str.clone()` on owned strings
  - prints `aura`
- `string_methods.au`
  - single-quoted strings, an owned `Option[str]` match helper, and the
    maintained `str` method surface: `int64` Unicode-scalar `len()`,
    `int64` UTF-8 `byte_len()`, `contains(...)`, `starts_with(...)`,
    `ends_with(...)`, `split(...)`, `replace(...)`, `to_lower()`,
    `to_upper()`, `strip_prefix(...)`, `strip_suffix(...)`, `trim()`, and
    `clone()`
  - prints:
    - `15`
    - `true`
    - `true`
    - `true`
    - `aura repo`
    - `2`
    - `aura`
    - `repo`
    - `aura lang`
    - `aura repo`
    - `AURA REPO`
    - `repo`
    - `none`
    - `aura`
    - `none`
    - `11`
- `string_parsing_and_formatting.au`
  - parsing builtins, scalar and boolean `.to_string()`, and `str.join(...)`
  - prints:
    - `42`
    - `-9000000000`
    - `3.5`
    - `true`
    - `aura-lang-tests`
    - `true`
    - `12`
    - `4`
    - `9`
    - `3.0`
- `borrow_str.au`
  - borrowed string parameters with `str`
  - prints `Hello, Aura`
- `f_strings.au`
  - interpolated `f"..."` strings producing owned `str` values
  - prints `Hello, Aura 42`
- `literal_forms_and_formatting.au`
  - exact triple-quoted prompts, raw backslash-heavy paths, and statically
    checked width, sign-aware zero padding, grouping, and percentage formatting
  - demonstrates Unicode-aware formatting through the maintained f-string
    grammar

## Stable Bootstrap Examples

The original top-level files are still present as stable bootstrap references:

- `point.au`
- `basic_addition.au`
- `top_level_addition.au`
- `control_flow.au`
- `simple_addition.au`

## Run Examples

From the repo root:

```bash
cargo run -p aura -- run examples/basics/top_level_script.au
cargo run -p aura -- run examples/basics/named_arguments.au
cargo run -p aura -- run examples/basics/named_builtin_arguments.au
cargo run -p aura -- run examples/basics/default_arguments.au
cargo run -p aura -- run examples/basics/function_values.au
cargo run -p aura -- run examples/basics/borrow_parameters.au
cargo run -p aura -- run examples/basics/numbers.au
cargo run -p aura -- run examples/basics/pass_keyword.au
cargo run -p aura -- run examples/collections/list_basics.au
cargo run -p aura -- run examples/collections/list_iteration.au
cargo run -p aura -- run examples/collections/list_polish.au
cargo run -p aura -- run examples/collections/slices.au
cargo run -p aura -- run examples/collections/dict_basics.au
cargo run -p aura -- run examples/collections/set_basics.au
cargo run -p aura -- run examples/classes/point_distance.au
cargo run -p aura -- run examples/classes/methods.au
cargo run -p aura -- run examples/classes/mutating_methods.au
cargo run -p aura -- run examples/control_flow/for_range.au
cargo run -p aura -- run examples/control_flow/match_literals.au
cargo run -p aura -- run examples/control_flow/boolean_logic.au
cargo run -p aura -- run examples/control_flow/conditional_expressions.au
cargo run -p aura -- run examples/control_flow/membership_and_chains.au
cargo run -p aura -- run examples/control_flow/enumerate_and_zip.au
cargo run -p aura -- run examples/basics/len_and_str.au
cargo run -p aura -- run examples/control_flow/while_break_continue.au
cargo run -p aura -- run examples/enums/result_match.au
cargo run -p aura -- run examples/enums/result_option.au
cargo run -p aura -- run examples/enums/wildcard_match.au
cargo run -p aura -- run examples/generics/box_and_wrapper.au
cargo run -p aura -- run examples/generics/generic_method_calls.au
cargo run -p aura -- run examples/generics/bounded_types.au
cargo run -p aura -- run examples/generics/clone_safety_obligations.au
cargo run -p aura -- run examples/traits/greeter.au
cargo run -p aura -- run examples/traits/multiple_bounds.au
cargo run -p aura -- run examples/traits/marker_trait.au
cargo run -p aura -- run examples/traits/specialized_generic_impl.au
cargo run -p aura -- run examples/traits/clone_safety_contract.au
cargo run -p aura -- run examples/traits/builtin_target_traits.au
cargo run -p aura -- run examples/error_handling/try_result.au
cargo run -p aura -- run examples/resources/with_resource.au
cargo run -p aura -- run examples/io/read_text_file.au
cargo run -p aura -- run examples/io/bytes_file_io.au
cargo run -p aura -- run examples/io/process_run.au
cargo run -p aura -- run examples/io/process_pipes.au
cargo run -p aura -- run examples/io/process_supervisor.au
cargo run -p aura -- run examples/io/tcp_echo.au
cargo run -p aura -- run examples/io/tcp_bytes.au
cargo run -p aura -- run examples/io/udp_echo.au
cargo run -p aura -- run examples/io/http_roundtrip.au
cargo run -p aura -- run examples/io/websocket_roundtrip.au
cargo run -p aura -- run examples/io/unix_tls_roundtrip.au
cargo run -p aura -- run examples/concurrency/task_group_start.au
cargo run -p aura -- run examples/concurrency/bounded_queue.au
cargo run -p aura -- run examples/concurrency/send_result.au
cargo run -p aura -- run examples/concurrency/task_group_start_soon.au
cargo run -p aura -- run examples/concurrency/queue_put_timeout.au
cargo run -p aura -- run examples/concurrency/task_group_queue_sum.au
cargo run -p aura -- run examples/concurrency/task_group_cancel.au
cargo run -p aura -- run examples/concurrency/yield_now.au
cargo run -p aura -- run examples/concurrency/typed_select.au
cargo run -p aura -- run examples/concurrency/queue_timeout.au
cargo run -p aura -- run examples/concurrency/queue_get_timeout.au
cargo run -p aura -- run examples/concurrency/queue_get_timeout_named.au
cargo run -p aura -- run examples/concurrency/sleep_builtin.au
cargo run -p aura -- run examples/concurrency/minute_duration.au
cargo run -p aura -- run examples/concurrency/duration_arithmetic.au
cargo run -p aura -- run examples/numbers/bit_packing.au
cargo run -p aura -- run examples/numbers/float32_values.au
cargo run -p aura -- run examples/numbers/numeric_casts.au
cargo run -p aura -- run examples/numbers/numeric_builtins.au
cargo run -p aura -- run examples/numbers/scalar_math.au
cargo run -p aura -- run examples/numbers/numeric_arrays.au
cargo run -p aura -- run examples/numbers/unary_minus.au
cargo run -p aura -- run examples/strings/string_clone.au
cargo run -p aura -- run examples/strings/string_methods.au
cargo run -p aura -- run examples/strings/string_parsing_and_formatting.au
cargo run -p aura -- run examples/strings/literal_forms_and_formatting.au
```

## Build Standalone Artifacts

The CLI can also package a runnable standalone native binary for a checked program:

```bash
cargo run -p aura -- build -o ./target/aura-point examples/point.au
./target/aura-point
cargo run -p aura -- build --backend direct -o ./target/aura-direct examples/basic_addition.au
./target/aura-direct
```

`aura build` supports:

- `--backend auto`
  - default
  - uses the direct native backend for the maintained Aura surface
- `--backend direct`
  - forces the current direct native backend
  - covers the full currently implemented Aura language surface

The built binary does not depend on the original `.au` source file at runtime, but the build step still needs Cargo/Rust and a host C compiler.

## Run Programs

The maintained public execution path is `run`, which executes through the MIR runtime:

```bash
cargo run -p aura -- run examples/classes/point_distance.au
cargo run -p aura -- run examples/classes/methods.au
cargo run -p aura -- run examples/enums/result_match.au
```

## Check, AST, and MIR

```bash
cargo run -p aura -- check examples/classes/default_fields.au
cargo run -p aura -- ast examples/classes/point_distance.au
cargo run -p aura -- mir examples/control_flow/while_break_continue.au
cargo run -p aura -- mir examples/enums/result_match.au
cargo run -p aura -- mir examples/error_handling/try_result.au
```

## Maintenance

The categorized examples are part of the supported development workflow.

When the implemented language subset changes:

1. update the relevant example
2. update the matching tutorial chapter
3. keep the example set runnable under `cargo test`

The single-print `basics/hello_world.au` is also the executable-size baseline.
