# Static Semantics

Static semantics are the rules applied after parsing and module loading and before MIR lowering or native code generation. A module is well typed only if every declaration, statement, expression, pattern, call, move, and borrow satisfies these rules.

This chapter states the cross-cutting rules. The declaration-specific chapters provide additional contracts, and [Ownership And Borrowing](/manual/ownership-and-borrowing) defines place and lifetime restrictions.

## Types And Type Equality

Aura 0.3 primarily uses nominal types with invariant generic arguments. Two
nominal types match when their canonical names and recursively all type
arguments are equal. Tuple types are structural: two tuple types match exactly
when their arity and every corresponding element type match recursively. There
is no general subtype relation and no implicit numeric widening.

Examples:

- `int` and `int64` are the same canonical type.
- `int32` and `int64` are different types.
- `list[int32]` and `list[int64]` are different types.
- two user classes with identical fields are still different types.
- an imported type retains its defining module identity even when imported under an unqualified binding.

`T?` is syntactic sugar for `Option[T]`. `int` canonicalizes to `int64`, and `str` currently canonicalizes to `str`; neither alias introduces a distinct runtime type.

Every generic type use must supply its declared number of type arguments. Non-generic types reject type arguments. `Self` is available only in supported trait and implementation type positions.

## Contextual Inference

Aura uses local, contextual inference rather than global inference. Public function parameters, fields, method signatures, and explicit return values remain typed in source. The checker uses an expected type from an annotation, parameter, return position, collection, constructor field, or surrounding expression where the rule is unambiguous.

### Literals

- An integer literal adopts an expected integer type and must fit it. In an expected `float32` or `float64` context it adopts that floating type only when its mathematical integer value is exactly representable there; an inexact case is a static error that directs the author to an explicit floating spelling or `.to_float()`; otherwise it defaults to `int64`.
- A negative integer literal is parsed as unary `-` applied to a non-negative literal. It follows the same exact float-context rule, or must fit the selected signed integer type.
- A floating literal adopts an expected `float32` or `float64`; otherwise it defaults to `float64`.
- `true` and `false` have type `bool`.
- A single-quoted, double-quoted, triple-quoted, raw, or formatted string has
  type `str`; delimiter and literal form do not create distinct types. Each
  f-string format specification is checked against the interpolation's static
  type. String-only, integer-only, numeric-only, sign, precision, and grouping
  restrictions are compile-time errors under `AU2002`.
- A duration literal has type `Duration`.
- Bare `None` has type `None`, except in an expected `Option[T]` position where it denotes `Option.None` of that type. Expected-option context flows through grouping, annotated bindings, return positions, and argument positions. For `==` and `!=`, when either operand has static type `Option[T]`, a bare `None` on the other side is contextually typed as that same option specialization; this rule is symmetric. Unit `None == None` is `true` and unit `None != None` is `false`. A qualified `Option.None` with no expected specialization remains an inference error.

### Collections

A non-empty list, set, or dictionary infers its element/key/value type from the first
value unless an expected collection type is available. All remaining values
must have the same inferred type. Equal keys in one dictionary literal are permitted;
the later value replaces the earlier value at runtime without changing the
key's first insertion position.

An empty list or dictionary literal requires an expected `list[T]` or
`dict[K, V]` type. `{}` is a dictionary literal. An empty set uses
`set[T]()`.

### Comprehensions

A list comprehension has type `list[T]`, a set comprehension has type `set[T]`,
and a dictionary comprehension has type `dict[K, V]`. An expected collection
specialization flows into the element, key, and value expressions before
inference. Otherwise those output expressions determine `T`, `K`, and `V`
under the ordinary exact-type and contextual-literal rules. A filter must have
exactly type `bool`.

Clauses are checked in runtime order. A clause iterable is checked before its
target enters scope. The target receives the same type and ownership
provenance as an ordinary bare `for` target, then becomes visible to that
clause's filters, later clauses, and the output. Targets cannot shadow visible
names or earlier targets and do not escape the expression.

