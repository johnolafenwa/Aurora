# Current Limits

This page documents known current limits of the Aura compiler and runtime.

## Language

- Identifiers are ASCII; Unicode is supported in string contents, not identifier spelling.
- A physical tab outside a triple-quoted string is rejected. Inside a
  triple-quoted string it is exact content. Use `\t` in an ordinary string.
- Source lists do not accept trailing commas except the required comma in
  singleton tuple values, types, targets, and patterns. Multi-element tuples
  still reject a trailing comma.
- Parser nesting/postfix/binary-chain guards are limited to 128 operations; deeper input is rejected with a diagnostic.
- Integers are fixed-width. Aura has no arbitrary-precision integer, implicit
  width promotion, rotate operator, literal suffix, hexadecimal floating
  literal, or distinct unsigned-right-shift operator. Power is builtin-only;
  `round` has no digit-count overload.
- Non-numeric casts are not implemented.
- Direct recursive fields require `indirect`.
- Ordinary `-> T` return values are always owned. Copy results are ordinary
  copies; a non-copy result must be constructed, cloned when clone-safe, moved
  from owned input, or produced by an owner operation. ADR-0038 separately
  permits one `-> view [mut] T from origin` result tied to a receiver or
  parameter place.
- Empty list, dictionary, and set literals need an expected collection type.
- Class field defaults cannot call user-defined functions in the current compiler. Compute the value before construction and pass it as an explicit field argument.
- `str(...)` is not a constructor; use string literals and string methods.
- Ordinary and triple-quoted strings may use single or double quotes. Raw
  strings are single-line. Raw triple strings, raw f-strings, and byte-string
  literals are not implemented. F-strings remain double-quoted and use static
  format specifications; dynamic width, nested fields, conversion flags,
  locale formatting, and the `g`, `G`, `n`, `c`, `#`, `0`, and `=` forms are
  not implemented.
- `str` has scalar-count `len()`, UTF-8 `byte_len()`, and owned
  Unicode-scalar slicing, but no integer indexing, `chars()`, `ord()`, or
  `chr()`.
- One concatenated or formatted `str` result is limited to 64 MiB. Aura
  preflights the next append and reports `AU4005` without committing an
  oversized partial result.
- `list[uint8]` is the bytes type. UTF-8 conversion is explicit; the reserved `encoding` argument, non-UTF-8 text codecs, byte-string literals, URL-safe or unpadded base64, streaming codecs, incremental hashes, and HMAC are not implemented.
- Physical newlines continue a logical line only while `(`, `[`, or `{`
  remains open. Continuation indentation is visual; delimiter kinds must
  match.
- Backslash continuation is not implemented. Ordinary, raw, and f-strings
  remain single-line; triple-quoted ordinary strings may span physical lines.
- Tuples have fixed structural types, recursive unpack targets and patterns,
  copy-only constant indexing, and non-consuming recursive `==` and `!=` for
  operands of the same static tuple type. There is no empty tuple,
  multi-element trailing tuple comma, tuple iteration or methods, tuple
  ordering, named/rest unpacking, mutable tuple-target writeback,
  dynamic/negative tuple indexing, or tuple-to-collection conversion. Unpack a
  tuple to take ownership of a non-copy element.
- Views have identity only for local/parameter/receiver roots, existing views,
  class-field paths, and fixed tuple positions. Collection indexes and keys,
  set elements, arbitrary temporaries, and escaping enum-payload views are not
  loanable. View-bearing aggregates, module storage, multi-origin results,
  returned loan closures, and lifetime-parameterized structural callable
  types remain unavailable. Views
  and loan closures are always non-Transfer.
