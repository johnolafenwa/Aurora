# Execution Model

Aura source is statically checked, lowered, and executed with deterministic single-expression sequencing plus scheduler-controlled concurrency and external I/O. This chapter defines observable behavior shared by `aura run` and built programs.

## Maintained Execution Paths

Aura 0.3 maintains one checked source language and two runtime representations:

- `aura run` lowers the entry package to MIR and executes it in the MIR runtime.
- native direct builds lower MIR-compatible program structure to native code linked with the direct runtime.

`aura build --backend direct` requires direct emission and fails if the program cannot be emitted. The default `--backend auto` first attempts direct emission and may fall back to a native launcher containing serialized MIR plus the MIR runtime when direct emission fails. This fallback is a packaging choice, not a third language semantics.

Both runtime representations MUST agree on maintained observable behavior. Backend parity tests compare the eligible runtime fixture corpus.

Duration values remain signed 128-bit nanosecond counts across both paths.
Direct code passes a Duration literal as its exact low and high 64-bit
two's-complement limbs, and the native runtime reconstructs the same i128
value. This ABI transport never narrows through milliseconds or a host timer
type.

## Entry Module Execution

After successful checking, an entry module runs in one of two modes:

1. If it has executable top-level statements, those statements execute in their stored source order. The file cannot also declare a local `main`.
2. Otherwise, a local `main()` is called when present. It returns `None` or `int32`.

An imported function named `main` is not an entrypoint. Imported module top-level statements do not execute as import side effects in Aura 0.3.

For `aura run`, a returned `int32` is passed to the host process as the requested exit status; `None` means success. A built native program follows the same entry result contract.

## Evaluation Order

Except for short-circuit boolean operators and control-flow constructs, subexpressions are evaluated left-to-right:

- a binary expression evaluates its left operand before its right operand
- collection literal elements are evaluated in source order
- a dictionary evaluates each key before its value and entries in source order
- f-string interpolations are evaluated from left to right
- an index evaluates its base before its index
- a slice evaluates its base, written start, and written end exactly once from
  left to right; omitted endpoints evaluate nothing
- a receiver is evaluated before call arguments
- every supplied call or constructor argument is evaluated in call-site source order
- a lambda acquires every capture from left to right when the lambda expression
  is evaluated: implicit and `own` captures copy or move, while explicit bare
  and `mut` entries begin loans before any later sibling expression
- a comprehension allocates its result, then evaluates clause iterables and
  filters in nested outer-major order before evaluating its textually leading
  output; a dictionary output evaluates its key before its value

Evaluating a copy place captures its copied value at that point. A non-copy
place selected as a binary left operand, index base, method receiver, or
indexed-assignment target remains borrowed until that operation has consumed
all of its inputs. A later shared borrow is permitted, but an overlapping
mutable borrow or consumption is rejected with `AU3002`; the diagnostic points
to both the conflicting access and the retained-borrow origin. This applies to
name roots and projected member places, and no backend may insert a hidden deep
clone. Each f-string interpolation is converted to its rendered `str` at
its own position before the next interpolation begins.
Each append preflights the maintained 64 MiB constructed-string limit. An
oversized result reports `AU4005`, does not evaluate a later interpolation,
and releases the partial output through ordinary failure cleanup.

A list or str slice captures its base, then any written start, then any
written end. A non-Copy base remains retained while the endpoint expressions
run. Negative endpoints are normalized once after those expressions complete;
both effective bounds are checked in `0..=len`, followed by the `start <= end`
check. A failure traps with `AU4003` and returns no partial value. A successful
slice copies the selected range in source order into a fresh owned `list` or
`str`. String endpoints count Unicode scalar values and locating them is
O(n). No maintained backend may substitute Python-style endpoint clamping or a
view into source storage.

All supplied arguments complete before any omitted default is evaluated. Each
supplied function/method argument or class-field expression is fully evaluated
before the next supplied expression begins. Its copy or move result is captured
in the destination slot; a borrow-mode selection is established without a
clone and remains subject to the retained-borrow overlap rule above. Later side
effects cannot cause an earlier captured argument or field value to be re-read.
Omitted function parameters and class fields then evaluate their defaults in
declaration order when the call or construction occurs. Binding supplied values
to declaration slots never reorders them, and a supplied slot suppresses its
default. Each omission causes a fresh evaluation; a mutable default value is
not a process-global singleton. A shared-borrow parameter's default temporary lives until the call completes. An `own` parameter consumes its fresh default
temporary. Mutable-borrow defaults are statically rejected.

