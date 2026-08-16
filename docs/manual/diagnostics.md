# Diagnostics

Aura diagnostics are part of the language and tooling contract. Lexing,
parsing, static checking, ownership checking, lowering, building, and runtime
traps all use the compiler-owned diagnostic structure described here. A typed
library failure such as `Result.Err`, `Option.None`, a timeout, cancellation, or
an `io.Error` value is ordinary program data, not a diagnostic.

## Stable Diagnostic Codes

Every diagnostic has a stable code of the form `AU####`. The first digits name
the phase that owns the failure:

| Band | Phase | Current codes |
| --- | --- | --- |
| `AU10xx` | lexical analysis | `AU1001` invalid lexical input; `AU1002` invalid f-string delimiter |
| `AU11xx` | parsing | `AU1101` invalid syntax |
| `AU20xx` | names and types | `AU2001` name resolution; `AU2002` type mismatch; `AU2003` unsupported operator; `AU2004` argument binding; `AU2005` unsupported syntax or feature; `AU2006` builtin method collision; `AU2007` builtin function redefinition; `AU2008` equality unavailable; `AU2999` general compile-time rejection |
| `AU30xx` | ownership, loans, and transfer | `AU3001` moved value; `AU3002` borrow/loan conflict; `AU3003` mutability violation; `AU3004` ownership or place mode; `AU3005` non-copy indexed read; `AU3006` non-copy indexed compound assignment; `AU3007` non-cloneable state duplication; `AU3008` non-transferable task/Queue boundary; `AU3009` single-consumer task-result duplication; `AU3010` view escape or returned provenance |
| `AU40xx` | runtime-checked traps | `AU4001` general runtime trap; `AU4002` arithmetic overflow or underflow; `AU4003` bounds or lookup violation; `AU4004` zero divisor; `AU4005` resource, allocation, or I/O failure; `AU4006` invalid runtime configuration; `AU4007` numeric Array shape or reduction violation |

`AU1001` also owns source-delimiter pairing. An unexpected closer is primary at
that closer. A mismatched closer names the expected kind and labels its opener
as related information. EOF with an unclosed delimiter reports the expected
closer and labels the opener. These locations and labels are preserved by
analysis JSON and the LSP bridge.

The registry is append-only. Once published, a code MUST NOT be reused,
renumbered, or silently reassigned to a different diagnostic category. If a
diagnostic becomes obsolete, its number remains reserved. New categories
receive new numbers. Message wording and attached guidance may become more
specific without changing a code when the failure category is unchanged.

`AU2999` is the maintained catch-all for compile-time rejections that do not
yet have a narrower public category. It is a stable code, not permission for a
tool to omit the code.

`AU2006` identifies an explicit or inherited trait method whose name would
shadow a builtin member of the implementation's builtin target. The rule covers
every builtin target, from the runtime handles `Queue[T]`, `Task[T]`,
`TaskGroup`, `random.Rng`, `fs.File`, and the `net` and `process` handles, to
the builtin value types such as `str`, `list[T]`, `dict[K, V]`, `set[T]`,
`Duration`, and the scalar types. Its guidance requires the trait method to be
renamed; backend dispatch is never selected by which implementation happens to
run first.

`AU2007` rejects a module-level function declaration whose name is already a
builtin function name, such as `len`, `str`, `abs`, `print`, or `select`. The builtin
surface is closed, so the declaration must be renamed. This rejection is
distinct from the `AU2006` method collision: it covers free functions rather
than trait methods on a builtin target.

`AU2008` reports an unmet equality obligation. It covers direct `==` and `!=`
and every collection operation that depends on equality: membership,
`list.remove`, `list.index`, `list.count`, set element insertion, and
dictionary-key use. Named function values, closures, `random.Rng`, opaque FFI
handles, and values containing any of those types do not define equality.
The diagnostic names the unavailable relation before execution can reach a
backend identity comparison.

`AU4007` is the numeric Array structural runtime diagnostic. It reports
rank-zero or negative-dimension construction, `from_list` count mismatch,
exact-shape operator mismatch, direct coordinate-count/runtime-rank mismatch,
and empty `min`, `max`, or `mean`. Shape-product/element-count overflow and
allocation failure remain `AU4005`. Out-of-range coordinates and invalid
first-axis slice bounds remain `AU4003`. Optional `get` absence is ordinary
`None`; method `set` traps on an invalid coordinate or rank.