- Statement match arms cannot be inline. Expression match arms may use a same-line expression after `case pattern:` or an indented expression body.
- `for` loop bindings cannot shadow names already visible in the same scope.
- Duration literals have only the integral `ms`, `s`, and `m` suffixes; there is no `ns` or fractional Duration literal and no unary `-Duration`. Associated constructors and checked Duration arithmetic provide signed and sub-millisecond results instead.
- Capture-free named functions are copy, `Transfer` values. They may be stored
  and called through `def(T1, mut T2, own T3) -> R` types and used as task
  targets; bare function-type parameters are shared.
  Instance, associated, and trait method values remain unavailable; the task
  API retains its direct associated-method-without-`self` target carve-out.
- Lambdas with parameters require complete expected parameter types; a
  zero-parameter lambda may infer `def() -> R` from its expression body.
  A lambda without a capture list captures by value. An explicit exhaustive
  `[value, mut value, own value]` list creates shared/mutable loans or an owned
  capture. Inline parameter types, defaults, generics, and statement bodies
  remain unavailable. A mutable-loan closure is mutable-repeatable through a
  mutable place; a consuming closure is single-use. Capturing closures cannot
  pass through arbitrary written-`def` parameters, fields, collections, or
  annotated returns because those boundaries describe capture-free code
  pointers. Compiler-known repeatable callback sites preserve closure
  metadata; task start accepts a qualifying closure by move for one call.
  Conditional and `match` expressions cannot merge capturing closures from
  multiple branches because Phase 6.3 has no closure-union type; invoke the
  closure within each branch or use capture-free lambdas or named functions.
- List, set, and dictionary comprehensions are eager and always return fresh owned
  collections. Their clauses use bare-loop iteration only; there is no
  comprehension `mut`/`own` source form, early `break`/`continue`, or lazy
  result. Nested clauses are outer-major and Queue comprehensions receive
  until ordinary Queue iteration ends. Generator expressions
  remain unavailable and report `AU2005` with an eager-comprehension or
  an explicit loop.
- list and str slices accept one contiguous half-open range and return fresh
  owned copies. Written endpoints use `int64`; negatives normalize once,
  and invalid or reversed ranges trap with `AU4003`. Endpoints are not clamped.
  str endpoints count Unicode scalar values and slicing is O(n). Slice
  steps and slice assignment remain reserved `AU2005` forms; arbitrary
  sliceable types, indexed zero-copy views, str integer indexing, grapheme slicing,
  and Python-style endpoint clamping are unavailable. List slicing requires
  clone-safe, repeatably observable elements.
- Numeric `Array[T]` is CPU-only, contiguous, row-major, and specialized only
  by `int32`, `int64`, `float32`, or `float64`. Shape is runtime metadata and
  rank is at least one; zero dimensions are allowed. Same-dtype scalar
  broadcast is implemented, but there is no array-shape broadcasting, mixed
  promotion, equality, views, reshape, transpose, matrix multiplication,
  multidimensional or step slicing, slice assignment, autograd, accelerator
  placement, distributed storage, or foreign-buffer aliasing. First-axis
  slices are fresh owned copies. Maintained NumPy comparisons record exact
  post-reboot workloads and provenance. Shape elements, coordinates, and
  element counts use `int64`; practical Array size is bounded by address space,
  allocation limits, element size, and available memory.
- FFI v0 is package-only and requires `[package] allow_ffi = true`; a root
  package also reports every reachable FFI-enabled dependency under exact
  `[ffi] dependencies`. Calls resolve already-loaded process-global symbols
  and are synchronous on the current worker. The accepted ABI is limited to
  fixed-width scalars, temporary str/byte pointer-length views,
  same-length mutable byte scratch copy-in/out, and non-null opaque handles.
  Empty views use `(NULL, 0)`. There is no library-loading/link-name syntax,
  callback, variadic, raw-pointer arithmetic, returned view, nullable handle,
  foreign aggregate layout, automatic handle destructor, or async offload.
  Process-global lookup is currently supported on Unix-family hosts. A false C
  signature or misbehaving native function remains outside Aura's safety
  guarantees and may terminate or corrupt the process.