Enum-variant constructor arguments also evaluate in call-site source order.
Named arguments are then bound by name to the variant's declaration-order
payload slots; declaration-slot order never reorders their evaluation.

When two evaluated keys in one dictionary literal compare equal, the later value
replaces the earlier value and the key keeps its first insertion position.

`and` evaluates the right operand only when the left value is `true`. `or` evaluates the right operand only when the left value is `false`. Both operands have static type `bool`.

A lambda call evaluates arguments under its contextual structural function
type. A read-only or shared-loan closure may be called repeatedly. A
mutable-loan closure may be called sequentially through a mutable closure
place and writes directly to its captured source. A closure whose body
consumes a non-Copy owned capture is consumed by the call. Never-called and
called closure environments release owned values and loan registrations
exactly once on both maintained backends.

A comparison chain evaluates its operand expressions from left to right at
most once. It evaluates each adjacent link after obtaining that link's right
operand, stops at the first false link, and does not evaluate any remaining
operand. This applies equally to chains containing tuple `==` or `!=`: a tuple
used as a middle operand is evaluated once and read by both adjacent links.

An assertion evaluates its condition exactly once. A true condition skips the
optional message and falls through. A false condition evaluates the message
exactly once, then establishes the assertion failure before cleanup begins. A
trap in either operand occurs first. Assertion-triggered cleanup follows the
ordinary reverse-order rule, and a cleanup trap does not replace the assertion
diagnostic.

## Calls And Returns

A call evaluates and binds arguments, then transfers control to the target body
or runtime builtin. Explicitly owned non-copy arguments have been moved at the
call boundary; bare shared arguments remain owned by the caller and are
constrained for the duration of the call.

`return value` evaluates `value`, copies or moves it into the owned result,
runs active lexical cleanups, and returns to the caller. Reaching the end of a
`None` function returns `None`. A non-`None` function cannot pass static
checking if a reachable path falls through.

`return view [mut] place` instead hands one checked loan to the caller. The
declaration's `from` slot fixes its origin, and the caller continues the exact
selected projection without an unlock/relock or clone. Every other callee loan
ends before control returns. A trap before the handoff transfers no loan.

A recursive Aura call consumes one logical call-depth unit. The maintained runtime rejects execution after 256 nested Aura calls with a source diagnostic rather than allowing the host stack to overflow.

## Loan Lifetime And Cleanup

A local view begins a shared or mutable loan over one place and storage
generation. Both maintained backends resolve reads and writes through that
identity; mutable writes happen immediately. Static final-use analysis may end
the loan before lexical scope ends. Branch joins and loop edges extend regions
conservatively, and every iteration releases its iteration-local loans.

Exit actions form one reverse-acquisition stack containing loan ends, closure
environment drops, mutable writebacks, match/iteration reconstruction, and
resource cleanup. Normal fallthrough, return, escaping loop control, `try`
propagation, maintained traps, and cancellation drain the required suffix.
Consequently a view into a resource ends before `close`, and an inner reborrow
ends before its parent. A task may retain a loan to its own storage across a
scheduler suspension, but no descriptor or loan closure may cross to another
task or worker.

## Foreign Calls

A direct extern call evaluates its arguments left-to-right, marshals them only
after ordinary call binding succeeds, and synchronously invokes the matching
process-global C symbol. The current Aura worker remains occupied until the
foreign function returns. A missing symbol or pre-call marshalling failure
prevents entry. Return validation occurs after the foreign function and cannot
roll back native side effects or mutable-byte writeback.

Empty `str`, `list[uint8]`, and `mut list[uint8]` views pass a null pointer
with length zero. A non-empty shared view passes a valid const pointer and byte
length. A non-empty mutable byte view uses a same-length scratch buffer:
initial bytes are copied in, then exactly that length is copied back after the
foreign function returns, even when later return-value validation produces an
Aura runtime error. The callee must not retain a pointer or read/write
outside the supplied length.

Aura diagnostics do not unwind through a foreign frame. A native abort,
signal, memory fault, or foreign unwind is not caught and may terminate the
process. The complete type, ownership, package, and safety contract is in
[FFI v0](/manual/ffi).

## Operators

Arithmetic is checked under the selected concrete numeric type.

- integer addition, subtraction, multiplication, power, floor division,
  remainder, checked left shift, negation, and casts reject overflow
