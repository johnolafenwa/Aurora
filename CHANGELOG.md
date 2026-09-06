# Changelog

All notable user-facing changes are recorded here. Aura follows semantic
versioning for release artifacts while it remains a technical preview; a minor
preview release may still contain source and API incompatibilities called out
in this file.

## 0.3.4 — Unreleased (technical preview)

- Enable Cranelift speed optimization, validated by the full forced-backend gate.
- Tune release artifacts with fat LTO, one codegen unit, stripped compiler
  symbols, platform section collection and user debug/local-symbol stripping;
  preserve unwinding and Aura source diagnostics. Size measurements pending.
- Add pinned Rust baseline programs and lanes to all three timing harnesses,
  protocol smoke checks, and clean-ref executable-size tooling. Timings deferred.
- Document the native semantic boundary and incremental Batch 1 builder plan.
- Correct the documented compiler coverage floors and `aura test` lifecycle and
  parameterized-registration support.

## VS Code extension 0.3.4 — 2026-09-06

- Added syntax highlighting and snippets for shared and mutable views,
  returned-view contracts, and explicit shared/mutable/owned closure captures.
- Updated the bundled language server to semantic interface version 6 with
  view-aware diagnostics, completion, hover, go-to-definition, and capture
  provenance.

## 0.3.3 — 2026-09-06 (technical preview)

Aura 0.3.3 is a technical preview feature release of the compiler,
command-line tools, language server, VS Code extension, Manual, Learn track,
tutorials, examples, and installation tooling.

- Implemented ADR-0038 place-based loans and views. `view name = place` and
  `view mut name = place` grant shared or exclusive non-owning access to fixed local,
  parameter, receiver, class-field, and tuple places without copying or moving
  the owner.
- Added inferred last-use lifetimes, reborrowing with parent suspension and
  resumption, immediate mutable write-through, and deterministic cleanup on
  ordinary, early-return, branch, match, and managed-resource exits.
- Added returned-view contracts with `view ... from source`, including methods,
  generic trait dispatch, forwarded calls, and exact runtime handoff of the
  selected fixed projection from one declared origin.
- Added explicit closure loan captures (`[value]`, `[mut value]`, and
  `[own value]`) with repeatability, escape, storage, ownership, and Transfer
  checks that preserve the source place and release each loan exactly once.
- Added stable `AU3010` provenance, overlap, escape, and lifetime diagnostics;
  compiler analysis and editor tooling expose the same place and capture
  information.
- Added explicit MIR loan operations, public serialized-MIR validation, and
  direct-native lowering with forced MIR/direct parity across local, returned,
  reborrowed, and closure-held views. Validation bounds adversarial projection
  expansion before allocation.
- Ratified ADR-0045's binding-test and assertion-introspection checkpoint, and
  formally recorded ADR-0049's positional and named class-pattern deferment.
- Published VS Code extension 0.3.4 with the matching syntax, snippets, and
  compiler-backed semantic behavior.

## VS Code extension 0.3.3 — 2026-08-04

- Added builtin-function highlighting for calls such as `print(...)`,
  `range(...)`, and `len(...)`.
- Preserved member completion inside nested calls and in buffers containing an
  unrelated compiler diagnostic.
- Fixed extension shutdown by registering the language client itself as the
  disposable after asynchronous startup.

## 0.3.2 — 2026-08-04 (technical preview)

Aura 0.3.2 is a technical preview patch release of the compiler, command-line
tools, language server, VS Code extension, Manual, Learn track, tutorials, and
installation tooling.

- Fixed MIR and direct execution of module constants that call list
  algorithms such as `map`, `filter`, and `copy`.
- Fixed module-constant list, set, and dictionary comprehensions by retaining
  their checked closure and comprehension metadata through lowering.
- Added `aura upgrade`, which downloads the maintained installer and upgrades
  the compiler and bundled native runtime in the active install prefix.
- Corrected tutorial ownership, equality, error-conversion, queue-send,
  receiver, networking, numeric-cast, and current-surface guidance.
- Classified and compiler-checked every Aura tutorial fence through the new
  tutorial integrity gate.
- Published VS Code extension 0.3.3 with builtin highlighting, resilient list
  member completion, and correct language-client disposal.

## VS Code extension 0.3.2 — 2026-08-04