`AU2002` reports an exact callback-contract mismatch for callable-powered
builtins. List `map`, `filter`, and keyed `sort` require the documented shared
`def(T) -> ...` parameter capability; a `mut` or `own` callback is not silently
adapted. The same code reports a `control.retry` worker that is not exactly a
zero-parameter `def() -> Result[T, E]`. `AU2002` also reports a `sort` element
or keyed `sort` key type without the required natural ordering.

At the FFI boundary, `AU1101` provides dedicated parser guidance for malformed
extern bodies, defaults, type parameters, callbacks, variadics, and raw-pointer
syntax. `AU2002` rejects types outside the FFI v0 scalar/view/opaque table;
`AU2005` rejects reserved raw-pointer/callback contracts or opaque
construction; `AU2999` covers package authorization, root dependency reports,
and direct-call-only policy; and `AU3004` reports an invalid FFI capability.
Opaque handle moves and task/Queue boundaries retain `AU3001` and `AU3008`.
A non-canonical C boolean result (a returned byte other than `0` or `1`) traps
with `AU4001`. `AU4005` reports a missing process-global symbol, null
opaque-handle result, or runtime marshalling failure. Native aborts, signals,
memory faults, and foreign unwinds may terminate the process and are not
Aura diagnostics. See [FFI v0](/manual/ffi).

`AU3005` rejects a direct `list` or `dict` indexed read that selects a non-copy
element or value, and constant tuple indexing that selects a non-copy element.
For collections its guidance is clone-safety aware, classified exactly as the
rejection is: a clone-safe type is directed to the explicit cloned `get`
surface; a type carrying non-cloneable `random.Rng` state is directed to
`remove` alone, because `get` on it would be rejected in turn by `AU3007`; and
an unresolved generic type is told that `get` requires a clone-safe type, with
`remove` offered unconditionally. For tuples, unpack the whole tuple to move
its non-copy elements. `AU3006` rejects the
corresponding `list` or `dict` indexed compound assignment because
read-modify-write would otherwise require a hidden clone or destructive move
of the stored value.

`AU3007` rejects an operation that would duplicate non-cloneable state.
Protected values include `random.Rng`, opaque FFI handles, and capturing
closure environments, and the check follows them through collections, user
classes, enum payloads, and other value wrappers. A generic
definition over unresolved types records an inferred clone-safety obligation;
`AU3007` is emitted at an unsafe concrete specialization, when a concrete
requirement cannot be proved, or when an implementation would strengthen its
trait method's contract. Because `list.filter` clones accepted source elements
into a fresh result, it establishes the same obligation and rejects
`list[random.Rng]` or a transitive wrapper. Under Accepted ADR-0033,
`random.Rng` is not
Transfer: a task returning it and a Queue carrying it are rejected with
`AU3008`, and the task handle is not copyable. Moving or removing a generator
within one owning task remains valid because it leaves one owner.

Accepted ADR-0033 reserves `AU3008` for a captured argument, task result, or
Queue payload that cannot cross a task-worker boundary. The diagnostic names
the failed boundary and follows the specialized type to the first
non-transferable leaf, including its field, tuple element, collection
component, or enum payload path. For example, it explains that a `Job` cannot
cross because `Job.source` contains `fs.File`; it does not stop at
“`Job` is not Transfer.”

`AU3008` guidance recommends passing owned transferable input/output data
instead of a non-copy shared or mutable capability, and keeping live host
authority or `random.Rng` on its owning task. It may explain that reading Copy
data materializes an owned snapshot; it must not claim all borrowed Copy
captures fail. It never proposes an `impl Transfer` because Transfer is
compiler-derived and has no builtin source-level trait surface. An ordinary
user trait also named `Transfer` and its implementations do not alter the
structural property.

`AU3009` rejects an operation that would duplicate an existing
single-consumer task-result observation right. It covers explicit clone,
clone-producing `get`, and implicit collection or aggregate copy. It is not a
Transfer-boundary failure: the contained task handle is Transfer, but is
non-copy because its result is non-repeatable. A later use of the same binding
after a consuming result observation is `AU3001`; trying to consume the right
through shared access is `AU3002`.