- Callable-powered list algorithms are eager. `map` and `filter` return owned
  lists; `filter` requires clone-safe elements.
  Built-in natural sorting covers all integer types, `float32`, `float64`, and
  `Duration`; `str` has no built-in `Ord[str]`. Preserve insertion order, use
  `sort(key=callback)` with an orderable key/index, or define a
  nominal type with an application-specific `Ord` implementation when text
  records require ordering.
  Keyed `sort`, `map`, and `filter` accept only their exact bare/shared callback
  parameter capabilities. There is no comparator-form sort, lazy map/filter,
  parallel traversal, or algorithm callback with mutable/owned element access.
- `TaskGroup.start(...)` and `start_soon(...)` support bare shared and `own`
  target parameters; `mut` targets are rejected because child tasks cannot
  write back through the starting call frame.
- Detached lightweight tasks are not a language form; use `TaskGroup`.
- `for value in mut set:` is not currently supported.

## Runtime

- MIR and native direct-backend traps carry the same typed Aura call frames
  and task ancestry. Call frames are innermost first, task ancestry is youngest
  first, and every frame retains its own defining or spawning source path.
- Human diagnostics synthesize the compact call-chain, task-entry, and
  task-ancestry note lines. Structured schema-version-1 diagnostics expose
  `call_frames` and `task_ancestry` arrays instead; generated frame prose is
  not duplicated in `notes`.
- Aura does not expose host Rust/Cranelift backtraces, debugger stack
  reflection, exception catching, or a standalone-binary JSON switch.
- Aura task code executes on pinned cooperative scheduler workers. The
  default count is the available parallelism reported by the host; the
  `AURA_WORKERS=<positive integer>` override selects an explicit count.
  Assignment happens when a child is spawned and remains stable for its
  lifetime: coroutine stacks never migrate and the runtime does not steal work
  between workers.
- A positive `AURA_WORKERS` value may exceed the host's available-core count.
  Empty, zero, signed, whitespace-padded, nonnumeric, and overflowing values
  are rejected before execution with `AU4006` and
  ``invalid AURA_WORKERS value `<raw>`: expected a positive integer``.
- Scheduling is cooperative, not preemptive. The compiler checks every loop
  backedge and eventually yields from a tight loop, but only to runnable work
  assigned to that task's worker. One long loop body or long straight-line
  computation can still delay siblings pinned to the same worker. The
  automatic checks do not inspect cancellation.
- Queue and Task handles are the maintained cross-worker communication
  surface. All other task captures and results must be owned `Transfer` values,
  preserving a share-nothing boundary. A task's cancellation and diagnostic
  context remain isolated from work executing on other workers.
- Task scheduling, cross-worker completion, and program-output order are
  unspecified. There is no worker-index or affinity-introspection API.
- MIR is the checked development path, not the performance path. In the
  Batch-4 multicore control, four MIR tasks took about `2.1x` the wall time of
  one task: interpreter work and synchronization increase the per-task cost
  when several workers execute MIR concurrently. Use the direct native backend
  for performance measurements.
- Pinned task execution is maintained on the MIR and direct native backends.
  Work stealing, preemption, and detached tasks are unavailable. Parallel
  speedup depends on the workload, and automatic parallelism applies only to
  task execution.
- Ordinary lightweight tasks request 512 KiB of writable coroutine stack.
  `TaskGroup.start_with_stack` and `start_soon_with_stack` accept exact
  `int64` requests from 256 KiB through 64 MiB inclusive. Accepted requests
  are rounded upward to the host page size and guard-protected; smaller and
  larger requests are rejected rather than clamped. The MIR/direct runtime
  entry thread reserves 64 MiB, and maintained execution paths stop with a
  friendly recursion-depth diagnostic after 256 nested Aura calls. The
  override API is Provisional under ADR-0032. The 256 KiB lower bound is an
  opt-in minimum for measured shallow tasks, not the generally safe default;
  the complete compiled Aura HTTP example faulted when 256 KiB was the
  global default and succeeds at 512 KiB. An isolated runtime protocol
  round trip succeeds with 256 KiB callers because it excludes compiled
  language-execution frames; it proves the service offload boundary, not a
  256 KiB whole-program default.