- Fixed VS Code and documentation highlighting for f-string interpolations.
  Interpolation braces now receive the standard format-placeholder scope, and
  embedded identifiers and operators are highlighted as Aura expressions
  instead of inheriting the surrounding string color.

## 0.3.1 — 2026-08-04 (technical preview)

Aura 0.3.1 is a technical preview patch release of the compiler, command-line
tools, language server, VS Code extension, Manual, Learn track, tutorials, and
installation tooling.

- Fixed plain and compound reassignment of mutable top-level script locals.
  Module constants that read a script local now receive a focused diagnostic
  explaining initialization order and the valid `mut` or `main` repairs.
- Fixed direct-backend integer `==` and `!=` when a function call supplies an
  `int32` or `uint64` operand. Function-result temporaries retain their declared
  type across ordinary comparisons, reversed operands, comparison chains, and
  assertion introspection, with MIR/direct parity coverage.
- Documented the required language-server request field
  `semantic_interface_version: 5` in the Manual and server package.
- Added Aura file icons to the VS Code language contribution using the
  maintained Aura mark.
- Expanded platform installation guidance, native Aura syntax highlighting,
  AI-agent documentation entry points, the Python fast track, conversions,
  testing guidance, and the systems-language positioning for agents and ML
  infrastructure.

## 0.3.0 — 2026-08-03 (technical preview)

Aura 0.3.0 is a technical preview of the current Python-shaped language,
compiler, runtime, command-line tools, language server, VS Code extension,
Manual, Learn track, tutorials, and examples. It combines static typing,
deterministic ownership, native compilation, structured concurrency, and typed
failure for systems software, ML infrastructure, and agent runtimes.

- Unified collection positions on `int64`: ranges, list indices, slice
  endpoints, enumeration positions, Array coordinates, and concurrent
  source-result indices now compose directly with `len(...)`. Fixed-width
  `int8`, `int16`, `int32`, `uint8`, `uint16`, and `uint32` values widen
  losslessly only at these position sites.
- Standardized owned text and collections as `str`, `list[T]`, `dict[K, V]`,
  and `set[T]`. Collection literals are homogeneous and methods use the
  canonical Python-shaped names with typed ownership and failures.
- Expanded `aura test` with function-level discovery, literal case-sensitive
  `-k` filtering, schema-versioned JSON results, per-case setup and teardown,
  parametrized function-value registration, and deterministic source-order
  reporting. Supported comparison and membership assertions now report the
  two typed operand values on failure while preserving once-only left-to-right
  evaluation and lazy assertion messages.
- Added module aliases with `import ... as ...` and per-name aliases in
  `from ... import ...` declarations. Aliases preserve the target module,
  declaration, type, visibility, and constant-storage identity.
- Added decimal separators, hexadecimal, binary, and octal integer literals;
  fixed-width bitwise operators; checked shifts; and explicit wrapping and
  saturating shift methods. Added right-associative power, exact-type
  `round` and `divmod`, and the scalar `math` functions `floor`, `ceil`,
  `trunc`, `pow`, `exp`, `log`, `log2`, `log10`, `sin`, `cos`, and `tan`.
- Added exact-content triple-quoted strings, single-line raw strings, and a
  closed f-string format grammar covering alignment, fill, signs, width,
  precision, grouping, integer bases, fixed-point, scientific, percentage,
  and text formatting.
- Added Boolean match guards and recursive or-patterns with left-to-right
  probing, one guard evaluation, exhaustive binding checks, delayed owned
  extraction, and mutable writeback on every guard exit path.
- Added eager immutable module constants with dependency-first, source-ordered,
  once-only initialization. Copy values read by value, while non-Copy values
  provide shared access to their defining module's stored value.
- Added root binding patterns, sign-aware zero padding, equality-obligation
  checking across collections, and precise diagnostics for unsupported string
  prefixes. Values without defined equality cannot reach hidden identity
  comparison through membership, list search, sets, or dictionary keys.
- Published coordinated `aura-v0.3.0-preview-*` archives, the 0.3.0 VS Code
  extension, static documentation, and a verified `SHA256SUMS` manifest.

## 0.2.0 — 2026-07-31 (technical preview)

Aura 0.2.0 is the first complete distribution of the implemented
language reference: a typed Python-shaped source language, deterministic
ownership model, structured-concurrency runtime, MIR and direct-native
backends, package tooling, language server, VS Code extension, and maintained
Learn and Manual tracks. It is intended for evaluation and controlled
experiments, not production deployment or execution of untrusted code. It was
developed under the working name Aurora before its first publication.

