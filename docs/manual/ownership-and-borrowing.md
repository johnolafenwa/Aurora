# Ownership And Borrowing

Aura statically tracks whether an operation copies, moves, shares, or mutates a value. The rules apply to local bindings, parameters, method receivers, fields, supported indexed operations, collection iteration, pattern matching, task starts, and resources.

A **place** is a storage location such as a local binding or field path. A copy use duplicates a value. A move use transfers ownership from a place. A borrow temporarily grants access without transferring ownership.

## Copy Types

Copy values are duplicated by assignment, by-value argument passing, returns, collection insertion, and other value uses. The source remains usable.

Current copy categories are:

- all signed and unsigned integer types
- `float32` and `float64`
- `bool`
- `Duration`
- `Queue[T]` handles and, under Accepted ADR-0033, `Task[T]` handles whose
  result type is repeatable
- tuples whose every element type is copyable
- `copy class` values whose fields are copyable
- user enums whose every declared payload type is statically copyable
- `Option[T]`, `Result[T, E]`, `SendError[T]`, and `QueueReceive[T]` when every payload type is copyable

```aura
a = 1
b = a
print(a)
print(b)
```

`Queue[T]` is a copy handle to shared runtime state. Accepted ADR-0033 makes
`Task[T]` copyable only when `T` is repeatable. Copying an allowed handle does
not duplicate the underlying queue, task, queued values, or stored result.

## Move Types

Move values transfer ownership on by-value use. Current move categories include:

- tuples with at least one move element
- `str`
- `list[T]`, `dict[K, V]`, and `set[T]`
- `random.Rng`
- ordinary user classes
- user or builtin enums with any move payload
- `TaskResult[T]`, `SelectOutcome[Q, T]`, `WaitAny[T]`, and `WaitAll[T]` even
  when every payload type is copyable
- `Range`
- `TaskGroup`
- file, process, supervisor, pipe, and network resources

```aura
def main():
    name = "aura"
    other = name
    print(other)
    # print(name) would be rejected: name was moved
```

