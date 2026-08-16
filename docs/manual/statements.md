# Statements

Statements introduce and update bindings, control execution, or evaluate an expression for its effects. This chapter defines their legality and observable flow. Exact syntax is normative in [Grammar](/manual/grammar#suites-and-statements), compile-time legality in [Static Semantics](/manual/static-semantics), and runtime sequencing and cleanup in [Execution Model](/manual/execution-model).

## Statements, Items, And Suites

Aura 0.3 statements are:

- binding and assignment
- local `view` bindings
- expression statements
- `return`
- `assert`
- `if` / `elif` / `else`
- `while` and `for`
- statement-form `match`
- `with`
- `break`, `continue`, and `pass`

Class, enum, function, trait, and implementation declarations are items, not statements. Items are module-level; declaration members such as fields, enum variants, and methods appear only in their permitted item bodies. Nested functions, classes, enums, traits, and implementations are not supported.

A compound statement header ends with `:` and `NEWLINE`, followed by an indented suite. Suites contain one or more statements:

```aura
if ready:
    print("ready")
    record_success()
```

One-line suites such as `if ready: print("ready")` are not valid. Blank and comment-only lines do not make a suite nonempty; use `pass` when no operation is required.

Statements are terminated by logical newlines. A physical newline suppressed
inside an open delimiter is not a statement terminator. Aura has no semicolon
and does not permit multiple statements on one physical line.

## Bindings And Assignment

The first assignment to a simple name introduces a binding:

```aura
name = "aura"
count: int32 = 0
```

The binding's type is its annotation when present, otherwise the initializer type. The initializer must have exactly that type after contextual literal inference.

`mut` makes a newly introduced binding assignable and usable as a mutable place:

```aura
def main():
    mut count: int32 = 0
    count = 1
    count += 2
```

Reassignment requires an existing mutable binding and preserves its type. `mut` does not mean dynamically typed, and it does not make values globally mutable through aliases.

`from` is a contextual identifier and is legal as a binding and assignment target when the token sequence is not a from-import:

```aura
def main():
    mut from = "cache"
    from = "network"
```

### Assignment Targets

An assignment target begins with a name and may continue through fields or indices:

```aura
point.x = 4.0
values[0] = 9
counts["ready"] = 2
user.profile.name = "Ada"
```

Calls cannot occur in a place-assignment target. A tuple unpack target contains
only names and recursively parenthesized name targets:

    left, right = pair
    name, (x, y) = record

The right side is evaluated once. Its exact tuple shape and corresponding
element types must match the target. A top-level comma distinguishes unpacking
from an expression; tuple value expressions themselves require parentheses.
Tuple unpacking uses plain `=`, not a type annotation, leading `mut`, compound
assignment, member leaf, or index leaf.

A type annotation is allowed only on a simple-name target. `mut` also belongs only to a new simple-name binding. These forms are invalid:

```aura
# Invalid.
# point.x: float64 = 4.0
# mut point.x = 4.0
```

Field assignment requires a mutable base place and a declared field. List
index assignment uses the `int64` index domain. Simple dict index assignment requires
exactly the dictionary's key type and either replaces an equal key or inserts a new
entry; an absent key is not a simple-assignment error. It accepts any value
type. The key and value are owned storage positions, so each is consumed when
non-copy, matching `set(key: own K, value: own V)`.

### Compound Assignment

Aura supports the complete arithmetic compound-assignment family `+=`, `-=`, `*=`, `/=`, `%=`, and `//=`:

```aura
count += 1
total *= scale
pages //= page_size
```

A compound assignment requires an existing mutable, initialized target. It
selects that target place once and uses exactly the corresponding binary
operator dispatch. This includes an applicable user-defined operator trait for
a root or projected target. For a copy target, it captures the current copied
value before evaluating the right operand and stores a same-typed result into
the originally selected place. Right-operand side effects therefore cannot
change the captured left operand or retarget the store. A non-copy root or
projected target remains borrowed across right-operand evaluation; an
overlapping mutable borrow or consumption is rejected with `AU3002`.

Direct indexed compound assignment requires a copy `list` element or `dict`
value. A non-copy indexed element is rejected because reading it for
read-modify-write would require either a hidden clone or a destructive move
before an operation that may fail. Use an explicit safe read or ownership
transfer followed by a simple write; for a dict, use `get(key)` or `remove(key)`
and explicit simple assignment. Runtime overflow/division behavior is the same
as for the corresponding expression operator. Integer `/=` is rejected with
the integer `/` teaching diagnostic; use integer `//=` for a floor quotient.
Floating `/=` remains true division. `//=` uses the builtin numeric or
Duration rule when applicable and otherwise may dispatch through
`FloorDiv.floor_div`; as with every compound assignment, the result must have
the target's existing type.

### Assignment Evaluation

A simple-name or field assignment evaluates its right side before creating or
updating the target. Indexed assignment evaluates the collection place and then
the index or key before its right side. Its non-copy collection base remains
borrowed through those later inputs; an overlapping mutable borrow or
consumption is rejected with `AU3002`, and no hidden deep clone is inserted. A
simple dict assignment captures and, when non-copy, consumes that key before
evaluating and consuming its value, so later value-side effects cannot change
the selected key. Reassigning an exact
moved binding or field reinitializes that place when the new value has the
required type. Failed checked mutation produces the
documented runtime failure or typed result and does not create a different
language-level partial assignment contract.

See [Ownership And Borrowing](/manual/ownership-and-borrowing) for moves, partial field moves, and mutable-place rules.

## View Bindings

`view name = place` creates an immutable shared alias, and `view mut name =
place` creates a non-rebindable mutable write-through alias. The initializer
must resolve to a supported addressable root, field path, fixed tuple position,
or existing view; collection indexes and computed temporaries are rejected.

    mut pair = (1, 2)
    view mut second = pair[1]
    second = 7
    print(pair)

The binding's pointee type is inferred. Assigning through a mutable view
changes the source; it does not retarget the view. Static final-use analysis
ends its loan as early as control flow safely permits. Overlapping mutation,
move, rebind, cleanup, or another mutable loan is rejected while it remains
live. Scope and control-flow exits release active loans before outer cleanup.

## Expression Statements

Any expression may be used as a statement when its produced value is not needed:

```aura
print("ready")
queue.close()
counter.increment()
```

The expression is fully evaluated, including moves, mutations, I/O, and runtime failures; its resulting value is discarded. A discarded `Result` is not implicitly propagated. Use `try` or `match` when failure must affect control flow.

## `return`

`return` is legal only inside a function or method:

```aura
def answer() -> int32:
    return 42
```

The expression is evaluated before control returns. Its type must equal the
declared return type. Bare `return` produces `None` and is valid only where
`None` is a valid return. Inside a declared view-returning function, `return
view [mut] place` hands a matching loan derived from the named `from` origin to
the caller.

```aura
def maybe_log(enabled: bool):
    if not enabled:
        return
    print("enabled")
```

A non-`None` function must return on every statically reachable path. Returning runs active `with` cleanups in reverse nesting order before control reaches the caller.

## Conditional Statements

`if`, zero or more `elif` branches, and an optional `else` select at most one suite:

```aura
if value < 0:
    print("negative")
elif value == 0:
    print("zero")
else:
    print("positive")
```

Conditions must have exactly type `bool`. Aura does not convert strings, numbers, collections, resources, or classes by truthiness.

Conditions are evaluated in source order until one is `true`. Only the selected suite executes. Static checking analyzes branches independently and conservatively merges ownership, partial-move, and initialization state across paths that can continue.

## `while`

A `while` statement evaluates its condition before each iteration:

```aura
def main():
    mut attempts = 0
    while attempts < 3:
        attempts += 1
```

The condition must have type `bool`. A false first condition executes the body zero times. Aura 0.3 has no loop `else` clause.

Moving a non-copy outer value for the first time inside a repeatable loop is rejected when it could make a later iteration invalid. Reinitialize the place on every continuing path or restructure ownership explicitly.

## `for` Iteration

A `for` statement binds one name or recursively unpacks one tuple target for
each value from an iterable:

```aura
for value in values:
    print(value)

for name, count in records:
    print(name)
    print(count)
```

Use `for value in own values:` when the loop deliberately consumes a `list` or
`set` and needs owned element bindings. The collection moves once into a
loop-private source at entry. Reinitializing the consumed `values` binding in
the body does not switch or truncate that active iteration.

Every target leaf is local to the body, does not escape, and cannot shadow a
name already visible in the same scope. A tuple target must match the yielded
tuple shape exactly.

Maintained iterable forms include:

| Form | Behavior |
| --- | --- |
| `for i in range(n):` | Yields `int64` values from zero up to `n`, excluding `n`. |
| `for i in range(start, end):` | Yields `int64` values from `start` up to `end`, excluding `end`. |
| `for value in values:` | Retains the list and yields shared access for non-copy elements. |
| `for value in own values:` | Consumes the list and yields owned elements. |
| `for value in mut values:` | Retains a mutable list and yields mutable access; the iterable place must be mutable. |
| `for value in set:` | Retains the set and yields shared-borrowed access. |
| `for value in own set:` | Consumes the set and yields owned elements. |
| `for value in queue:` | Receives queue items under the scheduler-aware queue iteration contract. |
| `for index, value in enumerate(seq):` | Yields `(int64, element)` pairs, counting positions from zero. |
| `for left, right in zip(first, second):` | Yields one pair per shared position and stops at the shorter sequence. |

When an iterable yields tuples, bare/shared collection iteration gives
non-copy tuple leaves shared provenance; `own` collection iteration gives
owned leaves; and bare Queue iteration receives an owned item and gives owned
leaves. `mut` iteration with a tuple target is rejected because the
minimal tuple surface has no recursive element writeback.

`for value in mut set:` is not supported in Aura 0.3. Queue iteration
receives values rather than traversing places: each item arrives owned and the
queue handle is a copy value. Consequently `own` and `mut` are rejected for
Queue iteration; use the bare form. That form evaluates
and copies the Queue handle once at loop entry without freezing the source
binding. Rebinding the source in the body does not switch later receives.
Queue iteration ends according to close, cancellation, producer-completion,
and task-failure rules defined in [Concurrency](/manual/concurrency).

`enumerate` and `zip` are compiler-known loop forms rather than callable
values. They are legal only as the iterable of a `for` statement; naming either
one anywhere else reports `AU2005` and names the loop spelling. A user
declaration of either name shadows the loop form, so an existing `def zip(...)`
keeps its ordinary call meaning.

Both forms read their operands by position, so each operand must be a `list[T]`
or a `set[T]`; a `Range` or `Queue[T]` operand reports `AU2002`. Both iterate
over the bare-loop borrow default: an ownership modifier on the loop reports
`AU3002`, every operand stays shared-borrowed and frozen for the whole loop,
and a non-copy element binding is a shared borrow that cannot be moved out.
`enumerate` takes exactly one operand and `zip` exactly two, positionally; any
other arity or a named argument reports `AU2004`.

`zip` stops as soon as any operand has no value at the current position, so it
performs `min(len(first), len(second))` iterations and never observes the
longer sequence's tail.

```aura
hosts = ["alpha", "beta"]
ports = [80, 443, 8080]

for index, host in enumerate(hosts):
    print(index)

for host, port in zip(hosts, ports):
    print(port)
```

Range iteration accepts only the bare form. Every yielded `int64` is an
independent copy, so `mut` has no place through which to write back and `own`
has nothing to transfer. Either modifier reports `AU3004`, explains that
ownership modifiers do not apply to these copy values, and suggests
`for item in range(...):`.

## `break` And `continue`

`break` and `continue` are legal only inside `for` or `while`:

```aura
for value in range(10):
    if value == 5:
        break
    if value % 2 == 0:
        continue
    print(value)
```

`break` exits the nearest loop. `continue` begins its next iteration. If either operation exits an active `with` scope, that scope is cleaned up before loop control transfers.

## Match Statements

Statement-form `match` evaluates its scrutinee exactly once and considers arms in source order. The first matching arm executes:

```aura
match result:
    case Result.Ok(value):
        print(value)
    case Result.Err(message):
        print(message)
```

Every statement arm contains an indented suite. Inline statement arms such as `case Result.Ok(value): print(value)` are not valid. Inline arms are available only for match expressions whose arm body is one expression; see [Expressions](/manual/expressions#match-expressions).

Matches over enums and booleans must be exhaustive unless `_` covers the remainder. Integer, float, and string literal matches require `_` because their value spaces are open. Duplicate, unreachable, type-incompatible, or wrong-arity patterns are rejected.

`match own value` consumes a non-copy scrutinee. This includes a non-copy tuple,
which is consumed as one whole value and unpacked into owned pattern bindings.
Bare `match value` retains ownership and exposes shared enum-payload or tuple
leaf access. `match mut value` permits enum-payload mutation and
writeback, but a tuple pattern is rejected because recursive mutable tuple
writeback is not part of the minimal surface. See
[Enums And Pattern Matching](/manual/enums-and-match) for pattern forms.

## `with` And Scoped Cleanup

Aura accepts two equivalent binding forms:

```aura
with file = try fs.open("data.txt"):
    text = try file.read_all()
    print(text)
```

```aura
with TaskGroup() as group:
    group.start_soon(worker)
```

The first form is `with name = expression:`. The second is `with expression as name:`. Each form evaluates and consumes the resource expression, creates a fresh mutable managed binding, and registers cleanup after resource creation succeeds.

Supported builtin resources define their cleanup behavior. A user class can be used when it is non-generic and declares exactly `close(mut self) -> None`. The managed value cannot be moved out in a way that prevents cleanup.

The registered `close` operation runs exactly once when control leaves the body by:

- normal fallthrough
- `return`
- `break` or `continue` that exits the scope
- `try` error propagation
- a maintained Aura runtime failure

Nested cleanups run in reverse registration order. If the body is already failing and cleanup also fails, the body diagnostic remains primary.

This contract is shared by `aura run` through the maintained MIR runtime and by native builds through the maintained native execution paths. Backend parity tests enforce the common contract. See [Execution Model](/manual/execution-model#resource-lifetime-and-cleanup).

## `assert`

An assertion checks an invariant and either continues or produces an
unrecoverable runtime diagnostic:

    assert ready
    assert response_code == 200, "expected a successful response"

The condition must have exactly type `bool`. The optional message must have
exactly type `str`. The condition evaluates exactly once. A true condition
falls through without evaluating the message. A false condition evaluates the
message exactly once and traps with `AU4001`. Without a message, the exact
failure text is `assertion failed`; otherwise the supplied str is preserved
exactly, including an empty or whitespace-only value.

The diagnostic points to the `assert` keyword. A trap produced while evaluating
the condition or message occurs first and remains primary. Assertion failure
runs active `with` cleanups, and the assertion remains primary if cleanup also
fails.

An assertion has ordinary fallthrough for static analysis. It does not refine
the type or possible values of a later expression, and the compiler does not
strip it in any build mode. Assertions are valid executable top-level
statements in a script entry module; the ordinary rule against combining
top-level execution with a local `main` still applies.

See [Assertions](/manual/assertions) for the complete contract and executable
example.

## `pass`

`pass` performs no operation and produces no binding:

```aura
def placeholder():
    pass
```

It must appear on its own logical line. It is used for intentionally empty function, method, class, trait, implementation, or control-flow suites. An enum body still requires at least one variant and does not use `pass` as a variant.

## Module Constants, Imports, And Execution

Imports are module elements rather than executable statements. Aura accepts:

```aura
import util.math
from util.math import double, triple
import agents.telemetry as telemetry
from agents.telemetry import record as record_event, Event
```

Import paths are dot-separated identifiers. A module import may bind the
complete module under a local alias. Each name in a from-import may also have
its own local name, and renamed and direct imports may appear together. A
renamed import introduces only its local name into the importing module.

Aliases are static local names for resolved modules and declarations. They do
not change visibility, nominal identity, trait implementations, initialization
storage, or the package path used for resolution. Wildcard imports,
relative-dot imports, parenthesized import lists, and trailing import commas
are not accepted. Import resolution and visibility are defined in
[Packages](/manual/packages#imports).

An immutable binding at module level is a module constant:

```aura
message = "hello"
public retry_limit: int64 = 3

def main():
    print(message)
```

The constant initializer is required. `mut` module storage and later
assignment are rejected. Constants may coexist with a local `main`, and
reachable dependency constants initialize before entry execution. The full
scope, order, visibility, and ownership rules are defined in
[Names And Scopes](/manual/names-and-scopes#module-constants).

An entry module may also contain executable top-level statements:

    print(message)

Those statements execute in their stored source order after reachable module
constants are ready. An entry module with executable top-level statements
cannot define a local `main`. Imported module top-level statements do not
execute as import side effects.

A top-level `mut name = value` statement declares `name` in the entry script's
local environment. Later `name = value` and compound assignments such as
`name += value` reassign that same local:

```aura
mut count = 0
count = count + 1
count += 1
print(count)
```

A bare top-level binding with a new name remains a module constant, regardless
of its textual position among entry statements. It cannot read a top-level
script local because constants initialize before entry execution. Declare the
new binding with `mut` to keep the computation in the entry script, or move the
work into `main`.

The accepted `main` signatures and process exit behavior are defined in [Functions](/manual/functions#main) and [Execution Model](/manual/execution-model#entry-module-execution).

## Contextual Legality Summary

Parsing a statement shape does not make it legal in every context:

- `return` requires a function or method.
- `break` and `continue` require an enclosing loop.
- reassignment and compound assignment require a mutable existing place.
- member and index assignment require a mutable base and cannot declare a type or use `mut`.
- conditions require `bool` rather than truthiness.
- assertion conditions require `bool`, and assertion messages require
  `str`.
- match arms must satisfy compatibility, reachability, and exhaustiveness rules.
- `with` requires a supported resource and preserves its cleanup capability.
- items cannot appear inside suites.
- module constants are immutable and cannot use `mut` or reassignment.
- module constants cannot read top-level script locals, which initialize later.
- an entry module cannot mix executable top-level statements with local `main`.

The complete checker rules are normative in [Static Semantics](/manual/static-semantics), and ownership effects are normative in [Ownership And Borrowing](/manual/ownership-and-borrowing).

## Grammar

The simple and compound statement productions, suite indentation, binding and
assignment targets, loop modifiers, match arms, and `with` forms are normative
in [Grammar](/manual/grammar). Statements end at a physical `NEWLINE`; Aura
has no semicolon-separated or inline compound statements.

## Typing Rules

Bindings infer or check one type, and reassignment preserves it. Conditions are
exactly `bool`; return values match the enclosing signature; iterables determine
their loop binding contract; match patterns are compatible, reachable, and
exhaustive where required; and `with` accepts only the maintained cleanup
contract. Assertion conditions are exactly `bool` and messages are exactly
`str`; an assertion does not refine later control flow. Contextual legality
is checked after parsing.

## Runtime Semantics

Statements execute in source order within the selected suite. Simple-name and
field assignment evaluate the right side before writing the target; indexed
assignment evaluates its collection and index/key before the right side, with a
simple dict assignment capturing its owned key before any value-side effects;
compound assignment uses the corresponding binary dispatch and stores into its
once-selected target; a copy target is captured before the right side, while a
non-copy root or projected target remains borrowed across it; direct indexed
compound assignment reads only a copy element and traps with `AU4003` when a
Dictionary key is absent; conditionals select at most one branch; loops test or
receive before each body;
a match evaluates its
scrutinee once; and `with` registers cleanup only after resource construction
succeeds. An assertion evaluates its condition once, skips its message on
success, and evaluates that message once before failing. Control transfer runs
every exited cleanup in reverse registration order.

## Ownership And Evaluation Order

Bindings own, copy, or borrow their initializer according to type and context.
`own` list/set iteration consumes once into a loop-private source, bare
collection iteration retains and freezes its selected place, and Queue
iteration captures a copy handle once while receiving already-owned items.
The one-time iterable selection is the accepted ADR-0017 rule; the ownership
modes themselves remain those accepted in ADR-0006.
Simple dict indexed assignment consumes non-copy keys and values into owned
storage; direct list/dict indexed compound assignment is restricted to copy
elements. Assignment to a place
invalidates conflicting borrows and reinitializes the written place. Branch and
loop analysis conservatively preserves any move that may reach a continuing
path; no control-flow join restores ownership implicitly.

## Diagnostics

`AU1101` means malformed statement or suite syntax. `AU2001` means an
unresolved name or target. `AU2002` means an expected-type, condition,
iteration, match, return, or assignment mismatch. `AU2003` means an unsupported
compound-assignment operator. `AU2004` means call or target argument binding
failed. `AU2005` means unsupported syntax or feature for a Python-shaped
statement. `AU2999` means an exhaustiveness, contextual-legality, unsupported
statement rejection without a narrower code. `AU3001` means use of a moved
place; `AU3002` means a borrow conflict, including later access that mutably
borrows or consumes an overlapping retained non-copy compound or indexed-
assignment target; `AU3003` means an immutable target was used mutably; and
`AU3004` means an
invalid loop, parameter, or ownership mode. `AU3005` identifies a non-copy
direct indexed read, and `AU3006` identifies a non-copy indexed compound
assignment.
During execution, `AU4001` means a general statement trap, `AU4002` means
numeric range, overflow, or underflow failure, `AU4003` means a bounds or lookup
violation, `AU4004` means a zero divisor, and `AU4005` means a trapping resource
or I/O failure, including cleanup failure when no earlier body failure remains
primary. A failed assertion is `AU4001`, uses `assertion failed` or the exact
custom message, and points to its keyword.

## Backend Support

Every implemented statement form shares the checker and MIR lowering used by
MIR execution and direct native generation. Cleanup, loop, match, task, and
runtime-trap behavior is forced through the backend-parity suite; unsupported
direct lowering is contained rather than silently given different semantics.

## Limits And Implementation-Defined Behavior

Suites require a real statement, loop `else` is unavailable, statement match
arms cannot be inline, a statement may span
physical lines only through an open `(`, `[`, or `{`, backslash continuation is
unavailable, and items cannot nest in suites. Range iteration yields copy
`int32` values and accepts only the bare form as recorded above. No statement
evaluation order is implementation-defined.

## Status

Bindings, assignments, expression, return, and assertion statements,
conditionals, loops, match, scoped cleanup, `pass`, imports, and entry-module
top-level execution are implemented as described. Tuple assignment/loop
targets are implemented under Accepted ADR-0026. Class/collection
destructuring, loop `else`, exception statements, `yield`, `raise`, `async`,
and nested declarations are unavailable; `try` remains an expression over
`Result`.
