# Types

Aura is statically typed. Every expression has a type, and type annotations are part of the public shape of functions, fields, methods, and many empty literals.

The type system is designed to keep three facts visible:

- what kind of value a program has
- whether the value is copied or moved
- whether failure is represented in the return type

## Scalar Types

| Type | Description |
| --- | --- |
| `bool` | Boolean value: `true` or `false`. |
| `int` | Alias for `int64`; it is not a distinct type. |
| `int8`, `int16`, `int32`, `int64`, `int128`, `intsize` | Signed integers. |
| `uint8`, `uint16`, `uint32`, `uint64`, `uint128`, `uintsize` | Unsigned integers. |
| `float32`, `float64` | Floating-point values. |
| `str` | Owned UTF-8 string; `len()` counts Unicode scalar values and `byte_len()` counts encoded bytes. |
| `None` | Unit type and unit value. |
| `Duration` | Signed 128-bit nanosecond duration used by arithmetic, sleeps, timeouts, and scheduling APIs. |
| `Range` | Integer range returned by `range(...)`. |

Integer bounds are exact:

| Type | Inclusive range |
| --- | --- |
| `int8` | -128 through 127 |
| `int16` | -32,768 through 32,767 |
| `int32` | -2,147,483,648 through 2,147,483,647 |
| `int64` | -9,223,372,036,854,775,808 through 9,223,372,036,854,775,807 |
| `int128` | -2^127 through 2^127 - 1 |
| `uint8` | 0 through 255 |
| `uint16` | 0 through 65,535 |
| `uint32` | 0 through 4,294,967,295 |
| `uint64` | 0 through 18,446,744,073,709,551,615 |
| `uint128` | 0 through 2^128 - 1 |
| `intsize` | host-pointer-width signed range |
| `uintsize` | host-pointer-width unsigned range |

`float32` and `float64` use IEEE-754 binary32 and binary64 representations. Literal lexing first requires a finite binary64 value; contextual `float32` conversion may round or overflow as recorded in [Current Limits](/manual/current-limits). Runtime operations may produce NaN, but Aura 0.3 makes `/`, `//`, or `%` by a floating zero explicit runtime failures rather than producing infinity or NaN through those operators.

`int` is an alias for `int64`, so the two spellings have identical bounds,
type identity, layout, and runtime behavior. Integer literals may be decimal,
hexadecimal (`0x`), binary (`0b`), or octal (`0o`), with underscores between
digits. Every spelling follows the same contextual typing and bounds rules. An
unsuffixed integer literal uses an expected integer type when one is available.
It may also use an expected `float32` or `float64` when its value is exactly
representable in that target; this is literal typing, not a conversion
available to integer variables. Otherwise it defaults to `int64`.

The default does not widen explicitly typed APIs. Existing fixed `int32`
contracts remain `int32`, including `main()` exit statuses, queue capacities,
and bounded process/network I/O byte-count parameters. Position APIs form one
deliberate exception: range bounds and yields, collection indices, slice
endpoints, enumeration positions, and Array coordinates use `int64`. Values of
type `int8`, `int16`, `int32`, `uint8`, `uint16`, or `uint32` widen losslessly
at those positions. This conversion is unavailable in ordinary assignments,
arguments, operators, and returns. Length results are also `int64`:
the builtin `len`, `str.len`, `str.byte_len`, `list.len`, `dict.len`, and
`set.len` all return `int64`, so they compose directly with ranges and indices.
`random.secure_bytes(n)` is a separate byte-count API: `n` is `int64`, with a
fixed per-request resource and safety ceiling of `2147483647`.

`Duration` stores a signed 128-bit count of nanoseconds. Literal units are
normalized exactly to nanoseconds; literals are non-negative, while associated
constructors and arithmetic can produce negative values. Representability as a
language value is separate from validity as a host wait or deadline. `Range`
contains `int64` start/end values and iterates from the start inclusive to the
end exclusive.