A generic payload whose declared type is an unconstrained type parameter is not assumed copyable. The canonical category list and builtin generic types are in [Types](/manual/types#copy-and-move-categories).

## Operations That Move

A non-copy value is consumed when used in an owned position, including:

- assignment into a new owned binding
- an `own` function parameter or an `own self` method receiver
- a by-value return
- a class or enum payload, collection literal, mutating collection method, or
  simple dict indexed assignment that stores the value
- by-value enum matching
- `own` iteration over `list[T]` or `set[T]`
- the resource expression of `with`
- a task-start argument copied or moved into task-owned capture storage

An expression is evaluated before its move is recorded at that boundary. Aura also rejects an expression that tries to borrow and move overlapping places in incompatible subexpressions.

List slicing is a clone-producing shared read. It does not move elements.
`values[start:end]` retains `values` while the endpoint expressions run, then
copies Copy elements or clones clone-safe non-Copy elements into a fresh owned
list. A type containing `random.Rng` cannot be sliced because it cannot be
safely duplicated; a non-repeatable Task observation right likewise cannot be
duplicated. String slicing copies a Unicode-scalar range into a fresh owned
`str`. Neither result aliases its source or acts as an assignable place.
After creation, that fresh result follows the ordinary move rules for any
other owned non-copy list or str value.

Unpacking a non-copy tuple is one whole-source move. The target leaves receive
owned elements, but the source does not become a set of independently reusable
positional partial-move places. A later use of the source is rejected with the
ordinary move diagnostic. Unpacking a copy tuple copies its elements and keeps
the source usable.

## Borrow Forms

| Form | Meaning |
| --- | --- |
| `value: T` | Shared access for every `T`; copy bits may be passed directly as an implementation detail. |
| `value: own T` | Explicit owned ordinary parameter. |
| `value: mut T` | Exclusive mutable borrowed ordinary parameter. |
| `self` | Shared method receiver and the default receiver spelling. |
| `own self` | Consuming method receiver. |
| `mut self` | Exclusive mutable method receiver. |
| `for value in collection:` | Default shared iteration for `list` and `set`. |
| `for value in own collection:` | Consuming iteration for `list` and `set`. |
| `for value in mut collection:` | Mutable-borrow iteration where supported. |
| `match value:` | Shared borrowed pattern matching. |
| `match own value:` | Consuming pattern matching. |
| `match mut value:` | Mutable borrowed pattern matching with writeback. |
| `-> T` | Owned result. A copy result is an ordinary independent copy. |
| `view name = place` | Shared view whose lifetime is inferred from its final use. |
| `view mut name = place` | Exclusive mutable write-through view. |
| `-> view T from source` | Shared returned view tied to one receiver or parameter. |
| `-> view mut T from source` | Mutable returned view tied to one mutable receiver or parameter. |

The spelling asymmetry is intentional: parameter ownership occupies the type position as `value: own T`, parallel to `value: T`, while loop ownership prefixes the iterable as `for value in own values` because loops have no type position.

Call sites never prefix arguments with a capability. The parameter or receiver
declaration selects the mode:

```aura
def render(name: str) -> str:
    return name.to_upper()

name = "aura"
print(render(name))
print(name)
```

A shared borrow permits reading but cannot be moved and cannot be used as a mutable place. A mutable borrow is exclusive and may mutate its source through the borrowed binding.

Shared-borrow and `own` parameters may have defaults. An omitted shared default
creates a fresh temporary that lives through the call; an omitted owned default
creates a fresh value that the call consumes. A `mut` parameter cannot
have a default, even for a copy type: its caller-invisible temporary would make
every mutation a silent lost write. Require the caller to pass a mutable value,
or take `own T` and return the result.

```aura
def add_name(names: mut list[str], name: own str):
    names.append(name)

def main():
    mut names = list[str]()
    add_name(names, "Ada")
```

Only a mutable place can satisfy `mut T`. A local becomes mutable with `mut`; a
field is mutable when its base place is mutable; a `mut` receiver or parameter
is a mutable place inside its body. Parameter bindings themselves are not
reassigned.

## Local Views And Place Identity

`view` creates a non-owning alias to one addressable place. Supported places
are local roots, parameters, receivers, existing views, class-field paths,
and fixed tuple positions. A source is evaluated once. Collection indexes,
map keys, set elements, Queue receives, Range values, and computed temporaries
do not have view identity in Aura 0.3.

    class Counter:
        value: int64

    def main():
        mut counter = Counter(value=1)
        view mut value = counter.value
        view mut nested = value
        nested = nested + 1
        print(counter.value)

The mutable assignment writes immediately to `counter.value`; ending the loan
does not perform delayed copy-back. A shared view permits reads. A mutable view
is exclusive and blocks every overlapping source access except through itself
or a contained reborrow. Ancestors overlap descendants, while distinct fixed
fields and tuple positions are disjoint. A view binding cannot be rebound,
moved, cloned as a descriptor, stored in an aggregate, or sent across a task
or Queue boundary.

Loan regions begin at view creation and end after the final possible use,
conservatively across branches and loops. Their lexical scope is only an upper
bound. Rebinding, moving, cleaning up, or structurally mutating an overlapping
source is rejected while the loan remains live. Scope exits, `return`,
`break`, `continue`, propagated errors, traps, and cancellation release every
loan they leave in reverse acquisition order.

## Call-Boundary Exclusivity

All receiver and argument accesses for one call are checked together. Shared borrows may overlap other shared borrows. Every mutable borrow and every move must be exclusive with respect to an overlapping place.

```aura
class Acc:
    value: int32

    def add_from(mut self, source: Acc):
        self.value += source.value

def main():
    mut acc = Acc(value=1)
    # acc.add_from(acc) is rejected: mutable self overlaps shared source
```

Place overlap is prefix-based for tracked name/field paths. `value` overlaps `value.field`, and `value.field` overlaps `value.field.inner`. Distinct roots do not overlap. Sibling fields such as `pair.left` and `pair.right` are distinct when the checker can prove those paths.

The same exclusivity rule applies when one argument consumes a value and another argument borrows it. Argument evaluation order does not make an otherwise overlapping call legal.

## Partial Moves And Reinitialization

Moving a non-copy field from an owned class marks that field path moved while preserving disjoint fields:

```aura
class User:
    name: str
    id: int32

def main():
    mut user = User(name="Ada", id=1)
    name = user.name
    print(user.id)

    user.name = "Grace"
    print(user.name)
```

The complete class value cannot be used while any field remains moved. Assigning the exact moved field reinitializes that path. Assigning a fully moved mutable binding reinitializes the binding and clears its moved-field state.

Moving a non-copy field through a shared or mutable borrow is rejected because the borrower does not own the containing value:

```aura
def bad(user: User) -> str:
    return user.name # rejected
```

Use `.clone()` for a new owned value when the type supports it, or expose an owner method that performs the read or mutation:

```aura
def good(user: User) -> str:
    return user.name.clone()
```

## Flow-Sensitive Move Checking

Branches and match arms are checked independently. At a reachable join, a binding or field is considered moved if it may have been moved on any incoming path unless it was definitely reinitialized on all relevant paths.

Moves inside a loop need an additional invariant: the loop may execute again. Aura rejects a first move or partial move from an outer value in a repeatable loop when the next iteration could reuse the moved place. Limited constant-boolean reasoning recognizes forms based on `true`, `false`, grouping, and `not`; programs should not depend on broader compile-time evaluation.

Block-local bindings do not escape their branch, arm, loop, or `with` body. See [Names And Scopes](/manual/names-and-scopes#block-scope-and-control-flow).

## Owned Returns

Every function return transfers an owned value to its caller. Copy values are
ordinary independent copies. A non-copy return must come from an owned source:

```aura
def identity(value: int32) -> int32:
    return value

class User:
    name: str

def copy_name(user: User) -> str:
    return user.name.clone()

def into_name(user: own User) -> str:
    return user.name
```

A function can construct a fresh non-copy value, clone a clone-safe value,
accept an `own` parameter, or consume an owner through an `own self` method.

Shared or mutable access does not transfer ownership of a non-copy field, so
returning that field directly is rejected as an invalid move through access
the function does not own.

Every ordinary `-> T` result is owned. The detailed rules are in
[Functions](/manual/functions#owned-returns).

## Returned Views

A declaration can instead return a view tied to exactly one receiver or
ordinary parameter:

    class User:
        name: str

    def name(user: User) -> view str from user:
        return view user.name

    def name_mut(user: mut User) -> view mut str from user:
        return view mut user.name

The origin is part of the callable contract. A shared result may originate
from bare or mutable access; a mutable result requires a `mut` origin. An
`own` or defaulted parameter, callee local, temporary, or newly allocated
value cannot be an origin. A caller must supply an addressable origin and bind
the result with a matching `view` form. Trait implementations must use the
same receiver or parameter slot as the trait declaration even if parameter
names differ.

Different return paths may select different fixed projections of the declared
origin; the caller locks that origin conservatively while execution retains the
exact selected projection. A different root is `AU3010`. Ordinary `-> T`
remains an owned return and structural `def(...) -> R` types do not erase a
returned view's origin.

## Borrowed Pattern Matching

`match own` consumes a non-copy enum scrutinee. Bare `match` retains the enum
and gives non-copy payload bindings shared-borrow provenance:

```aura
result: Result[str, str] = Result.Ok("ready")

match result:
    case Result.Ok(value):
        print(value)
    case Result.Err(error):
        print(error)
```

`match mut` requires a mutable place. Its non-copy payload bindings are mutable
borrows, and mutations are written back by reconstructing the enum on normal
arm exit, `return`, `break`, `continue`, and `try` propagation. A nested
mutable match cannot overlap an already active mutable match. Reassigning the
exact scrutinee, its root, or an ancestor field invalidates payload bindings
tied to the old value. A write to a proven-disjoint sibling field does not
invalidate them.

Tuple patterns follow a smaller rule. A `match own` tuple match consumes the
whole non-copy scrutinee and gives owned leaf bindings. Bare `match` retains
the tuple and gives shared leaf provenance. Tuple patterns are rejected under
`match mut`; Aura does not reconstruct and write back recursive tuple
targets.

Payload bindings are arm-local and cannot shadow a visible binding. Match typing and exhaustiveness are specified in [Enums And Pattern Matching](/manual/enums-and-match).

## Borrowed Iteration

Bare `list` and `set` iteration retains the collection and yields shared-borrowed
non-copy elements. `for value in own collection` moves the collection once into
a loop-private source and yields owned elements. Reinitializing the consumed
source binding in the body cannot switch or truncate that active iteration.
That one-time source selection is accepted under ADR-0017.
`for value in mut values` requires a mutable list place and yields
mutable-borrowed elements.

The place selected by bare iteration is frozen against overlapping mutation
for the loop body.
Mutable-borrow set iteration is not supported; mutate a set through `add`
and `remove` outside borrowed iteration. Queue iteration receives values; it is
a scheduler operation, not a place traversal. The bare form copies the Queue
handle once at loop entry and yields owned items without freezing the source
binding; rebinding that source does not switch later receives. All three
explicit ownership modifiers are rejected. The one-time handle selection is
also accepted under ADR-0017. See
[Concurrency](/manual/concurrency).

When an iteration item is a tuple, recursive target leaves inherit the item
provenance. Shared collection iteration gives shared non-copy leaves, `own`
collection iteration gives owned leaves, and bare Queue iteration gives owned
leaves because it receives the item. A tuple target is rejected with `mut`
iteration; recursive mutable tuple writeback is not defined.

## Clone

`.clone()` explicitly creates another owned structural value where the maintained type exposes cloning:

```aura
name = "aura"
copy = name.clone()
print(name)
print(copy)
```

Text clones and collection copies create owned contents. Cloning a
runtime-backed resource handle does not necessarily create an independent host
resource; rely on the resource's documented API.

Not every move type supports cloning. `random.Rng` is deliberately
non-cloneable, and wrapping it in a class, enum, or collection does not make
the stored generator cloneable. Clone-producing collection reads and task
observations follow the same structural rule. Copying a `Task[T]` or `Queue[T]`
handle is different because it copies only the handle, not a stored `T`.

When a clone-producing operation depends on an unresolved generic type, the
callable acquires an inferred clone-safety obligation. Safe specializations
remain valid; a specialization that would duplicate `random.Rng` is rejected
with `AU3007`.

## Closures And Capture

Closure capture without a capture list is an ownership operation at lambda
creation. A referenced outer Copy value is copied into the closure environment.
A referenced outer non-Copy owned value is moved, so the source cannot be used
afterward unless the program cloned before creation.

A read-only closure borrows its environment for each call and is repeatable,
including when it owns non-Copy data. A closure whose body consumes any
non-Copy capture is itself consumed by the call and is single-use under
`AU3001`. Capturing closures are non-Copy. Their environment is Transfer only
when every captured value is Transfer.

An explicit exhaustive capture list requests live capabilities:

    read = lambda [settings] key: settings.lookup(key)
    mut update = lambda [mut stats] value: stats.record(value)
    snapshot = lambda [own cache] key: cache.get(key)

A bare entry creates a shared loan, `mut` creates an exclusive mutable loan,
and `own` retains ADR-0037 copy/move capture. Every used outer local must be
listed exactly once and every listed local must be used. A mutable-loan closure
is mutable-repeatable and must be called through a mutable closure place. A
loan closure is non-Copy, non-Transfer, synchronous, and local. See
[Closures](/manual/closures) and ADR-0038.

## FFI Views And Opaque Handles

FFI v0 views are temporary call-boundary capabilities, not first-class Aura
references. Bare `str` and `list[uint8]` retain their owner while exposing a
const pointer and byte length for one synchronous foreign call. `mut list[uint8]`
requires an exclusive mutable list place and copies the initial
bytes into a same-length scratch buffer; exactly that length is written back
after the foreign function returns. Empty views use a null pointer with length
zero. Foreign code must not retain any view pointer.

An opaque FFI handle is a non-Copy, non-cloneable, non-Transfer owned wrapper
for one non-null foreign pointer. A bare handle parameter retains it; `own
Handle` consumes it; `mut Handle` is unavailable. Aura does not automatically
call a foreign destructor, so a binding must invoke its explicit consuming
close/free declaration. Opaque handles cannot be task captures, task results,
or Queue payloads.

See [FFI v0](/manual/ffi) for the complete ABI and failure boundary.

## Tasks And Borrowing

The four `TaskGroup` start methods accept named functions or associated
methods with bare shared or `own` parameters. `mut` targets are rejected. The
two `_with_stack` forms add an `int64` capacity argument before the callable;
they do not change capture ownership.

```aura
def worker(label: str):
    print(label)

with group = TaskGroup():
    label = "compile"
    group.start_soon(worker, label.clone())
    print(label)
```

Each task argument is copied or moved into task-owned capture storage before
the child runs. The target then shares or consumes that capture according to
its declared mode. Copy task and queue handles still refer to shared runtime
state. See
[Concurrency](/manual/concurrency) and [Execution Model](/manual/execution-model#tasks-and-scheduler).

Accepted ADR-0033 adds a separate `Transfer` check to task captures,
results, Queue construction, and Queue `put`/`try_put`. Handle-only Queue
operations do not recheck the payload. A bare target parameter can still borrow its
child-owned capture for the call, but the captured value itself must be
transferable. A shared or mutable capability view cannot cross the boundary:
pass owned structural data instead. Copy values, `str`, and aggregates made
entirely from transferable components qualify; `random.Rng`, `TaskGroup`, and
live host resources do not.

`process.Completed`, `net.HttpResponse`, and `net.UdpDatagram` are owned
snapshot data rather than live resources, so they qualify. Their live
`process.Child`, `net.HttpExchange`, and `net.UdpSocket` sources do not.

A Copy value read through shared or mutable access is a narrow exception: task
capture materializes an independent owned snapshot, so no capability crosses.
A non-copy access cannot be captured this way because the child would need
ownership of the value.

The same decision statically divides task results into repeatable values and
single-consumer values. `Task[T]` is copyable only when `T` is copyable, a
`Queue[...]`, or a recursively repeatable `Task[...]`. Otherwise each result
method consumes the unique observation right even when it reports timeout,
cancellation, or failure. Multi-task waits consume their entire task list,
and `wait_any` abandons unchosen rights. These rules are required before the
pinned-worker runtime can safely run sibling task bodies on different host
threads. Queue and Task handle identity may cross workers, while every other
capture or result remains an owned
`Transfer` value rather than a shared capability.

## Resources And `with`

Resource ownership should normally be lexical:

```aura
import fs
import io

def show_file() -> Result[None, io.Error]:
    with file = try fs.open("data.txt"):
        text = try file.read_all()
        print(text)
    return Result.Ok(None)
```

`with` consumes the resource expression and creates a fresh mutable managed binding. A managed resource or its non-copy fields cannot be moved out in a way that would prevent cleanup. The registered `close` runs on normal fallthrough, `return`, escaping loop control, `try` propagation, and maintained runtime failure; nested cleanups run in reverse order.

Builtin resource behavior is defined by its module chapter. A user class must be non-generic and define `close(mut self) -> None` with no ordinary parameters. Full cleanup ordering and failure precedence are specified in [Execution Model](/manual/execution-model#resource-lifetime-and-cleanup).

## Grammar

The normative capability spellings are bare, `own`, and `mut` ordinary
parameters; `self`, `own self`, and `mut self` receivers; bare, `own`, and
`mut` collection loops where the iterable supports them; bare, `match own`,
and `match mut` matching; mutable bindings; local and returned `view` forms;
explicit lambda capture lists; owned return annotations; and `with`. Their
productions are in [Grammar](/manual/grammar). Call arguments themselves never
carry a capability prefix.

## Typing Rules

Every expression has one static copy/move category and every parameter has one
declaration-stable passing mode. Bare parameters grant logical shared access
for every type; an implementation may pass copy bits directly. Explicit `own`
consumes; `mut` requires one exclusive mutable place. Shared and owned
defaults are legal, with shared temporaries lasting through the call;
`mut` defaults are rejected. Place-prefix overlap, partial moves,
control-flow joins, loop repetition, owned-return moves, view provenance and
last-use regions, borrowed matches, borrowed iteration, task capture, and
managed-resource containment are checked before lowering. Clone-producing
generic operations infer obligations that are propagated through calls and
discharged after specialization.

## Runtime Semantics

A copy use duplicates a value and a move transfers it. Ordinary parameter
borrows remain call-scoped access contracts. An explicit view carries a
compiler/runtime loan descriptor for one source place and generation; reads
and writes resolve through that descriptor without cloning. Mutable borrowed
calls and list iteration write through the original place; `match mut` reconstructs
and writes back on every arm exit. Simple dict indexed assignment accepts and
owns any value type; direct compound indexed assignment requires a copy `list`
element or `dict` value.
Task start first transfers captures into child-owned storage. `with` owns one cleanup registration and runs it exactly
once on every maintained scope exit under the documented failure-precedence
rules.

## Ownership And Evaluation Order

Subexpressions evaluate in the order defined by [Execution
Model](/manual/execution-model#evaluation-order), then a copy, move, or borrow
is applied at its typed boundary. All receiver and argument accesses for one
call are checked together, so source order cannot legalize overlapping shared,
mutable, and owned uses. A partial move preserves proven-disjoint fields;
reinitializing the exact moved place restores it. Control-flow merging never
silently restores ownership, and Aura never inserts a clone or coercion to
repair an invalid use.

Capturing a copy place duplicates its value. A non-copy place selected as a
binary left operand, index base, method receiver, or indexed-assignment target
remains borrowed until that operation consumes all of its inputs. A later
shared borrow is permitted. An overlapping mutable borrow or consumption is
rejected with `AU3002`, with the retained selection identified as the borrow
origin. Name roots and projected member places follow the same rule, and no
backend inserts a hidden deep clone. Operations that require a point-in-time
representation produce it immediately; each f-string interpolation renders to
`str` before the next interpolation begins.

Compound assignment uses the corresponding binary operator dispatch, including
applicable user-defined operator traits for root and projected targets. A copy
target is captured before the right operand. A non-copy root or projected
target remains borrowed across that operand, so overlapping mutable borrow or
consumption is `AU3002`. A non-copy `list` element or `dict` value cannot be a
direct compound target because Aura 0.3 has no indexed-place identity and
writeback model; Aura rejects the operation instead of cloning or
destructively moving the stored value.

## Diagnostics

`AU1101` reports malformed ownership, receiver, loop, match, or return syntax.
`AU2002` covers type mismatch, while
`AU2004` reports argument binding that cannot satisfy a required mutable place.
`AU2999` covers unsupported move/control-flow/resource cases without a narrower
category. `AU3001`
reports use of a moved or partially moved place. `AU3002`
reports overlapping or invalid borrows, moving through a borrow, invalid
mutable-borrow defaults or task targets, stale borrowed-pattern bindings, and
later mutable or consuming
access that overlaps a retained non-copy binary operand, index base, method
receiver, or indexed-assignment target. In a retained-expression conflict, the
diagnostic points to both the later access and the retained-borrow origin.
`AU3003` reports assignment or mutation through an immutable place, including
shared `self`. `AU3004`
reports invalid parameter, receiver, loop, or Queue-iteration ownership modes.
`AU3005` rejects a direct indexed read of a non-copy list element or dict value;
`AU3006` rejects the corresponding indexed compound read-modify-write.
`AU3007` rejects direct or transitive duplication of non-cloneable state,
including `random.Rng`, opaque FFI handles, capturing closure environments,
and unsafe generic specializations. `AU3008`
reports a non-Transfer task or Queue boundary. `AU3009` rejects clone,
clone-producing collection read, or aggregate copy that would duplicate a
single-consumer task-result right. Reuse after direct observation is the
ordinary moved-value `AU3001`; shared-access consumption is `AU3002`.
`AU3010` reports an invalid view escape, returned-view origin, or provenance
path. View diagnostics identify the creation and final use that retain the
conflicting loan and recommend shortening the region, using a disjoint place,
or producing an owned clone.
Ownership failures are static. A runtime operation reached through an owned or
borrowed value keeps its own code: `AU4001` for a general trap, `AU4002` for
arithmetic overflow or underflow, `AU4003` for a bounds or lookup violation,
`AU4004` for a zero divisor, and `AU4005` for a resource or I/O failure.

## Backend Support

The compiler performs one ownership/borrow analysis before backend selection.
MIR execution and direct native generation receive the same resolved parameter
ABI, moves, copies, capture modes, borrowed-match/iteration operations, and
cleanup registrations. Analysis and LSP signatures expose those same modes.
The parity matrix pins observable move, mutation, capture, writeback, cleanup,
and primary-diagnostic behavior.

## Limits And Implementation-Defined Behavior

Place analysis tracks local roots, fixed tuple positions, and field-prefix
paths; it proves disjoint fixed projections but is not a general alias theorem.
Indexed/keyed views, view-bearing aggregates, multi-origin returned views,
returned loan closures, and lifetime-parameterized callable types are
unavailable. Mutable set iteration,
explicit Queue ownership modifiers,
mutable-borrow task targets, moving out of a managed resource, and arbitrary
reference values are unavailable. Loop move analysis intentionally uses only
the limited Boolean reasoning described above. Ownership mode and evaluation
order are language-defined, not backend- or host-defined.

## Status

Copy/move classification, declaration-stable parameter defaults, explicit
owned/shared/mutable passing, all receiver modes, call-boundary exclusivity,
partial moves and reinitialization, flow-sensitive checks, owned returns,
borrowed matching and list/set iteration, task capture, cloning,
and lexical resource ownership are implemented for the post-Phase 1.5
surface; the one-time list/set/Queue iteration-source rule is accepted under
ADR-0017. Place-based local and returned views, inferred regions, reborrowing,
explicit loan closure captures, and unified loan cleanup are implemented under
ADR-0038. Mutable set iteration, Queue ownership modifiers, and mutable task
capture are unavailable.