- builtin integer `/` and `/=` do not reach execution because static checking rejects them
- for integers with nonzero divisor `b`, `q = a // b` is the mathematical quotient rounded toward negative infinity and `r = a % b` satisfies `a == q * b + r`; a nonzero `r` has `b`'s sign
- integer `//` or `%` by zero is a runtime failure; an unrepresentable floor quotient, including the signed minimum divided by `-1`, is integer overflow
- floating `/` is ordinary true division, except that a zero divisor is an explicit runtime failure rather than IEEE infinity or NaN
- floating `//` and `%` use the CPython-compatible divmod correction: start from the host remainder and `(a - remainder) / b`; when a nonzero remainder's sign differs from `b`, add `b` to the remainder and subtract one from the provisional quotient; give a zero remainder `b`'s sign; for a nonzero quotient, take its floor and add one when the provisional quotient minus that floor is greater than `0.5`; preserve the quotient's division-result signed zero when it is zero
- floating `//` and `%` by either signed zero are runtime failures
- integer `**` is checked, defines `x ** 0` as `1` including `0 ** 0`, and
  rejects a runtime negative exponent with `AU4001`; floating `**` follows the
  maintained floating power domain and overflow classification
- `&`, `|`, `^`, and `~` operate on the declared fixed-width integer bit
  representation; all shift counts require `0 <= count < width`
- signed `>>` is arithmetic and unsigned `>>` is logical; `<<` is checked,
  while `wrapping_shl` discards high bits and `saturating_shl` clamps at the
  declared bounds
- `wrapping_shr` and `saturating_shr` have the same result as ordinary `>>`
  after the common count check
- `divmod(a, b)` computes the same corrected quotient and remainder together from one evaluation of each operand; a zero divisor is `AU4004`
- `round(integer)` preserves the exact integer type; `round(float)` uses ties-to-even and returns `int64`, with NaN, infinity, and out-of-range results classified as `AU4002`
- ordinary floating operations otherwise use host IEEE-754 `float32`/`float64` behavior, including possible runtime NaN results from operations such as square root of a negative value
- integer `.to_float()` converts to `float64` with IEEE-754 round-to-nearest, ties-to-even and may round; integer `as float32` or `as float64` retains its exactness check and fails instead of rounding
- Duration addition, subtraction, and multiplication operate on signed 128-bit nanoseconds and reject overflow with `AU4002`
- `Duration // int64` returns a Duration whose signed nanosecond count is the mathematical quotient rounded toward negative infinity; a zero divisor is `AU4004` and the signed-minimum divided by `-1` is `AU4002`
- string `+` creates a new concatenated `str`
- numeric Array `+`, `-`, and `*` traverse exact-shape row-major buffers and
  return fresh owned storage; float Arrays also support `/`
- ordinary integer scalar and Array arithmetic remains checked; the explicit
  `wrapping_*` methods use fixed-width modular arithmetic and `saturating_*`
  methods clamp to the declared width
- Array rank-zero/negative-dimension construction, `from_list` count mismatch,
  exact-shape/rank mismatch, and empty reductions use `AU4007`;
  shape-product/element-count overflow and allocation failure use `AU4005`;
  direct coordinate and first-axis-slice bounds failures use `AU4003`
- floating Array reductions visit row-major elements left to right with
  deterministic dtype rounding and NaN propagation; `mean` accumulates in
  `float64`, and no reassociation or vectorized reduction order is promised

Trait-backed operators invoke the selected trait implementation method with
ordinary receiver, argument, move, borrow, and runtime-error behavior. `/` may
invoke `Div.div` for an applicable non-numeric user type. `//` and `//=` invoke
`FloorDiv.floor_div` when no builtin numeric or `Duration // int64` rule
applies.

`==` and `!=` perform structural equality for maintained plain values and collections. Resource/handle identity is not a portable substitute for an application identifier; programs should use documented resource data rather than depend on equality of runtime handles.

More precisely:

- numbers, booleans, strings, durations, ranges, enum values, classes, datagrams, and HTTP responses compare by represented value
- tuples compare corresponding element values from left to right using
  ordinary equality, recursively for nested tuples, and stop at the first
  unequal element
- vectors compare element-by-element in order
- maps and sets compare by contents and ignore insertion order
- floating equality follows IEEE behavior, so a NaN value is not equal to itself
- queue/task handles, random generators, and live file, process, listener, stream, exchange, supervisor, and WebSocket values compare by shared runtime identity

Equality is defined only after static typing has established compatible
operand types. Tuple equality specifically requires the same static tuple type.
It reads both complete operands and consumes neither, including tuples with
non-copy elements. Runtime element-type, transport, or backend metadata carried
with a tuple value is not compared; it cannot change the recursively determined
value result. Operand expressions retain their ordinary ownership effects; the
equality operation itself adds no move of the resulting tuples.