- On the clean Mac14,9 Phase 5.10 measurement at `181204b`, 10,000 parked
  sleepers used 207,798,272 bytes of worst whole-process RSS and 198,787,072
  bytes above the same-process pre-spawn baseline, passing the maintained
  512 MiB gate.
- The runtime accepts larger task counts; 10,000 sleepers is the maintained
  memory-capacity bound. The final Phase 5.10 100,000-sleeper plus 1,000-timer
  repetitions peaked at 1,170,735,104,
  1,921,531,904, and 2,001,305,600 bytes. Two of three exceed the 1.5 GiB
  gate. On this 16 KiB-page host, one resident page for each of the 101,000
  stackful child coroutines alone requires 1,654,784,000 bytes before task
  metadata or the root runtime. The Phase 5.9 passing observation depended on
  compression and reclaim behavior.
- The contractual 10,000-sleeper bound plus the timer, idle, starvation, and
  multicore controls all pass at Phase 5.10:
  the standalone timers had a 6 ms worst arm span and 1 ms p99 overshoot,
  idle CPU was below 2%, starvation latency was 14 ms, and the four-worker
  control had a `1.039673x` paired median wall-time ratio with `396.73%`
  median four-task process CPU on the measured Mac14,9 host.
- The scheduler uses persistent reactor registrations for nonblocking
  descriptors, a timer heap for deadlines, and direct Queue, task-completion,
  and blocking-pool notifications. When idle it blocks until an event or
  deadline and has no periodic scheduler tick.
- Deep HTTP, TLS, and maintained Unix WebSocket library frames run on a
  distinct protocol-step pool with two 2 MiB-stack workers and a 64-job queue.
  Each submitted job is a bounded, nonblocking step and returns owned protocol
  state before cancellation or reactor waiting resumes. The non-Unix
  WebSocket fallback does not use this Phase 5.4 service. The pool is
  process-global, lazily initialized, shared by all lightweight schedulers,
  and intentionally process-lifetime; it has no 0.3 runtime shutdown or join
  API. File reads, resolver work, and listener binding remain on the generic
  blocking-I/O pool; TLS asset bytes are read there before PEM parsing and
  rustls construction run on protocol workers.
- Filesystem one-shot reads and `fs.File` whole-file reads are capped at 256 MiB of remaining content. Aura 0.3 has no chunked file-read API.
- Process-pipe and captured-output reads plus TCP, Unix, and TLS whole/bounded reads remain capped at 64 MiB. TLS certificate, private-key, and CA-file loading uses the same independent 64 MiB ceiling. A bounded byte count of zero is invalid.
- UDP receives accept `max_bytes` from 1 through 65,535.
- Incoming HTTP parsing accepts at most 64 headers and 16 MiB of wire data per message, including the start line, headers, transfer framing, trailers, and body. Outbound HTTP writers have no separate size cap. The high-level dictionary header model cannot preserve repeated equal field names losslessly.
- WebSocket messages are capped at 64 MiB; individual frames and write buffers are capped at 16 MiB.
- TLS handshakes have a 10-second hard cap even when the caller supplies no shorter timeout.
- Duration is a signed i128 nanosecond language value, but host timer ranges are narrower. Negative values, out-of-range host conversions, and overflowing deadline calculations are invalid input rather than unlimited waits. The exact error classification is accepted under ADR-0019.
- High-level HTTP clients support HTTP/1.1 over `http://` and validated `https://`, including content-length, chunked, and close-delimited responses; redirects, pooling, HTTP/2, proxy configuration, decompression, and high-level custom CA arguments are not implemented.
- Byte-codec inputs have no separate byte-count cap, but byte conversions and
  hex/padded-base64 codecs preflight each fresh destination against a fixed
  2,147,483,647-byte safety ceiling. Crossing this codec output/resource cap
  or failing allocation traps with `AU4005`. This ceiling is independent of
  the public str and `list` length domains. SHA-256 always returns 32 raw
  bytes.
