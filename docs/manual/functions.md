# Functions

Functions are module-level declarations introduced by `def`. Their callable
contracts fix the parameter names, parameter passing modes, parameter types,
generic parameters and bounds, return behavior, and any inferred clone-safety
obligations used at every call site.

```aura
def add(left: int32, right: int32) -> int32:
    return left + right
```

The complete declaration grammar is in [Grammar](/manual/grammar#functions-methods-and-parameters). This chapter defines the corresponding static and execution rules.

## Signatures And Return Types

Every ordinary parameter has an explicit type. A return annotation is optional;
omitting it is exactly equivalent to `-> None`. Ordinary `-> T` returns are
owned; a returned view uses the separate `-> view [mut] T from origin` form.

```aura
def square(value: int32) -> int32:
    return value * value

def log(message: str):
    print(message)
```

`return expression` must have exactly the declared return type. `return` without an expression has type `None` and is valid only in a `None`-returning function. Reaching the end of a `None` function returns `None` implicitly.

A function may return one fixed structural tuple. Both the return annotation
and value use parentheses, and a comma distinguishes a singleton tuple from
grouping: `def locate() -> (str, int64):` may
`return ("north", 7)`. The caller may bind the result with
`name, number = locate()`. Tuple return copy/move behavior follows the complete
tuple's recursive classification; see [Tuples](/manual/tuples).

A function with any other return type must return on every statically reachable path:

```aura
def classify(value: int32) -> str:
    if value < 0:
        return "negative"
    return "non-negative"
```

There is no implicit numeric widening or general return coercion. A bare
`None` in an argument or return position adopts an expected `Option[T]`, and
grouping does not discard that context. Other contextual literal typing and
the complete symmetric option-equality rule follow [Static
Semantics](/manual/static-semantics#contextual-inference).

Function names share the module item namespace with classes, enums, traits, and imports. Duplicate items and attempts to redefine maintained builtin function names are rejected. Ordinary parameter names must be unique. A method parameter also cannot be named `self` when the method has a receiver. In a method declaration, `self: Type` is rejected rather than treated as an ordinary first parameter; receivers use `self`, `own self`, or `mut self`. See [Names And Scopes](/manual/names-and-scopes) for the complete namespace rules.

A function is private to its defining module by default. Prefix the declaration with `public` to make it importable from another module:

```aura
public def double(value: int32) -> int32:
    return value * 2
```

Visibility controls name access, not the ownership or type rules of the signature.

## Parameter Passing Modes

The passing mode is part of the function signature:

| Declaration | Contract at the call boundary |
| --- | --- |
| `value: T` | Shared access. An implementation may pass copy bits directly without changing the source contract. |
| `value: own T` | Owned argument. A move value is consumed; a copy value is duplicated. |
| `value: mut T` | Exclusive mutable borrow. The argument must be a mutable place. |

```aura
def consume(name: own str):
    print(name)

def length(text: str) -> int64:
    return text.len()

def push_name(names: mut list[str], name: own str):
    names.append(name)
```

The modifier is written in the declaration after the colon. Calls pass the
expression directly; Aura has no call-site capability prefix:

```aura
mut names = list[str]()
push_name(names, "Ada")
```

Arguments must have exactly the substituted parameter type. A call retains each
non-copy method receiver and non-copy argument access through every later sibling
expression. A later sibling, including an access nested inside another call or
expression, may shared-borrow the same place, but it may not mutably borrow or
consume an overlapping place; violations report `AU3002`. The ownership and
place rules are specified in [Ownership And Borrowing](/manual/ownership-and-borrowing).

The bare rule is resolved where the function is declared, not independently
at each call. An unconstrained generic `value: T` therefore resolves to a
shared borrow because `T` is not known copyable there. That choice is
**declaration-stable**: specializing the function later with `T = int32` does
not turn the parameter into an owned value. Write `value: own T` when a generic
function must consume or return its argument.

## Call Binding

Calls accept positional arguments followed by named arguments:

```aura
def render(name: str, count: int32 = 1):
    print(name)

render("Aura")
render("Aura", 2)
render(name="Aura", count=2)
```

Every declared parameter is positionally bindable. Aura 0.3 structural
callable types do not encode keyword-only callability, so a `*` marker in a
parameter list is rejected with `AU1101`.

Binding is deterministic:

1. positional arguments fill parameters in declaration order
2. named arguments fill the parameter with the same name
3. one parameter cannot be filled twice
4. unknown names and excess positional arguments are rejected
5. every omitted parameter must have a default
6. each bound argument must have the parameter's exact substituted type

Positional arguments cannot follow a named argument. Parameter and argument
lists may span physical lines while their parentheses remain open, but they do
not accept trailing commas in Aura 0.3.

## Default Arguments

A default is permitted on a bare shared or `own` parameter of a top-level
function or class method:

```aura
def greet(name: str = "world"):
    print("hello " + name)
```

The complete rules are:

- `mut` parameters cannot have defaults, regardless of whether their
  types are copyable; the default would be a caller-invisible temporary, so
  every mutation would be a silent lost write. Require the caller to pass a
  value, or take the parameter as `own T` and return the result
- a shared-borrow default is permitted; its default temporary lives until the
  call completes
- an `own` default is permitted and its fresh temporary is consumed by the call
- after the first defaulted parameter, every remaining parameter must also have a default
- the default expression must have exactly the declared parameter type
- a default expression cannot reference any parameter of the same declaration, including an earlier parameter
- trait method declarations and trait implementation methods cannot declare defaults

Defaults are evaluated afresh when the corresponding argument is omitted. They
are not process-global singleton values. Every supplied argument is evaluated
first in call-site source order before the next supplied expression begins. A
copy or move result is captured in its parameter slot; a borrow-mode selection
is established without cloning and remains subject to the retained non-copy
overlap rules. Later side effects cannot change an earlier captured argument.
Defaults for omitted parameters are then evaluated in declaration order.
Binding named values to parameter slots never reorders their evaluation, and a
supplied argument suppresses its default. See [Execution
Model](/manual/execution-model#evaluation-order).

## Named Arguments For Builtins

Maintained builtin functions and methods use the same binding rules, with parameter names defined by their API metadata:

```aura
import process

process.run(["/bin/echo", "hi"], stdout=process.pipe(), group=true)
```

```aura
import net

net.http_request_text_timeout(method="POST", url="http://127.0.0.1:8080/jobs", body="{}", headers={}, timeout=2s)
```

The module chapters and [API Index](/manual/api-index) are authoritative for builtin parameter names, defaults, and return types.

## `try` And Result Returns

`try` is valid only when its operand has type `Result[T, E1]` and the enclosing function returns `Result[U, E2]`:

```aura
def parse_total(left: str, right: str) -> Result[int32, str]:
    a = try parse_int32(left)
    b = try parse_int32(right)
    return Result.Ok(a + b)
```

`Result.Ok(value)` makes `try` evaluate to `value`. `Result.Err(error)` returns from the enclosing function immediately. `E1` must equal `E2`, or an applicable `impl From[E1] for E2` with a `from` method must be visible. Active `with` cleanups run during this early return. See [Execution Model](/manual/execution-model#try).

## Owned Returns

Every ordinary type return annotation describes an owned result:

```aura
class User:
    name: str
    score: int32

def score(user: User) -> int32:
    return user.score
```

Here the caller receives an ordinary `int32` copy. Copy results need no
provenance annotation because they are independent owned values.

For a non-copy result, the function must produce ownership. It can construct a
fresh value, clone a clone-safe value, move from an `own` parameter, or invoke
an operation that consumes an owner:

```aura
def copy_name(user: User) -> str:
    return user.name.clone()

def into_name(user: own User) -> str:
    return user.name
```

A bare or `mut` parameter grants access but does not give the function
ownership of a non-copy value stored behind that access. Moving such a value
into the result is rejected; use one of the owned-result patterns above.

Every ordinary result is an owned value. See [Ownership And
Borrowing](/manual/ownership-and-borrowing#owned-returns).

## Returned Views

A function or method can return non-owning access to one declared receiver or
parameter origin:

    class User:
        name: str

    class Counter:
        value: int64

    def name(user: User) -> view str from user:
        return view user.name

    def value_mut(counter: mut Counter) -> view mut int64 from counter:
        return view mut counter.value

    def bump(value: mut int64):
        value += 1

The `from` origin is mandatory and belongs to the signature. It is encoded by
receiver/parameter slot across modules and trait conformance, so an
implementation may rename its parameter but may not select another slot. A
shared result may use a bare or `mut` origin. A mutable result requires a
`mut` origin. Owned and defaulted parameters cannot be origins.

Every reachable `return view` must trace to the origin or one of its supported
fixed field or tuple projections. Locals, temporaries, enum arm payloads, and
newly allocated values cannot escape. At a call, the origin argument must be
an addressable place. A returned view may initialize a matching local `view`
or `view mut` binding. A shared result may instead be read directly within one
containing expression, and a mutable result may be immediately reborrowed into
a `mut` call. A returned view cannot initialize an ordinary owned binding or
be stored inside an aggregate.

    user = User(name="Ada")
    view current = name(user)
    print(current)
    print(name(user))

    mut counter = Counter(value=0)
    view mut editable = value_mut(counter)
    editable += 1
    print(counter.value)
    bump(value_mut(counter))
    print(counter.value)

Return paths may select different fixed projections of the one declared
origin. The caller locks that origin conservatively and execution retains the
exact selected projection; selecting a different root is `AU3010`. Structural
`def(...) -> R` types cannot represent returned-view origin metadata and
therefore do not accept these functions.

## Generic Functions

Type parameters follow the function name:

```aura
def identity[T](value: own T) -> T:
    return value
```

Bounds restrict substitutions:

```aura
def describe[T: Greeter](value: T) -> str:
    return value.greet()

def use_both[T: First + Second](value: T) -> int32:
    return value.score()
```

The checker infers type arguments from call arguments and an available expected result type. Explicit specialization fixes them:

```aura
answer = identity[int64](42)
```

Every type parameter must resolve, all bounds must hold, and explicit type arguments must have the declared arity. See [Generics And Traits](/manual/generics-and-traits#inference-and-specialization).

A clone-producing operation over an unresolved type parameter does not make the
generic declaration invalid. The checker infers a clone-safety obligation for
that parameter. Calls discharge the obligation after substitution, and a
generic caller propagates an unresolved obligation as part of its own callable
contract. The requirement also applies when the callable is imported or used
as a maintained task target. See [Generics And
Traits](/manual/generics-and-traits#inferred-clone-safety-obligations).

## Function Values

A module-level named function is a value. Its type uses declaration-shaped
syntax: `def(T1, mut T2, own T3) -> R`. The parameter list contains modes and
types rather than parameter names, and `def() -> R` is the zero-parameter
form. Bare parameters are shared. Function types may appear anywhere another
complete type may appear, including variable and parameter annotations, class
fields, return types, and collection element types.

This includes public user-module functions and maintained builtin-module
functions such as `process.pipe`. Calling an imported builtin through a value
uses the same builtin dispatch and result type as calling its qualified name.

An inferred local binding retains the named function's exact declared
parameter modes. An indirect call through a `mut` parameter requires a mutable
place, and an `own` parameter moves a non-copy argument. Assignment and
argument passing compare these modes as part of the function type:
`def(mut Counter) -> None` does not match `def(Counter) -> None`.

Function values are code pointers. They are copy values, cloning is
unnecessary, and copying or passing one as an `own def(...) -> R` parameter
does not invalidate the source binding. They also satisfy `Transfer`.
Ordinary indirect calls evaluate and bind arguments under the selected
function's unchanged capability contract. A binding whose target declaration
is statically known retains that declaration's call contract: named arguments
are accepted and omitted arguments use its defaults. The structural
`def(...) -> ...` type itself does not contain names or default expressions.
A control-flow selection can retain names and default availability when every
candidate agrees; an omitted argument then evaluates the runtime-selected
target's own default expression. Reassignment between conflicting contracts,
return through a structural function annotation, class-field storage, and
mutable-collection storage erase those extras. Storage still retains the
complete ABI type, including `mut` and `own`; a loaded value takes every
argument positionally.

If an indirect-call default traps, diagnostics use the public target name and
the precise default-expression span. Compiler-generated default helpers never
appear in the call chain.

A generic named function must receive explicit type arguments, for example
`show_int = show[int32]`, or a concrete expected function type. Expected types
can specialize a variable annotation, argument, field, collection element, or
parameter default such as a generic `empty` used where
`def() -> Option[str]` is required. A generic name with neither source of
type arguments does not have one concrete function-value type.

This stage is deliberately capture-free. Instance-method, associated-method,
and trait-method values are not first-class; an associated method without
`self` remains accepted only in the existing direct `TaskGroup` target form.
Lambdas and closure capture are specified separately.

## Function Values And Task Starts

The ordinary and explicit-stack `TaskGroup` start methods accept a named
function value as their target. Existing direct named-function and
associated-method-without-`self` target forms remain accepted.

```aura
def work(value: int32) -> int32:
    return value * 2

worker = work

with group = TaskGroup():
    task = group.start(worker, 21)
```

Task capture ownership is independent of the target function's call ABI. Each
argument is first copied or moved into task-owned capture storage: `own` target
parameters consume their capture, while bare shared parameters access that
storage for the duration of the child call. `mut` targets are rejected because
mutable access to detached capture
storage has no caller-visible writeback contract. See [Concurrency](/manual/concurrency).

## `main`

In the selected entry module, a local function named `main` is the entrypoint when there are no executable top-level statements. Its only valid signatures are:

```aura
def main() -> int32:
    return 0
```

```aura
def main():
    print("done")
```

`main` takes no parameters and returns exactly `int32` or `None`. A returned `int32` becomes the requested host exit status; `None` means success. An imported function named `main` remains an ordinary imported function. A file cannot combine a local `main` with executable top-level statements.

The alternate top-level execution form, evaluation order, cleanup on return, and the 256-call runtime depth limit are specified in [Execution Model](/manual/execution-model#entry-module-execution).

## Grammar

Function and method declarations, generic parameters and bounds, receiver and
ordinary parameter forms, defaults, owned return annotations, and call
arguments are normative in [Grammar](/manual/grammar). Ordinary
functions are module items; nested function declarations are not accepted.
Expression lambdas are specified by [Closures](/manual/closures).

## Typing Rules

Every ordinary parameter has one declared type and declaration-stable passing
mode. Calls bind positional then named arguments, substitute inferred or
explicit generic arguments, enforce bounds and exact types, and fill only legal
defaults. They also enforce inferred clone-safety obligations after
substitution. Every reachable non-`None` path returns the declared type. Shared
or mutable access never authorizes moving a non-copy value into the result;
non-copy returns require an owned source.

## Runtime Semantics

The callee target is resolved statically. Supplied arguments evaluate left to
right and each result is captured before later argument side effects; omitted
defaults then evaluate freshly in declaration order, a call creates one frame,
and `return` transfers its value after exited cleanups run.
`try` may perform that return early. Entry `main` maps `None` to success or its
`int32` result to the requested host process status.

## Ownership And Evaluation Order

Bare parameters grant shared access; an implementation may pass copy bits
directly. `own` parameters consume their arguments; `mut`
requires one exclusive mutable place and writes through it. Borrowed default
temporaries live through the call, owned defaults are consumed, and mutable
borrow defaults are rejected as guaranteed lost writes. Task start first stores
owned captures and then invokes the target under its declared ABI.

## Diagnostics

`AU1101` means malformed function, method, parameter, return, or call syntax.
`AU2001` means the call target or referenced declaration could not be resolved.
`AU2002` means a signature, function-value capability, parameter, default,
return, bound, or entrypoint type mismatch. `AU2004` means positional or named
argument binding failed. `AU2005` means focused guidance for an unavailable
callable spelling, including out-of-scope method values. `AU2999` means another
callable rejection without a narrower compile-time code.
`AU3001` means a moved argument was used; `AU3002` means a borrow or alias
conflict; `AU3003` means a mutability violation; and `AU3004` means an invalid
parameter, receiver, return, or task-capture ownership mode. `AU3007` means a
call specialization would duplicate non-cloneable `random.Rng` state or could
not satisfy a callable clone-safety obligation. `AU3010` means a returned view
has an invalid escape, origin, caller place, kind, or provenance path. `AU4001` means a
call-depth or general call trap. A callee's `AU4002` means arithmetic overflow
or underflow, `AU4003` means bounds or lookup violation, `AU4004` means zero divisor, and
`AU4005` means a trapping resource or I/O failure; each retains the same typed
Aura call frames and task ancestry on MIR and direct-native execution.

## Backend Support

Ordinary, returned-view, indirect function-value, generic, imported,
associated, trait-dispatched, and maintained task-target calls are implemented
for MIR execution and direct native builds.
Shared semantic checking and the forced parity matrix require identical call
results and primary failures. Compiler analysis and the LSP use the same
resolved signature metadata, including inferred clone-safety obligations.

## Limits And Implementation-Defined Behavior

Aura has no method values, trait-object function interactions, Aura
variadic functions,
overloads, nested functions, or mutable-parameter task targets. Expression
lambdas are specified by [Closures](/manual/closures); they do not add nested
item declarations. Written function types express bare shared, `mut`, and `own`
parameter contracts. Runtime calls are limited to 256 nested Aura frames. Host
process exit representation may narrow the requested `int32` after it leaves
Aura; function binding and evaluation order are otherwise not
implementation-defined.

## Status

The function, method, generic, capture-free function-value, default-argument,
named-argument, ordinary owned-return, returned-view, inferred clone-safety,
task-target, and entrypoint contracts described above are implemented. Supplied/default
evaluation and argument capture follow
`architecture_docs/decisions/0015-explicit-and-default-argument-order.md`,
which is **Accepted**. The rules are pinned
by
`crates/aura-compiler/tests/fixtures/run-pass/explicit_and_default_argument_order.au`
on both backends. Ordinary `-> T` return values are owned; only an explicit
`-> view [mut] T from origin` contract returns non-owning access. By-value
expression closures are implemented under Accepted ADR-0037. FFI v0 adds bodyless direct-call-only
`extern "C" def` declarations; they are not function values and their
restricted signatures are specified by [FFI v0](/manual/ffi).