Tuple `!=` is the logical negation of tuple `==`. Tuple `<`, `<=`, `>`, and
`>=` remain static errors; Aura does not define lexicographic or
metadata-based tuple ordering.

## Value Rendering

`print`, f-string interpolation, and scalar `.to_string()` use Aura's maintained value rendering where applicable. Strings render as their contents without quotes and `None` renders as the empty string. A directly printed `float32` or `float64` uses the shortest decimal spelling that round-trips to the same value in its source type. Integral finite values retain a decimal marker, scientific notation is used when it is shorter, and signed zero remains `-0.0`. A Duration renders as an exact decimal millisecond value with an `ms` suffix, using at most six fractional digits and trimming trailing fractional zeros; for example, `2s` renders as `2000ms` and `1ms // 3` renders as `0.333333ms`. This rendering policy is accepted under ADR-0019.

Lists render as `[a, b]`, non-empty sets as `{a, b}`, empty sets as `set()`,
and dictionaries as `{key: value}` in their defined order. Class values render
as `Class(field=value, ...)`; enum values render as `Enum.Variant(...)`.
Nested strings remain unquoted, so this display form is for people and is not
a round-trippable serialization format. A deterministic random generator
renders exactly `<rng>` without exposing or advancing its state. Live
resources render opaque labels such as `<file>` or `<tcp-stream>` without host
identifiers.
An FFI opaque handle renders as `<opaque TypeName>`, using its canonical Aura
type name and never exposing the foreign pointer address.

## Assignment And Mutation

A simple assignment ordinarily evaluates the right side before creating or
updating the target. Simple dict indexed assignment is the deliberate exception:
it evaluates and captures its owned key, consuming it when non-copy, before
evaluating and consuming the assigned value. A side effect in the value
therefore cannot retarget the write. Simple dict assignment accepts any `V`.
Reassignment preserves the target's type. A compound assignment selects its
target place once and uses exactly the corresponding binary operator dispatch,
including an applicable user-defined operator trait for a root or projected
target. For a copy target, it captures the current copied value before
evaluating the right operand and stores the operator result into the originally
selected place, so right-side effects neither change the left operand nor
retarget the store. A non-copy root or projected target remains borrowed across
the right operand; overlapping mutable borrow or consumption is rejected with
`AU3002`. Direct indexed compound assignment requires a copy `list` element or
`dict` value. A non-copy indexed element is rejected rather than implicitly
cloned or destructively moved, with `AU3006`. A non-copy direct indexed read is
rejected with `AU3005`.

Field and index assignment mutate the selected place. List indices are
zero-based. Simple dict assignment replaces an equal existing key or adds a new
entry; an absent key is therefore not a simple-assignment failure. Compound dict
assignment requires an existing key and traps with `AU4003` if its initial read
finds none. Failed checked mutation leaves the operation incomplete and
produces its documented runtime failure or typed error.

Moving a field marks that field unavailable while leaving disjoint fields usable. Reassigning the exact moved place reinitializes it.

## Collections And Iteration

`list` preserves element order. `dict` uses insertion order for its `keys()`,
`values()`, and `items()` projections. Replacing an equal dict key,
including from a later literal entry, retains that key's existing slot. `set`
uses an insertion-oriented runtime representation, but its public order is not
promised. Algorithms should rely on ordering only where the relevant API
promises it.

`list.map` and `list.filter` traverse from first element to last and produce
their eager result in that order. Their shared receiver remains unchanged.
`map` invokes its callback once per source element and owns each returned value
in the new list. `filter` invokes its predicate once per source element and
clones accepted elements into the new list.

Natural and keyed `list.sort` calls mutate their receiver into a stable order:
equal elements or keys retain their prior relative order. Keyed sorting evaluates its key
function exactly once for every element, from first to last, and stores all
keys before moving any receiver element. A trap during key evaluation
therefore propagates before mutation and leaves the complete source order
unchanged.

Bare iteration over a `list` or `set` retains and freezes the selected collection
for the loop and yields shared element access. `own` iteration moves the
collection into a loop-private source once at entry and yields owned elements;
reinitializing the consumed source binding in the body does not switch or
truncate the active iteration. That one-time source selection is accepted
under ADR-0017 and does not alter ADR-0006's ownership modes. `mut` iteration
over a mutable list grants exclusive element access with writeback and retains
the collection; mutable set iteration is rejected. Range iteration yields
independent `int32` values from `start` inclusive to `end` exclusive. Explicit
`mut` and `own` Range modifiers are rejected with `AU3004` because there is no
place access or ownership transfer to modify; use the bare form.