Every clause reuses the statement bare-loop iterable classification. Lists and
sets provide shared elements, Range provides copy `int64`, the compiler-known
`enumerate` and `zip` forms retain their contracts, and Queue receives owned
items through its existing carve-out. `mut` and `own` clause modifiers are not
part of the syntax.

Output storage owns its inserted value. A copy value is copied and an owned
non-Copy value moves. A shared non-Copy target cannot be inserted without an
explicit clone-safe `.clone()` route. Queue targets already own their received
items. Move checking treats each clause as potentially repeated, retains every
active source borrow through downstream clauses and output evaluation, and
rejects loop-carried full or partial moves from outer places.

### Lambdas

A lambda with parameters requires an expected structural function type. That
type fixes its parameter count and each bare/`mut`/`own` capability and
parameter type; an expected result constrains the body. The body is checked
once under those parameter bindings. Aura does not infer parameter types
from body operations. A zero-parameter lambda may infer `def() -> R` from its
body when no expected callable type is present.

Without a capture list, outer owned locals and `own` parameters referenced by
the body are captured by value. Copy values snapshot and non-Copy values move
when the lambda expression is evaluated. An explicit exhaustive list may
instead request a shared loan, mutable loan, or owned capture for each used
outer local. Mutable-loan closures require a mutable closure place for calls;
loan closures are non-Transfer and remain inside synchronous local use. See
[Closures](/manual/closures).

Capture-free lambdas may cross every ordinary structural function-value
boundary. Capturing closures retain environment and call-kind metadata, so
they cannot coerce through arbitrary written-`def` parameters or stored
fields, collections, and annotated returns. Immutable local bindings,
compiler-known repeatable callbacks, direct calls, and qualifying task starts
preserve the metadata.

### Generic Calls

Generic type parameters are inferred by unifying argument types with parameter type patterns and, where available, the expected result type. Explicit specialization such as `identity[int64](value)` seeds or fixes the substitutions.

Every declared type parameter must resolve. The substituted type must satisfy all declared trait bounds. Inference does not guess from unrelated declarations or from runtime values.

## Declarations

A declaration is valid only when:

- its item name does not collide with another local/imported item or a reserved builtin
- type parameter names are unique and their bounds name known traits with correct arity
- field, variant, and method names are unique within the relevant declaration
- all referenced types exist with the correct arity
- default expressions have exactly the declared parameter or field type
- a non-`None` function or method returns on every statically reachable fallthrough path
- every view-returning declaration names one bare or mutable receiver/parameter
  origin, and every `return view` derives from that origin with matching kind
- copy classes contain only copy-compatible fields
- trait implementations satisfy the trait's type arguments, supertraits, method set, and method signatures

Class, enum, function, and trait declarations may be `public` at module scope. `impl` cannot be public because it introduces no independently imported item.

An extern declaration participates in the module namespace but has no Aura
body. The ABI must be `"C"`, its package must be authorized, and its complete
signature must belong to the fixed FFI v0 scalar/view/opaque-handle table.
Extern functions are direct-call-only: referencing one without immediately
calling it is rejected rather than producing a function value. An opaque
declaration contributes a nominal type but no constructor, fields, methods, or
Aura-visible layout. See [FFI v0](/manual/ffi).

## Bindings And Assignment

The first simple-name assignment introduces a binding. Its type is the annotation when present, otherwise the initializer type. The initializer must match exactly.

`mut` makes the new binding assignable and a mutable place. Reassignment requires an existing mutable place and preserves the original type. Reassignment reinitializes a fully moved binding or field when the assigned value has the correct type.

Compound assignments `+=`, `-=`, `*=`, `**=`, `/=`, `%=`, `//=`, `&=`,
`|=`, `^=`, `<<=`, and `>>=` read the current target, apply the corresponding
binary operation, and write the result only after success. The target must
already exist, be mutable, not be moved, and have the operation's result type.
Integer `/=` is rejected by the same rule and teaching diagnostic as integer
`/`; floating `/=` remains valid.