- JSON supports the recursive `json.Value` tree, typed `json.Error` parse
  failures, deterministic dumps, a 128-container depth limit, a shared
  root-inclusive 262,144-value materialization limit, and independent 64 MiB
  parse-input and dump-output caps. Exceeding the node limit or encountering a
  controlled parse/conversion allocation failure traps with `AU4005`; it is not
  a `json.Error` variant. Dynamic `json.parse` uses a separate process-global
  service with two 2 MiB-stack workers and total in-flight capacity two;
  capacity is reserved before the fallible source copy, and saturated
  lightweight tasks park through the scheduler. Once admitted, synchronous
  parse defers cancellation until codec completion. Runtime materialization,
  JSON-aware clone/render, and dumping use iterative traversals. The service is
  process-lifetime and has no 0.3 sizing or shutdown API. The bounded
  `json.is_valid` and `json.parse_string_map` helpers retain their bounded
  caller-side paths and do not use that service; JSON
  flat-dictionary and TOML helpers remain restricted to typed
  `dict[str, str]`. JSON has no arbitrary-precision number, streaming
  codec, or derived class/enum schemas.
- `random.Rng` provides one fixed deterministic stream with integer, floating,
  and mutable-list shuffle operations. There is no global generator, state
  serialization, reseeding, jump/substream operation, distribution library,
  choice helper, public direct or transitive clone route, secure floating
  function, or `random.Error`. Clone-producing collection operations are
  rejected with `AU3007` when their produced value contains or may contain an
  `Rng`. An owned generator may move within one owning task, but it is not
  `Transfer`: it cannot be a task result or Queue payload. Queue handle copies
  remain valid; a Task handle is copyable only for a repeatable result.
  Generic clone-safety requirements are inferred from callable bodies,
  propagated through generic calls and imports, and checked after
  specialization; there is no source annotation for them. Trait defaults may
  establish this contract, but an explicit implementation may not strengthen
  it. Recursive nominal inspection terminates conservatively when safety cannot
  be proved.
  `secure_bytes` accepts at most 2,147,483,647 bytes as a fixed per-request
  resource and safety ceiling, independently of the public `list` length
  domain. Larger counts fail with `AU4005` before allocation or entropy.
  Within that request ceiling, unsatisfied allocation or OS entropy requests
  also trap with `AU4005`.
- Metrics are process-global counters within one running program; log and trace APIs emit structured stderr records and do not yet include exporters or scoped spans.
- `control.retry` is a sequential eager helper for a repeatable
  `def() -> Result[T, E]` worker. The worker may be a capture-free function
  value or a repeatable capturing closure. Every `Err` is retryable. It has no
  error classifier, jitter, attempt hook, shared retry budget, or
  detached/parallel mode. Attempt budgets below one and negative or
  host-unrepresentable backoffs trap before the worker runs. Backoff overflow
  traps, worker traps propagate, and task cancellation is not converted to the
  worker's `E`.
- Floating-point `/`, `//`, or `%` by zero traps at runtime instead of producing IEEE 754 infinity or NaN.
- `float32` literals that overflow may currently become infinity; prefer `float64` when large literal validation matters.
- Unix domain sockets require a Unix host.
- TLS APIs require PEM certificate/key assets.
- Package support has local path and git dependencies, but no registry publish/install flow.
- `fs.read_dir` silently skips an individual directory entry that fails after the directory itself was opened.
- High-level HTTP header conversion may expose duplicate equal dictionary keys when the wire message repeats a header name; repeated headers are not a lossless 0.3 contract.
- Accepted ADR-0033 rejects non-Transfer task captures, task results, and
  Queue payloads with `AU3008`. Every other non-repeatable transferable task
  result has one statically enforced observation right: direct result methods
  consume it on every outcome, and multi-task waits consume the complete task
  list. A second runtime claim that reaches the atomic containment check
  traps with `AU4001` rather than returning or cloning the stored value.