Queue iteration receives items until the queue closes, cancellation is observed, registered producers complete cleanly with no more items, or an unread sibling-task failure ends the surrounding group. It is a scheduler operation rather than iteration over a snapshot. Each item arrives already owned by the loop binding; explicit `own` and `mut` modifiers are rejected because neither the received value nor the copyable Queue handle has a place-iteration ownership mode to modify. Under accepted ADR-0017, the bare form evaluates and copies its Queue handle once at loop entry. This does not freeze the source binding: rebinding that variable in the body is allowed, but later iterations continue receiving through the captured handle. ADR-0006's ownership carve-out is otherwise unchanged.

### Comprehensions

A list, set, or dictionary comprehension creates a fresh empty owned result and
executes its clauses like nested bare loops. The first source is selected once.
For each selected item, filters execute left to right and stop that item at the
first `false`. Each inner source is selected once for every combination that
survives the earlier filters. The traversal is outer-major: the complete inner
traversal for one outer item finishes before the next outer item begins.

At an innermost surviving combination, the list/set element or dictionary key/value
is evaluated and inserted. The output is written first in source but executes
last. A dictionary captures its key before evaluating its value. Set
deduplication and dictionary equal-key replacement use their literal/storage contracts.

Every clause inherits the selected bare-loop behavior above. List/set sources
remain shared and frozen through downstream filters, sources, and output.
Range targets are copy `int64`. `enumerate` and `zip` retain their lockstep
rules. Queue copies its handle for the clause and yields each received item
owned; a Queue comprehension ends only when ordinary Queue iteration ends.

Insertion owns its output. Copy values copy; owned non-Copy values move; a
shared non-Copy list/set element needs an explicit clone-safe clone. Each
reached lambda creation uses ADR-0037. A trap or `try` propagation destroys the
partial result and all active temporary sources exactly once before continuing
the ordinary failure or early-return path.

## Pattern Matching

The scrutinee is evaluated exactly once. Arms are considered in source order. The first matching arm executes.

- `match own` consumes a non-copy scrutinee place
- bare `match` leaves the scrutinee owned and exposes shared payload borrows for non-copy data
- `match mut` permits payload mutation and writes the reconstructed enum value
  back on normal arm exit, `return`, `break`, `continue`, and `try`
  propagation
- literal patterns compare against the scrutinee value
- `_` always matches and binds nothing

A match expression evaluates only its selected arm and produces that arm's value. Static exhaustiveness ensures a checked match has a selected arm for every permitted input.

## Conditional Expressions

For `value if condition else alternative`, the runtime evaluates `condition`
first and exactly once. A true result evaluates and produces `value`; a false
result evaluates and produces `alternative`. The unselected arm performs no
calls, moves, mutations, I/O, allocation, or runtime failures.

Static checking still analyzes both arms and merges their ownership effects.
This conservative merge prevents later use of a non-copy value that may have
been moved on the selected path. MIR and direct lowering use an explicit
condition branch and a single typed join value; a backend must not eagerly
evaluate either arm. When the surrounding operation takes a shared borrow, the
join does not consume the selected source value, so both source owners remain
available after the borrow ends.

## `try`

`try expression` evaluates one `Result[T, E]` value:

- `Ok(value)` produces `value` and continues the enclosing expression
- `Err(error)` returns immediately from the enclosing function

When the enclosing function uses a different error type, the implementation invokes the applicable `From` trait conversion before returning the error. Active `with` scopes are cleaned up during this early return.

## Resource Lifetime And Cleanup

`with` creates an active cleanup registration after its resource expression succeeds. Leaving the body invokes `close(mut self) -> None` exactly once through that registration.

Cleanup runs on:

- normal fallthrough
- `return`
- `break` or `continue` that exits the scope
- `try` error propagation
- a maintained Aura runtime failure

Nested active cleanups run in reverse registration order. If a body is already failing and cleanup also fails, the original body diagnostic remains primary. Resource-specific `close()` behavior is defined in its API chapter.

Explicitly closing a resource before scope exit is permitted only where the resource contract makes repeated close harmless; otherwise programs should let the lexical owner perform cleanup.

These cleanup rules apply while Aura control flow or a maintained runtime
failure exits the task through the language cleanup machinery. Internal
scheduler abandonment is a last-resort containment path used when the whole
scheduler stops with a child still suspended, such as after root completion or
a fatal reactor failure. It marks the remaining task cancelled and releases
scheduler-owned and direct-runtime host state, but it does not invoke arbitrary
Aura cleanup thunks. A direct generated stack may be reset on that path
because it cannot be safely Rust-unwound across Cranelift frames. Programs must
use structured `TaskGroup` scopes rather than depend on scheduler abandonment
as a cleanup mechanism.