`AU3010` rejects a view that escapes into ordinary storage, a returned view
whose expression does not derive from its declared receiver/parameter origin,
an invalid returned kind, or a call whose origin is not an addressable place.
The diagnostic identifies the declared origin and the incompatible expression
or destination. Return an owned clone/index/handle when the access must escape,
or keep a local/closure loan synchronous and inside the owner's region.

For explicit views, `AU3002` labels both the view creation and the final use
that keeps its inferred region live. Removing a later use can shorten the loan;
otherwise shorten the lexical scope, select a proven-disjoint place, or create
an owned clone. `AU3003` covers mutation through a shared view and calling a
mutable-repeatable closure through an immutable place. `AU3004` covers
non-place sources, immutable mutable-view targets, and unsupported projections
such as collection indexes.

For `select(...)`, `AU3009` also rejects the same statically visible
non-repeatable Task source appearing twice in one call. `AU3002` explains that
each non-repeatable Task must arrive through owned access because `select`
consumes all such observation rights at entry and abandons losers. Call-shape
errors such as an empty call or named source are `AU2004`; an invalid source
kind or inconsistent Queue/Task category type is `AU2002`.

The atomic runtime containment for non-repeatable results is separate from
those static errors. If a backend defect or foreign handle reaches a second
runtime claim, Aura traps with `AU4001` and
`task result has already been observed; non-repeatable task results allow
exactly one observing attempt`; it never returns or clones the stored value.
The same defense applies when malformed backend state reaches `select`; an
already-claimed or duplicated non-repeatable Task traps with `AU4001` before
any result is delivered.

## Diagnostic Structure

A diagnostic contains all of the following fields:

| Field | Meaning |
| --- | --- |
| `code` | stable `AU####` identifier |
| `severity` | `error`, `warning`, `information`, or `hint` |
| `message` | concise primary explanation |
| `primary_span` | optional path and source range for the failed operation |
| `secondary_spans` | related source ranges, each with a label |
| `notes` | contextual facts that do not prescribe a change |
| `help` | actionable human guidance |
| `edits` | source replacements with an applicability classification |
| `call_frames` | Aura call frames, ordered innermost first |
| `task_ancestry` | structured task parentage, ordered youngest first |

The current compiler emits errors; the additional severity values are reserved
by the shared schema. A machine-applicable edit is safe for a tool to offer as
an automatic source replacement at the stated range. Tools MUST preserve edits
and MUST NOT infer an edit from prose alone.

Compiler and CLI spans use one-based line and column numbers. Each structured
span is a half-open range with `start` and `end`; current token diagnostics may
use a one-column primary range. The LSP bridge converts those ranges to the
zero-based line and character coordinates required by the Language Server
Protocol.

## Human-Readable Form

The default CLI form begins with the stable code:

```text
error[AU2001]: unknown name `missing`
 --> path/to/file.au:2:11
  |
2 |     print(missing)
  |           ^
```

Related spans follow as `related` records. Context appears as `note`, proposed
actions as `help`, and source replacements as `fix`. A source-backed operation
uses the path and source context where the diagnostic was detected, including
an imported module rather than its importer. If no valid source line is
available, the renderer still emits the code, message, and best available
location.

The compiler normally reports one primary failure for an operation instead of
inventing speculative follow-on errors. A conforming implementation MUST reject
invalid source rather than silently reinterpret it.

## JSON Form

`aura check --format json` writes one JSON document. `aura run --format json`
and `aura build --format json` use the same document for compile failures. The
top-level `schema_version` is currently `1`, and `diagnostics` is an array.

For `check`, `run`, and `build`, the current compiler emits at most one
diagnostic per invocation: the pipeline stops at the first failure. On failure
the schema-version-1 `diagnostics` array therefore contains exactly one entry;
on successful `check` it is empty. The array is retained for schema
compatibility and future recovery, and tools must not infer that the source
contains no additional errors.

```json
{
  "schema_version": 1,
  "diagnostics": [
    {
      "code": "AU2001",
      "severity": "error",
      "message": "unknown name `missing`",
      "primary_span": {
        "path": "path/to/file.au",
        "start": { "line": 2, "column": 11 },
        "end": { "line": 2, "column": 12 }
      },
      "secondary_spans": [],
      "notes": [],
      "help": [],
      "edits": [],
      "call_frames": [],
      "task_ancestry": []
    }
  ]
}
```