- Cancelling filesystem and other blocking-worker I/O cancels Aura's wait,
  not an accepted operating-system call. Before insertion into the pending
  queue, timeout or cancellation prevents submission; after insertion, the
  host operation runs exactly once and external side effects may still
  complete while its late result is discarded.
- The process-wide blocking-I/O pool defaults from host parallelism with
  fallback `4` and a derived `2..=8` clamp.
  `AURA_BLOCKING_WORKERS=<positive integer>` instead requests that exact
  count without clamping. `AURA_BLOCKING_QUEUE_CAPACITY=<positive integer>`
  bounds pending accepted jobs only; running jobs and admission waiters do not
  consume it, and omission preserves an unbounded queue. Full-queue admission
  is FIFO and scheduler-aware. The first runtime preflight reads the settings
  once and keeps them immutable for the process lifetime without starting
  workers. First submission creates the complete worker set; production reuses
  it until process exit and has no Aura shutdown/join surface. This bounds
  accepted pending backlog, but not admission waiters, and cannot interrupt a
  stuck accepted call or guarantee unrelated blocking-I/O progress while every
  worker remains occupied.
- `WebSocketListener` has no explicit `close()` method, and WebSocket cancellation/error propagation is not yet fully aligned with TCP and UDP.

## Tooling

- `build` requires a host C compiler. Source-checkout builds may use Cargo to refresh the native runtime; release archives carry that runtime and do not require Rust or the source checkout.
- Native `run` cache entries larger than 512 MiB are not retained. The
  just-built program still runs, but a later invocation rebuilds it instead of
  using the cache.
- The direct backend is the maintained native backend for the implemented language surface.
- The default `--backend auto` first tries direct emission and may package an embedded-MIR launcher when direct emission is unavailable. Use `--backend direct` when fallback is unacceptable.
- Editor tooling uses a persistent compiler service. If that process is unavailable, recovery is lexical only and intentionally has no semantic diagnostics or member inference.
- `aura fmt` currently normalizes line endings, trailing whitespace, and final newlines; it is not yet a syntax-reflowing formatter.
- `aura test` discovers each parameterless `def test_*()` function as a
  separate result and retains file-level execution for files with no such
  function. Optional `setup()` and `teardown()` run per selected case, including
  teardown after setup/body failure. Parameterized registration returns labeled
  capture-free `def() -> None` values in `list[(str, def() -> None)]`; registration
  runs before `-k` filtering. Discovery remains name-prefix based; annotations
  are not implemented.
- A timed-out `aura test` stops waiting but cannot terminate its worker thread; the timed-out program may continue host side effects until the process exits.
- Recursive `aura fmt` and `aura test` traversal follows directory symlinks without cycle detection in 0.3.

### Native artifact profile (0.3.4 foundations)

The workspace release profile uses optimization level 3, fat LTO, one codegen
unit, no debug data, and symbol stripping for the shipped compiler executable.
Panic unwinding remains enabled. The runtime archive retains linkable symbols.
Both direct and embedded-MIR user executables link with `-Wl,-dead_strip` on
macOS or `-Wl,--gc-sections` on Linux, then use `strip -S -x` to remove debug and
local symbols while preserving globals needed by process-global FFI. Source
locations, typed Aura call frames, and task ancestry are embedded runtime
metadata and remain available; native symbol/debugger information is reduced.
The host toolchain must provide `strip` as well as `cc`. The compiler
and runtime still share one static archive.
