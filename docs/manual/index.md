# Aura Language Reference

The source version for this Manual is **Aura 0.3.3 (technical preview)**.
The release stamp below records the rendered implementation baseline commit.

<ReleaseStamp />

Release builders set `AURA_DOCS_COMMIT`; GitHub builds use `GITHUB_SHA`; a
clean local build falls back to the checkout's committed `HEAD`. A dirty or
Git-free build says `local-uncommitted-checkout` instead of inventing a commit
or writing a self-referential hash into this source page.

This Manual is the normative reference for the implemented Aura language and runtime. It is written so a reader can reconstruct the language accurately: complete syntax, name and type rules, ownership behavior, execution, module APIs, diagnostics, limits, and tool contracts.

The Learn track tells a story. A future book may build a longer learning sequence from this material. The reference defines the facts that those teaching materials must preserve.

Start with [Language Specification](/manual/language-specification) for scope, terminology, authority, and conformance language.

## Language Reference

- [Lexical Structure](/manual/lexical-structure): files, physical/logical
  lines and delimiter continuation, indentation, comments, identifiers,
  keywords, literals, f-strings, and duration literals.
- [Complete Grammar](/manual/grammar): normative EBNF, precedence, associativity, contextual syntax, layout, and unsupported forms.
- [Names And Scopes](/manual/names-and-scopes): modules, imports, visibility, bindings, block scope, no-shadowing, and member lookup.
- [Types](/manual/types): primitive types, tuples, `None`, `Duration`, generic types, copy and move categories, and type annotations.
- [Static Semantics](/manual/static-semantics): inference, type equality, assignment, calls, operators, constructors, matching, traits, resources, and entrypoints.
- [Expressions](/manual/expressions): operators, calls, indexing, owned
  list/str slicing, member access, literals, conditional expressions,
  membership and comparison chains, `match` expressions, `try`, and f-strings.
- [Statements](/manual/statements): bindings, assignment, control flow, loops, imports, `with`, `pass`, assertions, and top-level execution.
- [Tuples](/manual/tuples): fixed structural values and types, recursive
  unpacking and patterns, whole-source ownership, constant indexing, and
  recursive equality.
- [Assertions](/manual/assertions): exact boolean conditions, lazy messages,
  `AU4001` failures, cleanup precedence, and backend behavior.
- [Functions](/manual/functions): signatures, bare/`own`/`mut` parameter
  modes, default arguments, named arguments, `main`, owned returns, and call
  binding.
- [Closures](/manual/closures): contextual expression lambdas, by-value
  captures, repeated read-only calls, consuming single-use calls, and
  structural Transfer.
- [Foreign Function Interface (FFI) v0](/manual/ffi): explicit package
  authorization, bodyless C declarations, fixed-width scalars, pointer-length
  views, opaque handles, and the native safety boundary.
- [Classes](/manual/classes): fields, constructors, methods, receivers, associated methods, resources, and mutation.
- [Enums And Pattern Matching](/manual/enums-and-match): variants, payloads, exhaustiveness, literal patterns, short-form variants, and match value flow.
- [Generics And Traits](/manual/generics-and-traits): type parameters, trait declarations, impls, bounds, dispatch, and current restrictions.
- [Ownership And Borrowing](/manual/ownership-and-borrowing): moves, copies, clones, shared borrows, mutable borrows, field moves, and task boundaries.
- [Execution Model](/manual/execution-model): evaluation order, entry execution, backends, cleanup, runtime failures, scheduling, cancellation, and external effects.

## Runtime And Library Reference

- [Collections](/manual/collections): `list[T]`, `dict[K, V]`, `set[T]`,
  literals, eager owned comprehensions and slices, iteration, mutation, and
  eager callable-powered list algorithms.
- [Numeric Arrays](/manual/numeric-arrays): contiguous row-major `Array[T]`,
  four numeric dtypes, first-axis owned slices, reductions, native kernels,
  and explicit checked/wrapping/saturating integer arithmetic.
- [Math Module](/manual/math): exact binary64 constants plus scalar rounding,
  power, exponential, logarithmic, and trigonometric functions with explicit
  domain and overflow behavior.
- [Bytes, Text Codecs, And SHA-256](/manual/bytes): `list[uint8]`, strict UTF-8 conversion, canonical hex/base64, typed data errors, and raw SHA-256.
- [JSON Module](/manual/json): recursive JSON values, typed parse errors, exact number classification, deterministic dumping, and resource limits.
- [Randomness Module](/manual/randomness): deterministic seeded streams, exact sequence compatibility, unbiased ranges, in-place shuffle, and OS-secure integers and bytes.
- [Concurrency](/manual/concurrency): `TaskGroup`, `Task[T]`, `Queue[T]`,
  cancellation, `yield_now`, typed heterogeneous `select`, `wait_any`,
  `wait_all`, and scheduler-aware waits.
- [I/O Module](/manual/io): standard input/output and `io.Error`.
- [Filesystem Module](/manual/filesystem): one-shot helpers, `fs.File`, scoped file cleanup, byte and text limits.
- [Network Module](/manual/network): TCP, UDP, HTTP, WebSocket, Unix sockets, TLS, and HTTP client helpers.
- [Process Module](/manual/process): subprocess spawning, pipes, completed processes, process groups, supervisors, and restart policy.
- [Control-Plane Modules](/manual/control-plane): system/path helpers, JSON and
  TOML compatibility APIs, telemetry, and `control.retry`.
- [Packages](/manual/packages): manifests, package roots, import resolution, lockfiles, and editor analysis behavior.
- [CLI And Tooling](/manual/cli-and-tooling): `aura` commands, diagnostics, analysis JSON, completions, and build modes.
- [API Index](/manual/api-index): every maintained builtin function, method, enum, and module type in one place.
- [Diagnostics](/manual/diagnostics): compile-time/runtime categories, source rendering, machine-readable positions, and CLI exit status.
- [Performance](/manual/performance): reproducible measurements, current gaps,
  evidence provenance, and the optimization direction for later releases.
- [Current Limits](/manual/current-limits): intentional current boundaries and practical workarounds.
- [Conformance](/manual/conformance): executable fixture/test mapping and the rules for changing the language safely.

## Conventions Used In This Manual

Code blocks marked `python` contain Aura code using Python highlighting until the documentation theme ships a dedicated highlighter. The language grammar itself is defined by [Complete Grammar](/manual/grammar). Shell blocks contain repository commands.

Signatures use `Duration = ...` for optional timeout parameters whose default is documented in the relevant API section. In general:

- blocking APIs wait when a timeout is omitted
- convenience helpers ending in `_or_none` or `_or` may use immediate non-blocking checks when documented that way
- timeout results are explicit variants such as `TimedOut`, `None`, or `process.Error.TimedOut`
- explicit timeout values must be non-negative and fit the host deadline; invalid values return the documented typed InvalidInput/process error or trap with `AU4001` when the API has no typed error carrier

When a page says a value is returned "cloned", it means the caller receives a new owned value. When a page says a method "moves" an argument, the caller cannot use that argument after the call unless it is a copy type.