`primary_span` is `null` when no source location exists. A secondary span has
the same `path`, `start`, and `end` fields plus a string `label`. Each edit has
`path`, `start`, `end`, `replacement`, and `applicability`. Successful
`aura check --format json` emits schema version 1 with an empty diagnostics
array. Successful `run` and `build` retain their ordinary program-output and
artifact contracts; `--format` selects their diagnostic representation, not
the program's data format. A direct run that performs long native work may also
write one schema-version-1 status document on standard error. Its `progress`
array contains the exact wait/rebuild notices. If `auto` falls back
successfully, the same document contains
`"fallback":{"from":"direct","to":"mir","reason":"..."}`. If the fallback
then fails, its progress and direct failure are retained as notes in the one
diagnostic document.

Every diagnostic entry contains both frame arrays, including compile-time and
pre-user-code failures where they are empty. A call-frame record contains
`function` and a `span`. A task-ancestry record contains `task_function`,
`task_entry_span`, `parent_function`, and `spawn_span`. Each frame span carries
its own `path`, `start`, and `end`, so a frame defined or spawned in an
imported module is never mislabeled with the entry module's path.

The arrays are an additive schema-version-1 extension. Schema-version-1
readers MUST ignore unrecognized object members while continuing to validate
the fields they use. The compiler-service semantic-interface version is `5`.

The process exits unsuccessfully after emitting a JSON error report. Tools MUST
parse standard error as one JSON document in JSON mode and MUST NOT scrape the
human renderer.

## LSP Contract

The compiler service owns editor diagnostics. Its analysis record carries the
same code, severity, message, secondary spans, notes, help, edits, call frames,
and task ancestry. Frame spans use zero-based `file_path`, `line`,
`start_character`, and `end_character` coordinates in this editor shape. The
JavaScript language-server bridge maps the primary span to the LSP range, maps
secondary spans to `relatedInformation`, places the code in `Diagnostic.code`,
and preserves the remaining metadata in `Diagnostic.data`.

There is no independent semantic-diagnostic implementation in the language
server. If the compiler service is unavailable, lexical recovery may keep basic
editor navigation usable, but it MUST NOT invent semantic success or fabricate
compiler diagnostics.

## Ownership Diagnostics

Ownership diagnostics use the `AU30xx` band. When the checker has both sites,
the primary span identifies the invalid later operation and a labeled secondary
span identifies the earlier move or borrow that made it invalid. Applicable
guidance names the smallest explicit repair: change a parameter to `own`, clone
at a deliberate ownership boundary, use the appropriate borrow loop form, add
`mut`, or declare a mutating receiver as `mut self`. When a repair is a
local, unambiguous source replacement, the diagnostic also carries a
machine-applicable edit.

Guidance is not a relaxation of ownership rules. In particular, Aura never
inserts a hidden clone or converts a borrow into ownership to recover from an
error.

For `AU3007`, guidance offers the two explicit single-owner exits: move or
remove the existing value, or construct an independent generator from an
explicit seed. It does not offer `.clone()` on any type whose value contains or
may contain `random.Rng`. Clone-producing aliases—including collection reads
and task-result observations—are subject to the same rule as a direct clone.

When a binary left operand, index base, method receiver, or indexed-assignment
target retains a non-copy borrow through later inputs, an overlapping later
mutable borrow or consumption is `AU3002`. The conflicting later access is the
primary span and the retained selection is a labeled borrow-origin secondary
span. Guidance may suggest an explicit clone when the type supports it or a
separate earlier mutation, but the compiler does not deep-clone implicitly.

For example, consuming a bare shared parameter reports that parameter `x` is
borrowed and recommends declaring it as `own str` to take ownership or
cloning the value before consuming it. The parameter name and concrete type in
that message come from the rejected declaration.

## Python-Shaped Source Guidance

`AU2005` identifies focused guidance where Python-looking source has an Aura
spelling. Maintained hints
cover `True`/`False`, `.append(...)`, `is` and `is None`, and `try`/`except`.
Eager list, set, and dictionary comprehensions are accepted. A generator expression,
whether parenthesized or used as a call argument, receives this exact `AU2005`:

    generator expressions are unavailable; use an eager owned list comprehension or an explicit loop

`mut` or `own` in a comprehension clause is malformed syntax and receives
`AU1101` with the exact teaching message:

    comprehensions use bare iteration; remove `mut` or `own` and write `for name in values`