### Ownership surface

- Bare parameters and receivers grant logical shared access for every type,
  including copy types. `mut` grants exclusive mutable access and `own`
  transfers ownership.
- Bare `match` is shared matching. `match mut value` enables mutable matching
  with writeback, and `match own value` consumes the scrutinee and owned
  payloads.
- Return annotations carry types only. Every return is an ordinary owned
  return.
- `str.len()`, `str.byte_len()`, `list.len()`, `dict.len()`, and
  `set.len()` return `int64`. `str.byte_len()` is the UTF-8 byte count;
  `str.len()` is the Unicode-scalar count.
- The maintained builtin surface now reserves `len`, `str`, `select`,
  `SelectOutcome`, and the `control` module namespace. Rename conflicting user
  declarations.
- Lightweight tasks run on pinned cooperative OS workers. Per-producer Queue
  order stays FIFO, but global sibling
  scheduling and output order are unspecified. Programs must synchronize any
  order they observe.
- The native artifact-cache format is `aura-native-cache-v5`.

### Language

- Added fixed structural tuples, recursive unpacking and patterns, recursive
  equality, and whole-source copy/move behavior.
- Added conditional expressions, `in`/`not in`, chained comparisons,
  `enumerate` and `zip` loop forms, and maintained `len` and `str` builtins.
- Added eager owned list, set, and dictionary comprehensions, including nested `for`
  clauses and left-to-right filters. Generator expressions remain rejected
  with guidance to use a comprehension or explicit loop.
- Added owned `list[T]` and Unicode-scalar `str` slicing with omitted
  endpoints and one-time negative normalization. Aura traps invalid or
  reversed ranges with `AU4003`; slice bounds are not clamped. Steps, slice
  assignment, str integer indexing, and views
  remain unavailable.
- Added checked contextual narrow integers, wrapping and saturating integer
  methods, multiline delimiter continuation, assertions, bytes/codecs,
  deterministic seeded randomness, JSON, and the maintained typed
  control-plane modules.
- Kept the Manual reference-frozen: every accepted semantic addition has an
  ADR, executable conformance coverage, examples or tutorials where relevant,
  and MIR/direct parity evidence.

### Runtime and structured concurrency

- Fixed a queue-iteration livelock under oversubscription. Receive iteration
  now subscribes only to producers that are still running; a producer that
  has already completed can no longer keep every consumer in an immediate
  scheduler-ready loop while CPU burners occupy the worker pool. The reported
  iteration-consumer shape is pinned on MIR and direct at the default worker
  count.
- Replaced the periodic scheduler tick with persistent readiness registration,
  heap deadlines, cross-worker notifications, and loop-backedge fairness
  checks. Idle workers block until runnable work, I/O readiness, or a deadline.
- Added guarded 512 KiB default task stacks and explicit bounded stack-size
  overrides, compiler-derived structural `Transfer`, conditional task-handle
  Copy, and statically single-consumer non-repeatable results.
- Added typed heterogeneous `select(...)` across Queue, Task, and Duration
  sources, plus `wait_any`, `wait_all`, cancellation, scheduler-aware Queue
  operations, and structured child cleanup.
- Added a lazily created blocking-I/O pool. `AURA_BLOCKING_WORKERS` selects an
  exact positive worker count;
  `AURA_BLOCKING_QUEUE_CAPACITY` optionally bounds accepted pending work.
  Invalid settings fail before user code with `AU4006` on both backends.
- Expanded the maintained filesystem, process, TCP/UDP, HTTP, WebSocket, Unix
  socket, TLS, JSON, and control-plane surfaces with typed errors, explicit
  limits, timeout policy, cancellation behavior, and deterministic cleanup.
- Made the direct-native artifact cache content-addressed, integrity checked,
  concurrency safe on maintained Unix hosts, and optional for installed
  immutable runtimes. Long cold operations report whether Aura is waiting for
  a concurrent build or building the native program.

### Callables and closures

- Added structural `def(...) -> ...` types and Copy/Transfer named-function
  values, with inference, generic specialization, cross-module use, and both
  backend implementations.
- Added eager callable-powered natural and keyed `list.sort`, plus `map` and `filter`, and
  let `control.retry` accept repeatable capturing closures as well as named
  functions.