## Tasks And Scheduler

Aura lightweight tasks run on cooperative pinned scheduler workers. The
runtime uses the available parallelism reported by the host by default; the
`AURA_WORKERS=<positive integer>` environment override selects an explicit
count. Each child receives a stable assignment when it is spawned. Its
coroutine stack never migrates and the runtime does not steal tasks between
workers. Operations such as queue waits, task waits, sleep, nonblocking
sockets, and scheduler-integrated I/O yield instead of creating one OS thread
per Aura task. A task can also yield explicitly with `yield_now()`. The
generic blocking-I/O pool may execute host calls concurrently, but those
service workers do not run Aura code.

The compiler inserts a cooperative scheduling check on every semantic loop
backedge. Reaching the ordinary tail of a `while` or `for` body participates,
as does `continue`; `break`, `return`, and another exit that leaves the loop do
not traverse its backedge. These checks let a tight loop eventually return to
the scheduler so ready timers, Queue operations, and socket work are not
starved indefinitely.

A loop safepoint is not preemption and does not inspect cancellation. A single
long iteration can still delay every sibling pinned to the same worker until the body reaches its
backedge, and long straight-line CPU work with no loop or scheduler operation
can do the same. Use `cancelled()` when the task must observe cancellation, and
use `yield_now()` when the program needs an explicit scheduling point between
chosen chunks. Neither automatic nor explicit yielding specifies which ready
local task runs next.

MIR execution amortizes the cooperative yield with 8 units of function-local
loop fuel. Direct native code uses 4,096 units and replenishes the fuel after
yielding. A program proven to have no possible sibling Aura task elides the
runtime check entirely. These backend strategies may produce different valid
interleavings; scheduling order is not observable language order. An ordinary
lightweight task requests 512 KiB of writable coroutine stack. The
`TaskGroup.start_with_stack` and `start_soon_with_stack` methods accept an
exact `int64` request from 256 KiB through 64 MiB inclusive. Accepted requests
are rounded upward to the host page size and guard-protected; out-of-range
requests are rejected rather than clamped. This surface is Provisional under
ADR-0032. The 256 KiB lower bound is an explicit minimum for measured shallow
tasks, not the general default. The complete compiled Aura HTTP example,
including its MIR/direct language-execution frames, proved unsafe when
256 KiB was the global task default and succeeds with the 512 KiB default.
The separate isolated runtime round trip that forces protocol callers to
256 KiB proves that service workers own the deep host protocol frames; it
does not measure the full compiled task stack.

`yield_now()` places the current lightweight task back in its worker's ready
Set and returns when that worker selects it again. It gives other runnable
local tasks an opportunity to proceed without waiting for an event or
deadline, but it does not migrate the coroutine, steal work, guarantee that a
different task runs, or specify a ready-task order. With no current
schedulable lightweight task, it returns without effect.

The scheduler owns a persistent event reactor. Nonblocking descriptors remain
registered across scheduler turns, deadlines are ordered in a timer heap, and
Queue, task-completion, and blocking-pool events notify the responsible ready
queue directly, including across workers. Registration uses a
check-subscribe-recheck protocol with wait epochs,
so a readiness edge racing with suspension is not lost and stale wakeups do
not resume a later wait. If no task is ready, the scheduler blocks until the
next event or deadline; there is no periodic park tick.

`select(source, ...)` evaluates its Queue, Task, and relative-Duration sources
once from left to right, then uses one composite wait under that same
protocol. Current-task cancellation wins; otherwise each arbitration probes
sources by their original zero-based index and commits the first ready source.
A wake is only a request to arbitrate, so a Queue item lost to another
consumer before the selecting task resumes does not create a false outcome.
Winner commit consumes at most one Queue item or selected Task result and
removes every losing registration. All Duration sources share one base instant
established after evaluation and validation.

`control.retry` invokes its worker immediately for the first attempt. An `Ok`
returns immediately. An `Err` is retained only until the helper determines
whether another attempt exists. When one does, the helper waits for the current
backoff unless it is zero, invokes the next attempt, and doubles the delay only
when another retry could still use it. Every `Err` is retryable. The final
permitted `Err` is returned exactly, without an extra sleep or multiplication.
Worker traps and checked Duration overflow propagate as runtime diagnostics.
Cancellation of the current task propagates through the helper and its
scheduler-aware delay instead of being represented as the most recent `Err`.