Field assignment requires a mutable base place and a declared field. Index
assignment supports `list[T]` with the `int64` index domain and `dict[K, V]`
with a key of exactly `K`. Simple dict index assignment accepts any `V` and
replaces an equal existing key or inserts a new entry. Its key and value are
owned storage positions: each is consumed when non-copy. The key is fully
evaluated and captured before the assigned value is evaluated, so value-side
effects do not retarget the write. Compound dict indexed assignment is permitted
only for copy `V`; non-copy `V` is rejected rather than implicitly cloned or
destructively removed before the operator completes. An annotation and `mut`
are not permitted on member or index assignment.

## Expression Typing

### Unary Operators

- `not value` accepts `bool` and returns `bool`, or resolves a matching `Not.not` trait operation.
- `-value` accepts an integer or float and returns the same type, or resolves a matching `Neg.neg` operation.
- `~value` accepts an integer and returns the same exact integer type.
- `try value` requires `value: Result[T, E1]` and an enclosing return type `Result[U, E2]`; it has type `T` when `E1 == E2` or an applicable `impl From[E1] for E2` exists.

### Binary Operators

Built-in operator typing is:

| Operators | Operand rule | Result |
| --- | --- | --- |
| `and`, `or` | both `bool` | `bool` |
| `+` | equal integer types, equal float types, two `str` values, or two Duration values | operand type |
| `-` | equal integer types, equal float types, or two Duration values | operand type |
| `*` | equal integer types, equal float types, `Duration` and `int64` in either order | numeric operand type, or `Duration` |
| `**` | equal integer types or equal float types | operand type |
| `//` | equal integer types, equal float types, or `Duration // int64` | numeric operand type, or `Duration` |
| `%` | equal integer or equal float types | operand type |
| `/` | equal float types | operand type |
| `&`, `|`, `^` | equal concrete integer types | operand type |
| `<<`, `>>` | equal concrete integer types | left operand type |
| `==`, `!=` | equal operand types | `bool` |
| `<`, `<=`, `>`, `>=` | equal integer types, equal float types, or two Duration values | `bool` |

When both operands have the same integer type, `/` is rejected with this exact maintained diagnostic:

```text
integer `/` is not supported; use `//` for floor division, or call `.to_float()` on both operands for true division
```

Arithmetic and ordering operators may otherwise resolve through the
corresponding `Add`, `Sub`, `Mul`, `Div`, `FloorDiv`, `Mod`, or `Ord` trait
method. Builtin numeric and Duration rules take precedence over operator-trait
dispatch. Builtin equality does not dispatch through an operator trait in
Aura 0.3.

Tuple `==` and `!=` require operands with the same static tuple type. They
apply builtin equality recursively to corresponding element types and produce
`bool`; nested tuple elements apply the same rule. Both operands are read, not
consumed. Runtime metadata attached to a tuple value is not a further
type-compatibility or equality input. Tuple `<`, `<=`, `>`, and `>=` are
rejected: structural tuple types have no lexicographic ordering and cannot
acquire one through `Ord`.

When one equality operand is a tuple literal and the other has a known tuple
type, the known type contextually types the literal recursively. The rule is
symmetric. Each equality link in a comparison chain applies the same
contextual typing before enforcing exact operand-type equality.

Operator operands are not implicitly widened. An integer literal may be contextually typed to match an integer operand, or a `float32`/`float64` operand when the literal is exactly representable in that floating type. A floating literal may adopt the other operand's floating type. Non-literal values require an explicit numeric cast or integer `.to_float()` conversion.

Integer power requires a non-negative exponent. A negative exponent visible in
source is `AU2003`; a negative value discovered only during execution is a
runtime failure. Bitwise operations, shifts, and power are builtin numeric
operations and do not dispatch through operator traits.

### Conditions

`if` and `while` conditions, including the condition in
`value if condition else alternative`, must have exactly type `bool`. `and`,
`or`, and `not` also require boolean results under the rules above. Aura does
not apply general truthiness conversion to strings, collections, resources, or
user types.

### Assertions

An `assert` condition must have exactly type `bool`. Its optional message must
have exactly type `str`. Both mismatches use `AU2002` at the retained
`assert` keyword span.

The checker evaluates the condition's ownership effects first. It checks an
optional message from the resulting state, but because that expression is
runtime-lazy, message-only moves and mutations are not applied to the
fallthrough state. The statement itself has ordinary fallthrough and performs
no lasting type or value refinement.

### Indexing, Slicing, And Members

Direct indexing supports `list[T]` with the `int64` index domain, `dict[K, V]`
with exactly `K`, and `Array[T]` with one `int64` coordinate per runtime
axis. For a list, a negative index `i` is normalized once as
`len + i` before the existing bounds check; this applies equally to direct
reads and writes and to `get`, `set`, `pop`, and both `swap` indexes.
Fixed-width `int8`, `int16`, `int32`, `uint8`, `uint16`, and `uint32` values
widen losslessly only at these positions. Direct access, `set`, `pop`, and
`swap` fail at runtime when the normalized position is invalid, while `get`
returns `None`. `insert` clamps its position to `0..=len`.

A direct read produces `T` or `V` only when that element/value type is
copyable. For a non-copy list element, use `get(index)` for an explicit
cloned optional read only when the element type is clone-safe. For a non-copy
dictionary value, use `get(key)` only when the value type is clone-safe, or
`remove(key)` to transfer ownership. These non-copy
direct-read rejections use `AU3005`; a non-copy indexed compound assignment
uses `AU3006` because its initial read has the same ownership problem. A missing dictionary key
in a direct read is runtime diagnostic `AU4003`. Integer indexing is not
defined for `str`.

A slice suffix is defined on `list[T]`, `str`, and `Array[T]`. It returns a
fresh owned value of the same source type. Each written endpoint uses the
`int64` position domain, including lossless widening from the supported narrow
fixed-width types. An omitted endpoint contributes no
expression. After one `len + i` normalization for each negative written
endpoint, both effective endpoints must lie in `0..=len` and start must not
exceed end. Invalid or reversed bounds are runtime `AU4003`.

List slicing establishes a clone-producing obligation for `T`: Copy elements
are copied and non-Copy elements must be clone-safe. A concrete or transitive
`random.Rng` element is rejected with `AU3007`; a non-repeatable Task
observation right is rejected with `AU3009`; unresolved generic `T` carries
the inferred obligation to specialization. String slicing counts Unicode
scalar values and returns `str`. A slice is not a place and cannot be the
target of assignment or mutable access. Step syntax and slice assignment are
reported as unsupported forms with `AU2005`.

`Array[T]` is specialized only by `int32`, `int64`, `float32`, or `float64`.
Its constructors require exact `list[int64]` shape metadata. Array slicing
uses the same one-colon grammar and endpoint rules but copies only a first-axis
range, retaining the remaining runtime dimensions. Array/Array arithmetic
requires identical `T`; runtime shape equality is checked with `AU4007`.
Scalar arithmetic requires exactly `T`, with no mixed promotion or
broadcasting. Integer Array `/` is `AU2003`.

Array `map[U]` requires exact repeatable `def(T) -> U` and restricts `U` to
the four Array dtypes. `sum`, `min`, and `max` return `T`; `mean` returns
`float64` for all dtypes. Mutable `set`, `fill`, and indexed assignment
require a mutable Array place. Array `get` converts an invalid coordinate or
runtime-rank mismatch to `None`; `set` traps for either failure and returns
the replaced scalar in `Some` only after a valid update.

Member access must resolve to a visible field, method, enum variant, module item, or maintained builtin member. Calling a receiver method also validates whether the receiver is consumed, shared-borrowed, or mutable-borrowed.

A non-copy place selected as a binary left operand, index base, method receiver,
or indexed-assignment target remains borrowed through the operation's later
inputs. Another shared borrow is valid, but an overlapping mutable borrow or
consumption is rejected with `AU3002`, with the retained selection reported as
the borrow origin. The same rule applies to name roots and projected member
places. The checker never legalizes the operation by assuming a deep clone.
Equality and inequality retain this borrow through the right operand and
consume neither operand; tuple equality does not introduce a recursive move.

## Call Binding

Arguments are written as positional arguments followed by named arguments. Binding proceeds against the declaration or builtin metadata:

1. positional arguments fill parameters in declaration order
2. named arguments fill the parameter with the same name
3. a parameter cannot be filled twice
4. unknown names and extra arguments are rejected
5. omitted parameters require defaults
6. each argument type must equal the substituted parameter type

Default expressions are evaluated for each call where the parameter is omitted.
Every supplied argument expression is evaluated first in call-site source
order before the next supplied expression begins. A copy or move result is
captured in its argument slot; a borrow-mode selection is established without
cloning and is checked under the retained-borrow rule above. Later side effects
therefore cannot re-read or change an earlier captured argument. Defaults for
omitted parameters are then evaluated in declaration order. Binding a named argument to its
declaration slot does not reorder evaluation, and no default is evaluated for a
supplied parameter. Defaults may refer only to names valid under the
declaration's default-expression rules; they do not capture a caller's locals.
A bare shared default's temporary lives through the call. An `own` default is
consumed. A `mut` default is rejected because mutations to its
caller-invisible temporary would be silently lost.

A bare parameter grants logical shared access for every type. The ABI may pass
copy bits directly, but specialization never changes the declared capability.
An `own` parameter consumes a non-copy argument, and a `mut` parameter requires
a mutable place. All arguments at one call boundary are checked together for
overlapping move/shared/mutable access.

An indirect call through a function value follows the same rules. When the
value has one statically known declaration, binding uses that declaration's
parameter names and defaults. A control-flow join retains those extras only
when all candidate contracts agree on names and default availability; an
omitted argument evaluates the runtime-selected target's own default
expression. Conflicting reassignment, return through a structural function
annotation, class-field storage, and mutable-collection storage erase the
extras, so the call supplies every parameter positionally.
The structural type does retain each parameter's capability: bare is shared,
`mut` requires a mutable place and caller-visible writeback, and `own`
transfers a non-copy argument. Function-type assignment and substitution
require those modes, parameter types, and the return type to match.

Extern calls use ordinary positional/named argument binding and left-to-right
evaluation, then apply the declared FFI capabilities. Scalars require bare
parameters; `str` is a bare const UTF-8 view; `list[uint8]` is a bare const
byte view or `mut` fixed-length byte view; opaque handles permit bare sharing
or `own` consumption. A `mut` view requires a mutable place. Extern defaults,
generics, callbacks, variadics, returned views, and raw pointers are rejected.

Callable-powered list methods use exact structural callback types. `map`
requires `f: def(T) -> U`; `filter` requires
`f: def(T) -> bool`; and keyed `sort` requires
`key: def(T) -> K`. Bare callback parameters are shared capabilities. A
callback with `mut T` or `own T` is not substitutable. `filter` is
clone-producing and adds the ordinary clone-safety obligation for `T`.
`sort` requires `T` to support the existing natural `<` ordering, while
keyed `sort` requires that ordering for `K`. Both require a mutable list place.
`map` and `filter` retain a shared receiver.

`control.retry` requires
`worker: def() -> Result[T, E]`, `max_attempts: int32`, and
`initial_backoff: Duration`. The worker function type, including its empty
parameter list and `Result` return identity, must match exactly. `T` and `E`
are inferred from that return specialization or may be supplied through
ordinary explicit generic specialization. The callback is not widened from a
function with parameters or a different return type.

## Class Construction

Calling a class name constructs a value. Constructor fields may be supplied
positionally in declaration order, then by name. Positional arguments cannot
follow a named argument. Every field without a declaration default must be
supplied exactly once; provided and default values must match the substituted
field types. Supplied field expressions follow the same source-order capture
rule as call arguments, so a later field expression cannot change an earlier
captured field value.

A class receiver is declared as shared `self` (or its explicit synonym `self`), consuming `own self`, or mutable `mut self`. A first method parameter written `self: Type` is rejected with guidance naming those forms. A method without a receiver is associated and is called through the class/type rather than an instance.

## Enum Construction And Matching

An enum variant constructor must name an existing variant and provide exactly
its payload shape. A variant declares either all positional payloads or all
named payloads; constructors bind accordingly. Supplied named payload
expressions evaluate in their written source order, each result is captured,
and those results then bind by payload name to the variant's declaration-order
slots. Declaration-slot binding does not reorder evaluation.

Generic enum constructors require sufficient context to determine all type arguments. This may come from explicit specialization, an expected annotation/parameter/return type, or payload inference. Bare builtin variants such as `Some`, `Ok`, `Err`, or `None` are accepted only where the expected enum identity is unambiguous.

A match pattern must be compatible with the scrutinee type. Variant payload subpatterns must have exactly the variant's arity. Literal patterns must match the scrutinee's supported scalar type. Duplicate or unreachable arms are rejected where the checker can establish overlap.

Matches over enums and booleans must be exhaustive unless `_` covers the remainder. Literal matches over open numeric/string domains require `_`. Every arm of a match expression must produce the same result type, using the surrounding expected type where available.

A conditional expression checks both value arms against one result type.
Surrounding expected context applies to both arms. Without such context, a
context-dependent literal may adopt the type established by the other arm; no
rule widens or converts an already-bound value. The condition is checked first,
then each arm starts from the resulting ownership state.

## Generics, Traits, And Implementations

Traits are nominal interfaces. A bound `T: A + B` requires an applicable implementation of each trait after substitution. Supertraits are inherited requirements.

An `impl` identifies one trait specialization and one target type pattern. Its methods must correspond to trait methods; missing required methods are rejected unless the trait provides a default body. Extra methods are not part of that trait implementation.

For a concrete receiver, the checker chooses the unique applicable implementation with greatest specificity. If multiple equally specific implementations apply, the call or operator is ambiguous and rejected. Source order is not a tie breaker.

For a type parameter, available methods and operators come from its declared bounds. If multiple bounds expose an indistinguishable method, the access is ambiguous unless the language can resolve one unique contract.

Trait and implementation methods cannot declare default ordinary parameters in Aura 0.3. Trait default method bodies are permitted; a signature-only trait method has no body after its terminating newline.

A clone-producing operation over unresolved generic types infers clone-safety
obligations on the contributing declared parameters. Calls propagate those
obligations to a fixed point and discharge them after substitution. The
contract applies equally to ordinary, imported, inherent, associated, bounded
trait, operator, task-target, and `From` calls. A concrete type that contains
non-cloneable `random.Rng` state, or whose safety cannot be proved, is rejected
with `AU3007`.

An obligation inferred from a trait default method is part of that method's
contract and is structurally substituted through `Self`, trait arguments, and
method arguments. An explicit implementation may satisfy that contract but
MUST NOT add a clone-safety requirement absent from it. Recursive nominal type
inspection terminates conservatively rather than assuming an expanding cycle
is safe.

## Control Flow

`return` is valid only in a function or method. Its ordinary value must equal
the declared return type; an omitted value has type `None`. `return view` is
valid only under a matching declared view return and must preserve the exact
receiver/parameter provenance root.

`break` and `continue` are valid only inside `for` or `while`. A loop-local binding does not escape. Moving a non-copy outer value for the first time inside a repeatable loop is rejected unless the checker can prove the path does not create an invalid next iteration.

A comprehension is expression control flow, not a statement loop: `break`,
`continue`, and `return` cannot appear in its clauses or output. Each filter
checks one conditional path. Later clauses and output effects apply only on the
path where every preceding filter is true, and resulting ownership state is
merged conservatively. `try` remains an expression and may propagate from a
reached source, filter, key, value, or element after cleaning up the partial
result.

An `if`, statement match, match expression, or conditional expression checks
branches independently and merges move/partial-move state conservatively
across reachable paths. A non-`None` function is rejected when any reachable
path can fall through without returning.

## `with` Resources

`with` consumes its resource expression and creates a mutable managed binding for the body. Supported builtin resources have runtime-defined cleanup. A non-generic user class may be used only when it declares exactly a `close(mut self) -> None` instance method.

The managed binding cannot be moved out in a way that would prevent required cleanup. Leaving the scope normally, by return, by loop control, or through a runtime failure runs the cleanup behavior described in [Execution Model](/manual/execution-model).

## Tasks And Static Safety

`TaskGroup.start`, `start_soon`, `start_with_stack`, and
`start_soon_with_stack` accept capture-free function values and closure values
as well as the existing direct named-function and
associated-method-without-`self` targets. The explicit-stack methods first
require an exact `int64` capacity.
Target arguments are copied or moved into task-owned capture storage
independently of the target ABI. Bare shared target parameters borrow that
storage for the child call; `own` parameters
consume it. Generic targets also enforce their inferred clone-safety
obligations after specialization. `mut` target parameters are rejected.

Under the Accepted ADR-0033 Phase 5.6 contract, each value captured by
these four start methods and the specialized target return type must be
`Transfer`. This is a compiler-derived structural obligation, not a builtin
user trait. A same-named ordinary user trait cannot confer the property.
It follows collection, tuple, class, and enum storage to the first
non-transferable leaf. All copy types and `str` qualify; aggregates qualify
when every stored component does; `Queue[T]` and `Task[T]` handles qualify
without traversing `T`. Capability views, `random.Rng`, `TaskGroup`, and live
host resources do not qualify unless a later compiler-owned whitelist names a
specific type.

A task-start expression that reads a Copy value through shared or mutable
access captures an owned Copy snapshot, not the access capability, and is
allowed when that value type is Transfer. Non-copy access cannot be captured
by value without ownership, and the capability itself never crosses.

Queue construction, `put`, and `try_put` require the payload `T` to be
`Transfer`; handle-only receive/fallback/close operations do not recheck it. A
fully concrete generic specialization is checked structurally,
but an unresolved type parameter at a task or Queue boundary is rejected
conservatively; Phase 5.6 does not infer a deferred Transfer contract. A
rejection uses `AU3008`, identifies the task or Queue boundary, and gives the
nested component path that caused it, such as a field that contains `fs.File`;
it does not suggest implementing a `Transfer` trait.

A by-value closure target is Transfer exactly when every stored capture is Transfer.
The complete closure value is moved or copied into task-owned storage before
the child calls it. Capture-free lambdas are Copy and Transfer. A closure with
any shared or mutable loan capture is non-Transfer and rejected with `AU3008`.

Task-target resolution accepts a concrete function value. Explicit
`function[Types]` specialization may produce such a value before the call;
the direct associated-method `Type.associated_method[Types]` spelling remains
limited to the callable-target slot. A bare target whose declared/default
context already resolves its complete types is also concrete.

ADR-0008 also distinguishes repeatable and single-consumer task results.
`Task[T]` is copyable only when `T` is copyable, `T` is `Queue[...]`, or `T`
is a recursively repeatable `Task[...]`. For any other transferable `T`,
`result`, `result_or_none`, and `result_or` consume the unique observation
right on every outcome. `wait_any` and `wait_all` consume the complete task
list; `wait_any` abandons the unchosen rights. `select(...)` consumes every
non-repeatable Task source at call entry and abandons each losing right. This
prevents handle aliases, including nested `Task[Task[str]]`, from producing
a second value.
Attempts to clone, read through a clone-producing collection method, or copy
an aggregate containing such a right use `AU3009`. Reusing the task binding
after a consuming observation uses ordinary moved-value `AU3001`; consuming
through shared access is `AU3002`.

Task, queue, and cancellation runtime semantics are defined by [Concurrency](/manual/concurrency).

## Entrypoint Rules

The selected entry module may use one of two shapes:

- executable top-level statements and no local `main`
- a local `main` and no executable top-level statements

The local `main` takes no parameters and returns `None` or `int32`. Imported functions named `main` are ordinary imported functions and do not become the entrypoint.