The bare form preserves the iterable's ordinary contract, including owned
receive items for Queue. Related diagnostics
cover missing `mut`, consuming calls, integer `/`, typed `self: Type`, tab
indentation, and single-quoted f-strings.

String literal and format diagnostics use the smallest proving location.
`AU1001` reports malformed or unterminated ordinary, triple-quoted, and raw
strings, including a later physical line that contains an invalid escape.
`AU1002` gives focused supported-form guidance for raw and triple-quoted
f-string prefixes.
`AU1101` reports malformed static format grammar, nested fields, unsupported
codes, and width or precision above `1_000_000`. `AU2002` reports a valid
specification that is incompatible with the interpolation's static type.
Constructed string output above the 64 MiB limit reports `AU4005` before the
oversized append mutates the partial result.

Python permits decimal grouping without an explicit type code, as in
`f"{n:,}"`. Aura requires the numeric code: use `f"{n:,d}"`, `f"{n:,f}"`, or
`f"{n:,%}"`. This keeps grouping validation tied to a statically selected
numeric rendering contract.

Owned list and str slicing is implemented, but step syntax and slice
assignment remain reserved. They use `AU2005` with these exact messages:

    slice steps are unavailable; use an explicit loop to select a stride

    slice assignment is unavailable because slices are owned copies; mutate the source by index or build a new value

Written slice endpoints use the `int64` index domain; a mismatched bound uses
`AU2002`. Fixed-width narrower integers widen losslessly at that position. A list slice that would duplicate `random.Rng`, an opaque FFI handle,
or a capturing closure environment uses `AU3007`; one that would duplicate a
non-repeatable Task result right uses `AU3009`.
An endpoint outside `0..=len` after one negative normalization, or a start
greater than its end, traps with `AU4003`. Unlike Python, Aura never clamps a
slice endpoint.

`in`, `not in`, chained comparisons, `len(...)`, `str(...)`, and contextually
typed expression lambdas are accepted forms and their fixtures assert those
spellings.

Hints MUST name an available spelling when one exists. For an unavailable
form, they MUST name a working expression or statement form. The complete hint family is
pinned under `crates/aura-compiler/tests/fixtures/python-hints/`.
`AU2005` also identifies `str(...)` constructor-shaped source and directs
the caller to Aura string literals.

## Runtime Traps And Backtraces

Runtime diagnostics use `AU40xx` and preserve the source span embedded during
lowering. Output produced before a trap is not discarded: `aura run` leaves
program standard output intact, renders the diagnostic on standard error, and
exits unsuccessfully.

A failed assertion is `AU4001` at the `assert` keyword location. The
message is exactly `assertion failed` when omitted and otherwise exactly the
evaluated str, including an empty or whitespace-only value. A failure while
evaluating the condition or message remains primary. Active cleanup still
runs, but a cleanup failure cannot replace an already established assertion
diagnostic.

The MIR and direct runtimes attach the same typed Aura frames to every trap.
Call frames name the Aura function and its defining source span, ordered
innermost first. If the trap occurs in a task, task-ancestry records also
identify that task's entry, its parent function, and the exact source location
from which each task was started, ordered youngest first. These are Aura
frames, not host Rust, Cranelift, scheduler, or service-worker frames.

Frame records are captured once when the primary trap is established, before
cleanup or task-state reset. Propagation through callers, Task results, task
groups, or workers does not append observer frames. A child starts a new call
chain; its relationship to the parent is represented by task ancestry.

Human rendering synthesizes the established `Aura call chain`, `Aura task
entry`, and `Aura task ancestry` note lines from the typed records after
ordinary notes. Those generated strings are not stored in structured `notes`,
so JSON and LSP clients consume the frame arrays without parsing or
deduplicating prose.

JSON-mode direct runs transport a native trap to the `aura` parent through a
private fixed-marker pipe and a separate bounded JSON-data pipe. Native
initialization hides and marks both descriptors close-on-exec before user code.
The parent emits one schema-version-1 document, including any buffered
native-build progress in ordinary `notes`. An Aura trap is distinct from a
successful `main() -> int32` returning a nonzero status; a signalled
missing/malformed record is a host failure, and `auto` never falls back to MIR
after launch. Human-mode direct runs and standalone direct binaries continue
to render the complete diagnostic themselves.