The associated constructors `Duration.ms(int64)`, `Duration.seconds(int64)`,
and `Duration.minutes(int64)` accept signed counts. Duration values support
checked addition and subtraction with another Duration, multiplication by
`int64` in either operand order, floor division by `int64`, and full
value-based comparison. `to_ms()` and `to_seconds()` convert the exact rational
unit value to the nearest representable IEEE-754 binary64 value, ties-to-even;
they may round. Their rounding, Duration rendering, and invalid host-timer
policy are accepted under ADR-0019; the signed nanosecond representation and
operators are accepted under ADR-0007.

Numeric literals are checked against the target type. Integer literals must fit an annotated integer target, and a float-context integer literal must be exactly representable in its `float32` or `float64` target. An inexact literal must make rounding explicit with a floating spelling or `.to_float()`. Integer-to-float casts also reject silent precision loss. Separately, every integer type provides `.to_float() -> float64`, which intentionally permits IEEE-754 round-to-nearest, ties-to-even conversion when an application wants to enter the floating domain.

A bare `value: str` parameter grants shared access. Bare parameters do the
same for copy and move types; an implementation may pass copy bits directly
without changing that source contract. `str` owns its UTF-8 storage. Aura has
no separate slice layout or lifetime-bearing text-view type.

`str.len() -> int64` scans the text and counts Unicode scalar values in
O(n). `str.byte_len() -> int64` reads the UTF-8 byte count in O(1).
`str.to_bytes() -> list[uint8]` and
`str.from_bytes(list[uint8]) -> Result[str, bytes.Error]` provide the
explicit strict UTF-8 boundary; `list[uint8]` is Aura's bytes representation.
Aura has no distinct character type, integer str indexing, `chars()`,
`ord()`, or `chr()`. String slicing accepts `int64` scalar endpoints,
runs in O(n) over the source, and returns a fresh owned str. It is not a
view or a byte-indexing operation.

## Copy And Move Categories

Copy values may be reused after assignment or calls through value/`own`
positions:

- numbers
- `bool`
- `Duration`
- `Queue[T]`
- under Accepted ADR-0033, `Task[T]` only when `T` is repeatable as defined
  in [Provisional Transfer Classification](#provisional-transfer-classification)
- tuple values when every element type is copyable
- `copy class` values whose fields are all copyable
- user enum values when every declared payload type is statically copyable
- `Option[T]`, `Result[T, E]`, `SendError[T]`, and `QueueReceive[T]` when all payload types are copyable

Move values transfer ownership:

- tuple values with at least one move element
- `str`
- `list[T]`
- `dict[K, V]`
- `set[T]`
- `random.Rng`
- ordinary user classes
- user enum values with any move payload
- `json.Value` and `json.Error`
- `Option`, `Result`, and related outcome values with move payloads
- `TaskGroup`
- file, process, supervisor, and network resources
- opaque FFI handles declared by `extern "C" opaque class`

Move values can still be shared through a bare parameter, accessed mutably
through a `mut` parameter, or duplicated explicitly through methods such as
`.clone()` when the type supports cloning.

Slicing `str` produces a fresh owned str. Slicing `list[T]` produces a
fresh owned list and is clone-producing for `T`: Copy elements are copied,
non-Copy elements must be clone-safe, `random.Rng` state is rejected with
`AU3007`, and non-repeatable Task observation rights are rejected with
`AU3009`. The result is another move value independent of the source.

`Queue[T]` is a copy handle to shared runtime state. Under Accepted
ADR-0033, a `Task[T]` handle is conditionally copyable so aliases cannot
duplicate a single-consumer result right. Copying an allowed handle never
copies queued values or task results; it gives another reference to the same
queue or task.

`TaskResult[T]`, `SelectOutcome[Q, T]`, `WaitAny[T]`, and `WaitAll[T]` are
treated as move outcome values even when every payload type is copyable.
`Range` is also not a general copy type in Aura 0.3; use ranges directly in
iteration rather than relying on duplication.

A generic user-enum payload whose declared type is an unconstrained type parameter is not assumed copyable, even when one later instantiation supplies a copy type.

## Tuple Types

`(T1, T2)` is a fixed two-element structural tuple type and `(T,)` is a
fixed singleton tuple type. Tuple arity and corresponding element types are
part of type identity. Tuple types may appear anywhere another complete type
reference is accepted, including parameter, field, payload, local annotation,
and return positions.

A tuple is copyable if and only if every element is copyable. Copy
classification is recursive through nested tuples. Otherwise the complete
tuple is a move value; unpacking it consumes the source as one whole value
rather than exposing independently reusable positional partial moves.

Two tuple values may be compared with `==` or `!=` only when they have the same
static tuple type. The comparison is recursive over corresponding element
values and reads rather than consumes both operands, regardless of copy
classification. Runtime metadata carried with a tuple value is not part of
value equality. Tuple ordering is not defined.

Aura has no empty tuple type and does not convert tuples to or from
collections. See [Tuples](/manual/tuples) for construction, unpacking,
patterns, indexing, and the exact current boundary.

Copy/move classification and clone safety are distinct. `random.Rng` is not
merely a move type: it exposes no public duplication route. A clone-producing
operation is valid only when its produced type cannot contain an `Rng` through
an ordinary value-storing class, enum, or collection path. `Task[T]` and
`Queue[T]` stop that traversal because copying either handle does not observe
or copy its stored `T`; moving, removing, or receiving a value also transfers
one owner instead of cloning it.

## Provisional Transfer Classification

Accepted ADR-0033 defines the static property used at a task boundary.
`Transfer` means that ownership of a value may cross from one Aura task
worker to another; it is separate from both Copy and clone safety. `Transfer`
is derived by the compiler and is not a builtin trait that source code can
implement or assert. An ordinary user trait also named `Transfer` does not
affect this structural classification.

All copy types and `str` are `Transfer`. `list[T]`, `set[T]`, `dict[K, V]`,
tuples, classes, and enums are `Transfer` exactly when all of their stored
component types are. The same recursive rule covers data wrappers such as
`Option`, `Result`, task/queue outcomes, errors, and `json.Value`.
`Queue[T]` and `Task[T]` handles are `Transfer` independently of `T`: moving
the handle does not inspect or move the stored payload. Queue construction,
`put`, and `try_put` separately require its payload `T` to be `Transfer`;
handle copies, receives, fallback receives, and `close` do not recheck `T`.

Shared and mutable capability views are not `Transfer`. Neither are
`random.Rng`, `TaskGroup`, or live filesystem, process, pipe, supervisor,
listener, socket, stream, HTTP-exchange, WebSocket, or TLS resources. A later
decision may whitelist an individual host type only after its thread-safety is
proved. Owned data returned from a host operation, such as completed output or
a structural error value, is classified from the data it stores rather than
from where it originated. `process.Completed`, `net.HttpResponse`, and
`net.UdpDatagram` are explicitly Transfer owned snapshots; their live
`process.Child`, `net.HttpExchange`, and `net.UdpSocket` sources are not.

Reading a Copy value through shared or mutable access materializes an
independent owned snapshot rather than transporting the capability. That
snapshot may cross when its type is `Transfer`. Non-copy access cannot use this
exception because value capture would require ownership.

An unconstrained generic parameter does not prove `Transfer`. Phase 5.6 does
not infer a deferred Transfer contract: a task or Queue boundary with an
unresolved parameter is rejected with `AU3008`. A generic task target is usable
when call inference has already produced complete concrete capture and result
types. A task target may spell explicit specialization narrowly as
`function[Types]` or `Type.associated_method[Types]`; brackets retain ordinary
indexing meaning outside a TaskGroup start target. A bare target is valid when
its declared/default context already makes every relevant type concrete.

`Task[T]` is always `Transfer`, but ADR-0033 makes its Copy classification
conditional. It is copyable only when `T` is copyable, when `T` is
`Queue[...]`, or when `T` is `Task[U]` and `U` is recursively repeatable.
This prevents a nested handle such as `Task[Task[str]]` from being copied
to duplicate a single-consumer result right.

This classification is the Phase 5.6 boundary used by the pinned-worker
runtime. Queue and Task handle state is synchronized for cross-worker use;
all other task captures and results remain owned, structural `Transfer` values.
The boundary therefore stays share-nothing even when sibling task bodies run
on different pinned workers.

## Builtin Generic Types

| Type | Meaning |
| --- | --- |
| `Option[T]` | `Some(T)` or `None`; use for ordinary absence. |
| `Result[T, E]` | `Ok(T)` or `Err(E)`; use for recoverable failure. |
| `list[T]` | Owned ordered collection. |
| `dict[K, V]` | Owned key/value dictionary. |
| `set[T]` | Owned set of unique values. |
| `Array[T]` | Owned contiguous row-major numeric array; `T` is exactly `int32`, `int64`, `float32`, or `float64`. |
| `Queue[T]` | Scheduler-aware typed queue handle. |
| `Task[T]` | Transferable task-result handle; conditionally Copy under Accepted ADR-0033. |
| `SendError[T]` | Queue send failure that carries the unsent value. |
| `QueueReceive[T]` | Queue receive outcome. |
| `TaskResult[T]` | Task result outcome. |
| `SelectOutcome[Q, T]` | Typed `select(...)` outcome for Queue payload `Q` and Task result `T`; an absent source category uses `None`. |
| `WaitAny[T]` | `wait_any(...)` outcome. |
| `WaitAll[T]` | `wait_all(...)` outcome. |

`Array[T]` has runtime rank and `list[int64]` shape metadata rather than
shape-level static type arguments. Every Array has rank at least one, may
contain zero-length dimensions, and owns its contiguous CPU buffer. It is
non-Copy, explicitly cloneable, and structurally `Transfer`; a Task result
containing an Array retains the ordinary single-consumer observation right. See
[Numeric Arrays](/manual/numeric-arrays).

## Resource And Module Types

These types are provided by builtin modules and are reserved names.

| Module | Types |
| --- | --- |
| `io` | `io.Error` |
| `fs` | `fs.File` |
| `json` | `json.Value`, `json.Error` |
| `random` | `random.Rng` |
| `net` | `net.TcpListener`, `net.TcpStream`, `net.UdpSocket`, `net.UdpDatagram`, `net.HttpListener`, `net.HttpExchange`, `net.HttpResponse`, `net.WebSocketListener`, `net.WebSocket`, `net.UnixListener`, `net.UnixStream`, `net.TlsListener`, `net.TlsStream` |
| `process` | `process.Child`, `process.Pipe`, `process.Completed`, `process.Supervisor`, `process.ExitStatus`, `process.Wait`, `process.Stdio`, `process.Error`, `process.RestartPolicy`, `process.SupervisorEvent`, `process.SupervisorWait` |

Resource types should usually be scoped with `with` or closed explicitly.
`random.Rng` is an opaque move type rather than a resource: it has mutable
state but no `close()` operation or `with` contract. Its complete type and
sequence rules are in [Randomness Module](/manual/randomness).

`json.Value` is a move type whose recursive variants represent Null, Boolean,
`int64`, finite `float64`, str, list, and dict object data. `json.Error` is a
move type because its Syntax variant owns a str. Their exact variants and
number rules are in [JSON Module](/manual/json).

## Type Annotations

Simple annotations:

```aura
count: int32 = 0
name: str = "aura"
```

Collection annotations:

```aura
names: list[str] = []
lookup: dict[str, int32] = {}
seen = set[int32]()
```

Empty collection literals need an expected type. Constructors are also available:

```aura
names = list[str]()
lookup = dict[str, int32]()
seen = set[int32]()
```

`T?` is shorthand for `Option[T]`:

```aura
name: str? = None
```

Type arguments are invariant, nonempty when brackets are present, and must exactly match the declared arity. Aura does not implicitly convert `list[int32]` to `list[int64]` or treat structurally identical user classes as the same type.

## Option And Result Types

Construct `Option` and `Result` with their enum names:

```aura
maybe: Option[str] = Option.Some("name")
missing: Option[str] = Option.None

result: Result[int32, str] = Result.Ok(42)
failure: Result[int32, str] = Result.Err("bad number")
```

Bare `None` contextually denotes `Option.None` whenever an expected
`Option[T]` is available. This context flows through grouping, annotated
bindings, returns, and arguments. Equality and inequality provide the context
symmetrically: if either operand is `Option[T]`, a bare `None` on the other side
has that same option type. Unit `None == None` is `true` and unit
`None != None` is `false`. A qualified `Option.None` without an expected or
otherwise inferred specialization is rejected because `T` is unconstrained.
Aura has no identity-test spelling: use `value == None`, `value != None`, or
`match`, not Python's `is` or `is not`.

Pattern matching may use qualified or short-form variants when the type is known:

```aura
match result:
    case Result.Ok(value):
        print(value)
    case Result.Err(message):
        print(message)
```

## User Types

Classes create product types:

```aura
class Point:
    x: float64
    y: float64
```

Enums create sum types:

```aura
enum Load[T]:
    Ready(value: T)
    Empty
    Failed(message: str)
```

Traits define shared behavior:

```aura
trait Named:
    def name(self) -> str
```

## Recursive Fields

Direct recursive fields are not implemented. Use `indirect` for recursive class fields:

```aura
class Node:
    value: int32
    next: indirect Option[Node] = Option.None
```

`indirect` gives the recursive field a level of indirection so the value has a finite size.

## Casts

Numeric casts use `value as NumericType`. Non-numeric casts are not implemented.

- integer-to-integer casts require the value to fit the target bounds
- integer-to-float casts require exact representability and reject silent precision loss; use integer `.to_float()` when a possibly rounded `float64` result is intended
- float-to-integer casts require a finite in-range value and truncate toward zero
- `float64` to `float32` rounds through the host `float32` representation
- `float32` to `float64` preserves the represented value

Casts are checked at runtime when the source value is not a compile-time literal. A failed cast is a runtime diagnostic, not `Result.Err`.

Use parsing functions for text-to-number conversion:

```aura
def parse_answer() -> Result[int32, str]:
    value = try parse_int32("42")
    return Result.Ok(value)
```

## Grammar

Type syntax consists of an identifier or module-qualified type path, optional
bracketed type arguments, the optional marker `?`, and `indirect` in class-field
position, as collected in [Grammar](/manual/grammar). `mut` and `own` parameter
modifiers are not type constructors. Bare, `mut`, and `own` parameter
capabilities govern access at a call boundary, while every `-> T` annotation
describes an owned result.

## Typing Rules

Every expression has one static type. An annotation, parameter, return, field,
collection element, or expected enum context may type a compatible literal;
otherwise integers default to `int64` and floating literals to `float64`.
Unqualified `int` is exactly `int64`. Non-literal values never widen
implicitly. Generic arity, substitutions, bounds, field recursion, optional
desugaring, cast legality, and exact assignment equality are checked before
execution.

## Runtime Semantics

Copy scalars and declared copy aggregates are represented by value. Other
values use the maintained owned runtime representations documented by their
feature pages. Arithmetic and casts are checked and may trap with a runtime
diagnostic; typed library failure remains an `Option` or `Result` value.
`indirect` inserts the maintained runtime indirection needed to construct a
recursive field.

## Ownership And Evaluation Order

The static type determines whether reading an owned place copies it or moves
it. A copy declaration is valid only when every stored field or payload is
copy. Borrowing and parameter passing do not change the underlying type, and
Aura inserts neither hidden cloning nor runtime coercion. Type annotations
are erased after checking and add no evaluation step. Generic clone-producing
uses infer clone-safety obligations that are checked after specialization; this
does not change the underlying copy/move category.

## Diagnostics

`AU1101` reports malformed type, type-argument, or annotation syntax. `AU2001`
reports an unknown or unavailable type name. `AU2002` reports type mismatches,
unresolved contextual literal typing, generic arity, payload, field, and
annotation mismatches. `AU2003` reports unsupported numeric operators or
casts, and `AU2004` reports invalid constructor argument binding. `AU2999`
covers invalid recursive layouts and other type rejections without a narrower
category. `AU3001` reports use of a moved non-copy value; `AU3002` reports a
borrow conflict; `AU3003` reports mutation through an immutable place; and
`AU3004` reports an invalid ownership or receiver type mode. `AU3005` reports a
non-copy indexed read, and `AU3006` reports a non-copy indexed compound
assignment. `AU3007` reports an operation or specialization that would
duplicate non-cloneable state such as `random.Rng`, an opaque FFI handle, or
a capturing closure environment. `AU3008` reports a non-Transfer
task or Queue boundary. `AU3009` rejects cloning, collection reads, or
aggregate copies that would duplicate a single-consumer task-result right;
using an already-consumed task binding is `AU3001`. Runtime `AU4001` means a general checked trap, `AU4002` means numeric overflow, underflow, range,
or exactness failure, `AU4003` means a bounds or lookup violation, `AU4004` means a zero
divisor, and `AU4005` means a trapping resource or I/O failure.

## Backend Support

The checker produces one canonical type model for MIR lowering, compiler-backed
analysis, and direct native code generation. All types documented as
implemented are supported by both maintained execution paths; the parity gate
contains a backend surface that cannot preserve the same behavior.

## Limits And Implementation-Defined Behavior

`int` is an alias for `int64`; method-value types, user-defined numeric casts,
and non-numeric casts are unavailable, and recursive value fields require
`indirect`. Capture-free named function values use
`def(T1, mut T2, own T3) -> R`; bare parameters are shared and the written
`mut`/`own` modes are part of the type. Contextually typed lambdas use that
same source-level callable signature; a capturing closure additionally owns
its hidden environment. Arbitrary stored and parameter `def` types describe
capture-free code pointers; compiler-known callback and task-start sites
preserve the additional closure metadata. `intsize` and
`uintsize` follow the target pointer width, and host process exit transport may
narrow an `int32` after Aura returns it. Other numeric widths and overflow
behavior are language-defined rather than implementation-defined. FFI v0
opaque handles are nominal non-Copy, non-cloneable, non-Transfer wrappers for
one non-null foreign pointer. Extern functions are direct-call-only
declarations rather than `def(...) -> ...` values.

## Status

The scalar, collection, enum, class, trait-bound, resource, optional, result,
and indirect types described by this Manual are implemented for the post-Phase
1.5 surface. Ordinary `-> T` return values are owned. An explicit
`-> view [mut] T from origin` declaration instead returns non-owning access,
but a view descriptor is not an owned or structural `def(...) -> R` type and
cannot be stored in fields or collections. Capture-free function types and
by-value expression closures are implemented. FFI v0 fixed-width declarations, byte/string views,
and opaque handle types are implemented; extern functions do not become
first-class function values. Method-value types are unavailable.
Structural tuple types
and their Batch 3 B3.0-c equality amendment are Accepted under ADR-0026.
`str` is the owned UTF-8 text type. A distinct borrowed text-view type is
unavailable. None of the unavailable types may be inferred from current
syntax.