Queue and Task handles are the maintained cross-worker communication surface.
Every other capture and result is owned `Transfer` data, preserving the
share-nothing boundary. Cancellation and diagnostic context are installed per
task and remain isolated across workers.

Scheduling order among multiple ready tasks, completion order among
independent tasks, and program-output order are not specified. Programs
coordinate through queues, task results, cancellation, and other documented
synchronization rather than timing assumptions. Aura exposes no worker
identity or affinity API. Task execution is multicore; preemption and work
stealing are unavailable, and speedup depends on the workload.

Starting a child from a running task does not mutate the live scheduler
through an alias. The runtime first prepares the child's guarded stack and
task state, then transfers that prepared request to the scheduler for
admission. If preparation fails, the start fails synchronously before a handle
is returned and no child is admitted. A task may immediately wait on a
successfully returned child handle, including inside a nested `TaskGroup`.
The current admission broker preserves its own FIFO request order, but that is
an internal safety property; child execution order remains unspecified.

Deep HTTP parsing/construction, TLS operations, and maintained Unix WebSocket
protocol steps run on a distinct bounded protocol-step service. Its two named
workers have 2 MiB native stacks and share a 64-job queue. A job owns its
protocol state for one bounded, nonblocking library step. The coroutine waits
for the state to return before it observes cancellation or waits for descriptor
readiness again, so there is never an abandoned protocol state with two
owners, and no resource mutex remains held across the worker wait. Reactor
readiness, absolute deadlines, and cancellation remain scheduler-side
concerns. This protocol-step pool is lazily initialized and shared by every
lightweight scheduler. Its workers intentionally live until process exit;
Aura 0.3 has no protocol-pool shutdown or join surface. The non-Unix
WebSocket fallback retains its compatibility path. Resolver, listener-bind,
and file reads use the generic blocking-I/O pool.
`AURA_BLOCKING_WORKERS=<positive integer>` selects its exact worker count
without clamping; otherwise host parallelism is used with fallback `4` and a
derived `2..=8` clamp.
`AURA_BLOCKING_QUEUE_CAPACITY=<positive integer>` optionally bounds accepted
pending jobs. Capacity excludes running jobs and callers waiting for admission,
and omission preserves an unbounded queue. TLS asset bytes are read there
before PEM parsing and rustls construction run on protocol workers.
The generic pool is also process-global. Its settings are read once by the
first runtime preflight and remain immutable for the process lifetime; that
preflight starts no worker. First blocking submission creates the complete
configured set, which production reuses until process exit without an Aura
shutdown/join surface.

Dynamic `json.parse` uses a third, independent process-global service with two
2 MiB-stack workers and total in-flight capacity two. A task reserves capacity
before making the fallible owned copy of its parse source; saturation parks a
lightweight task through the scheduler rather than spinning. Once admitted,
synchronous `json.parse` waits through codec completion, so cancellation is
observed at the task's next ordinary cancellation boundary rather than
abandoning the codec job. Its dependency-owned recursive parsing runs on the
service stack, while runtime materialization, JSON-aware cloning/rendering,
and dump conversion/emission use iterative traversals. The direct backend
waits for admission without value-table access, then holds read access only
long enough to copy the source and releases it before submission and
completion waiting. The bounded `json.is_valid` and `json.parse_string_map`
operations remain caller-side and do not use
this service. Codec workers are process-lifetime and have no Aura 0.3
shutdown or configuration surface.

`Queue[T]` is a copy handle to shared runtime state. Under Accepted
ADR-0033, a `Task[T]` handle is copyable only when its result is repeatable;
every task handle remains transferable. Copying an allowed handle does not
duplicate the underlying task or queue.

Starting a task first copies or moves every argument into task-owned capture
storage. The child then applies the target's declared parameter capability to
that capture: a bare parameter borrows it, and an `own` parameter consumes it.
Mutable targets are rejected statically.

A task stores its completed result. Copy results, Queue handles, and
recursively repeatable Task handles permit repeated observation. Every other
transferable result has a unique observation right;
each direct result call consumes it on every outcome, and multi-task waits
consume the complete task list. `wait_any` abandons the unchosen rights.
Task captures, results, and Queue payloads must also satisfy the structural
Transfer check before the child is admitted to its spawn-time pinned worker.
Queue and Task handle state is synchronized for cross-worker notification and
observation; every other value crossing the boundary remains owned and
share-nothing.