- Added contextually typed expression lambdas. Captures are by value: Copy
  values are snapshotted, non-Copy values move at creation, read-only closures
  are repeatable, and a consuming capture makes the closure single-use. A
  closure is Transfer only when each capture is Transfer.
- Callable equality is uniformly rejected for named function values and
  closures. Compare call results or carry an explicit discriminant instead.
- Mutable captured state, capability capture, nested capture of an enclosing
  lambda's bare parameter, and arbitrary persistence of capturing-closure
  metadata remain outside the 0.2 surface. ADR-0038 defines the planned 0.3
  loan/view foundation.

### Foreign function interface

- Added explicitly authorized FFI v0 packages and direct synchronous
  `extern "C"` calls to process-global symbols.
- The supported ABI includes fixed-width scalars, pointer-length str and
  byte views, and non-null opaque handles. Package manifests must opt in and
  dependency authorization is reported exactly from the root package.
- FFI declarations are a native trust boundary. False declarations or
  misbehaving C code can violate Aura's invariants; callbacks, variadics,
  raw pointer values, nullable handles, returned views, retained views, and
  dynamic-library selection remain unsupported. The MIR and direct backends
  use the same validated ABI description and host-call engine.

### Numeric arrays

- Added owned contiguous row-major `Array[T]` for `int32`, `int64`, `float32`,
  and `float64`, with shape metadata and rank of at least one.
- Added `zeros`, `full`, and `from_list`; multidimensional get/set; `fill`;
  first-axis owned copying slices; map; sum/min/max/mean; exact-shape
  elementwise arithmetic; and scalar arithmetic.
- Elementwise and reduction work runs in dtype-specialized contiguous native
  runtime kernels. Release disassembly recorded scalar floating-point kernels.
  The Manual records the baseline-host one-million-element measurements and
  their provenance.
- Arrays intentionally have no views, array-shape broadcasting, mixed-dtype
  promotion, equality, shape transformations, autograd, accelerators, or
  integer division. Use explicit casts and owned copies.

### Tooling and diagnostics

- `aura --version` now prints the preview channel and 12-hex-digit source
  commit (`aura 0.2.0-preview (<commit>)`), so preview executables are
  distinguishable from a future final 0.2.0. Release publication is marked as
  a GitHub prerelease and includes a generated, verified `SHA256SUMS` asset.
- AU3002 diagnostics use the concise wording “shared values” and retain the
  explicit stable code.
- Added `aura run --backend auto|mir|direct`, native builds with relocatable
  runtime/link manifests, a content-addressed native cache, function-level
  `aura test` discovery, recursive formatting/testing, package/workspace
  resolution and lockfiles, and compiler inspection/analysis commands.
- Added complete typed Aura call frames and child-task ancestry to MIR and
  direct runtime failures. Human diagnostics, schema-version-1 JSON, analysis,
  the language server, and the VS Code extension preserve the structured
  records. A private direct-runtime trap channel keeps ordinary process exit
  status distinct from `AU####` runtime failures.
- Added stable dedicated codes for semantic defect classes, exact
  UTF-8/source spans, completion recovery on incomplete programs, hover and
  go-to-definition, and maintained example-file regression coverage.
- Release archives carry the compiler, native runtime, and linker manifest.
  Installed archives can check, run, and build Aura without Cargo or a source
  checkout; a host C compiler is still required for native output.

### Current limits

- Aura 0.2.0 is a technical preview whose compatibility may change. Production
  systems, sandboxing, and untrusted-source security are outside this release's
  scope.
- Release archives support glibc Linux x86-64 and macOS x86-64/Apple silicon.
  Windows, musl Linux, other architectures, and cross-compilation are
  experimental source-build territory.
- Structured concurrency is cooperative and pinned. Preemption, migration,
  work stealing, and detached tasks are unavailable; sibling order is
  unspecified, and speedup depends on the workload.
- Owned copies are the only 0.2 slicing model. ADR-0038's place loans, returned
  views, mutable closure capture, and view-aware concurrency checks target 0.3.
- Arrays are CPU-only and intentionally narrower than NumPy; externs are
  direct-call-only values on both maintained backends; imported modules have no
  runtime initialization side effects; package registries and publishing are
  not implemented.
- Resource caps, protocol boundaries, backend-specific FFI availability, and
  failure guidance are normative in the Manual's
  [Current Limits](docs/manual/current-limits.md) and
  [Status And Compatibility](docs/manual/status-and-compatibility.md) pages.