Checked overflow, zero division, bounds failure, recursion-depth failure, and
an explicitly trapping invalid runtime state are diagnostics. File, process,
network, timeout, cancellation, and protocol operations normally return typed
values instead; the feature page for an API states any trapping exception. A
negative, host-unrepresentable, or deadline-overflowing Duration returns the
documented `InvalidInput`/process error when that API has a compatible typed
carrier. A timer API without one traps with `AU4001`; deadline overflow never
means an unlimited wait. This classification is accepted under ADR-0019.

`control.retry` doubles a `Duration` backoff only when a later attempt can use
the doubled value. If that required doubling exceeds the exact signed
`Duration` range, it traps with `AU4002`; it does not wrap, clamp, return the
most recent `Err`, or compute an unused post-final delay. Worker traps and
current-task cancellation likewise propagate instead of being converted to
the worker's `E`. `AU4003` rejects `max_attempts < 1`; `AU4001` rejects a
negative or host-unrepresentable `initial_backoff`. These inputs are validated
before the worker runs.

The random module returns plain values rather than a `random.Error` enum.
`AU4003` reports an empty or reversed `next_int`/`secure_int` interval and a
negative `secure_bytes` count. `AU4005` reports a `secure_bytes` count above the
fixed per-request ceiling of `2147483647` before allocation or entropy is
requested, secure operating-system entropy failure, or allocation failure. A
secure operation never recovers by substituting bytes from the deterministic
generator.

An explicit task-stack request has exact type `int64` and an inclusive
262,144..67,108,864-byte range. `AU2002` rejects an out-of-range literal during
checking. A dynamic value outside that range and a stack-allocation or
platform-size failure trap with `AU4005`; Aura never clamps the request or
silently substitutes the default.

`AU4006` reports invalid process runtime configuration.
`AURA_WORKERS`, `AURA_BLOCKING_WORKERS`, and
`AURA_BLOCKING_QUEUE_CAPACITY` each require a positive decimal integer.
Empty, zero, signed, whitespace-padded, non-decimal, non-Unicode, and
overflowing values are rejected before user code; the diagnostic names the
setting and renders the supplied value, using a lossy display for a non-Unicode
value. Failure to create the configured blocking-I/O worker set also uses
`AU4006` and does not silently use fewer workers or synchronous execution.

JSON input-data failures are typed `json.Error` values rather than diagnostics.
Parse allocation failure or exceeding the shared 262,144-value
materialization limit uses `AU4005` instead. `json.dumps` uses `AU4003` for an
indent outside `0..=16` or a value deeper than 128 containers, `AU4001` for a
NaN or infinite `json.Value.Float`, and `AU4005` when conversion exceeds the
same node limit, encoded output would exceed 67,108,864 bytes, or a controlled
conversion/output allocation fails. No failed dump returns a partial str.

Malformed UTF-8, hexadecimal, and base64 input returns `bytes.Error`, including
the relevant zero-based byte offset or odd input length, when that metadata fits
the retained `int32` payload. A required offset or length above `2147483647`
uses `AU4005` rather than truncating or wrapping the typed error. A fresh bytes
conversion or codec destination above the fixed 2,147,483,647-byte safety
ceiling, destination-size arithmetic overflow, or allocation failure also uses
`AU4005`; the ceiling is independent of the public str and `list` length
domains, and no failed operation returns a partial str or byte list.

Unrecoverable host or dependency-internal out-of-memory termination remains
outside the catchable diagnostic contract.

## CLI Exit Status

| Status | Meaning |
| --- | --- |
| `0` | command succeeded, help/version was requested, or a `None`-returning program completed |
| `1` | compile, package, build, test, or runtime operation failed |
| `2` | command usage or option parsing was invalid |

For `aura run`, an `int32` result from the entry module's `main` becomes the
requested process exit status; a `None` result completes successfully. Host
operating systems may restrict how exit values are represented after the value
leaves Aura. `aura test` succeeds only when every selected `.au` program
checks and runs within its timeout and every integer `main` result is zero.

## Internal Errors

An `internal error` message indicates an implementation invariant failure or a
defensive check for malformed internal input. Valid, statically checked Aura
source must not produce one. Panics, host crashes, memory-safety failures, and
hangs are never conforming diagnostic behavior and must be treated as compiler
or runtime bugs.