The runtime also protects a non-repeatable stored result with an atomic
one-winner claim. A failed second claim traps with `AU4001` and
`task result has already been observed; non-repeatable task results allow
exactly one observing attempt`. This is defense in depth for backend defects
or foreign handles, not a replacement for static ownership diagnostics.
TaskGroup scope
cleanup joins, abandons, or accounts for a child without observing its
successful result: cleanup does not claim the right or make the value
available to another observer.

## Task Groups And Failure Observation

`TaskGroup` owns children started within its scope.

- normal scope exit waits for children that are making bounded progress
- a child blocked in an unbounded group-owned wait is cancelled only when the
  runtime's live wait graph has no reachable waker
- explicit `cancel()` signals cancellation and wakes scheduler-aware waits
- a task failure observed through its `Task` result does not also abort the group as unread
- an unread child failure aborts the group scope and wakes queue iteration/waits that depend on that group

Cancellation is cooperative. Pure CPU code observes cancellation through
`cancelled()`; `yield_now()` is a scheduling point but does not inspect
cancellation. Compiler-inserted loop safepoints likewise do not inspect
cancellation. Scheduler-aware blocking operations receive cancellation context
directly.

Queue reachability is based on live tasks known to hold `Queue` handles, not an
elapsed-time threshold. A sender parked on a full open queue remains joinable
while a live receiver can drain it; a receiver parked on an empty queue remains
joinable while a live sender or another live owner of the open queue can send
or close it. The task performing the join is not counted as its own child's
waker, because it cannot use its queue handle until the join returns. Cycles
made only of mutually blocked waits have no reachable waker and are cancelled.

## Host I/O And Cancellation

Socket-backed network resources use nonblocking descriptors with persistent
reactor registration. Their timeout and cancellation outcomes are documented
per operation.

Converting a Duration to a host wait is a checked boundary. Negative values,
values outside the host timer range, and durations whose addition to the
current instant would overflow are invalid inputs. Deadline overflow never
silently becomes an unlimited wait. An API with an `io.Error` carrier reports
`InvalidInput`; a process-error carrier reports
`process.Error.Io(io.Error.InvalidInput)`; an API without either typed carrier
traps with `AU4001`. This host-boundary classification is accepted under
ADR-0019.

Filesystem operations and some host operations run on the generic blocking-I/O
pool under Accepted ADR-0035. When its optional pending-queue bound is full,
Aura tasks wait for
admission through the scheduler in FIFO order instead of blocking a pinned
worker. Cancellation or deadline expiry before queue insertion prevents the
operation from being submitted. Once inserted, the operation cannot be
retracted: cancelling the Aura task cancels its wait, not an
operating-system call already pending or executing. A cancelled write or other
side-effecting operation may therefore complete in the host after Aura has
stopped waiting, with its late result discarded. Programs requiring
transactional cancellation must write to a temporary artifact and commit it
explicitly. Bounding accepted pending jobs does not bound admission waiters or
guarantee unrelated blocking-I/O progress while every configured worker
remains stuck.

Process cancellation and close operations signal/terminate according to the process API. Group-enabled processes extend those operations to the maintained host process group behavior.

## Standard Streams

`print` and `io.write` preserve call order within one task. Concurrent writes may interleave at operation boundaries; no global record transaction is implied unless the application serializes output.

`aura run` streams standard output while the program runs. If a later runtime failure occurs, already written output remains observable and the diagnostic is written to standard error. A broken stdout pipe is treated as clean early termination by the CLI.

## Runtime Limits

The maintained resource size, header, frame, timeout, and platform limits are normative for Aura 0.3 and are collected in [Current Limits](/manual/current-limits). An implementation MUST reject or return a typed error when a limit is exceeded; it must not allocate without bound or hang indefinitely where the API supplies a deadline.

## Determinism

Pure expression evaluation, ordinary control flow, and collection operations are deterministic for the same values. The following are external or scheduler-dependent and therefore not generally deterministic:

- task interleaving among simultaneously ready tasks
- wall/monotonic clock readings
- process identifiers, exit timing, and host scheduling
- network arrival order and peer behavior
- filesystem enumeration supplied by the host
- operating-system secure random output
- the exact wording of host operating-system errors

An explicitly seeded `random.Rng` is deterministic. Its xoshiro256** sequence,
integer/float mapping, and shuffle order are
fixed for Aura 0.3.x and specified in [Randomness Module](/manual/randomness).
Secure random calls are external effects and never draw from that stream.
Aura converts host effects into typed values and ordering primitives where
practical, but does not pretend the host environment is deterministic.
