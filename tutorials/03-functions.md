# Functions

Functions are declared with `def` and require explicit parameter types.

## Basic Functions

```aura check-pass
def add(a: int32, b: int32) -> int32:
    return a + b
```

The return type follows `->`. If a function does not return a value, you can omit the return type and it defaults to `None`:

```aura check-pass
def greet():
    print("hello")
```

Reaching the end of a `None`-returning function is allowed. You can also use a bare `return`:

```aura check-pass
def log_value(value: int32):
    print(value)
    return
```

See [examples/basics/main_function.au](../examples/basics/main_function.au).

## Parameters

Parameters are written with explicit types:

```aura fragment
def distance(a: Point, b: Point) -> float64:
    dx = a.x - b.x
    dy = a.y - b.y
    return (dx * dx + dy * dy).sqrt()
```

See [examples/classes/point_distance.au](../examples/classes/point_distance.au).

An unmodified parameter grants shared access for every type. An implementation
may pass copy bits directly, but that does not change the source-level
contract. Write `own` when the function takes ownership:

```aura fragment
def archive(doc: own Document):
    print(doc.title)
```

The choice is fixed at the declaration. For an unresolved generic `T`, the
bare form is a declaration-stable shared borrow even if a later call uses a
copy type; use `value: own T` for an identity, storing, or consuming helper.

## Borrowed Parameters

When a function only needs to read a value, give it shared access. The caller
keeps ownership and can continue using the value after the call. See
[06-ownership-and-borrowing.md](06-ownership-and-borrowing.md) for the full
explanation.

Use `T` for read-only access:

```aura fragment
def read(counter: Counter) -> int32:
    return counter.value
```

Use `mut T` for mutable access -- the function can modify the value and changes persist back to the caller:

```aura fragment
def bump(counter: mut Counter):
    counter.value += 1
```

A `mut` parameter requires a mutable binding at the call site:

```aura fragment
mut counter = Counter(value=41)
bump(counter)
print(counter.value)    # 42
```

Aura rejects overlapping arguments when `mut` is involved. Mutable access
must be exclusive -- no other overlapping access can exist in the same call:

```aura check-pass
# This would be rejected:
# bad(a: mut Counter, b: Counter) called with bad(c, c)
```

This rule prevents subtle bugs where a function reads from and writes to the same value through different parameters.

See [examples/basics/borrow_parameters.au](../examples/basics/borrow_parameters.au).

Task targets may use bare shared or `own` parameters.
Arguments are moved or copied into task-owned capture storage before the child
runs, and a shared target borrows that capture. `mut` targets are
rejected.

## Calling Functions

Aura supports positional and named arguments:

```aura check-pass
def subtract(left: int32, right: int32) -> int32:
    return left - right

print(subtract(10, 3))
print(subtract(left=10, right=3))
print(subtract(10, right=3))
```

Function parameters remain positionally bindable. A `*` keyword-only marker
is not part of Aura 0.3's structural callable model and receives `AU1101`.

Rules:

- positional arguments come before named arguments
- named arguments match declared parameter names exactly
- a parameter cannot be provided more than once

## Default Parameter Values

Parameters can have defaults, which must come after required parameters:

```aura check-pass
def greet(name: str = "world"):
    print("hello " + name)

greet()               # "hello world"
greet(name="aura")  # "hello aura"
```

Default values are evaluated on each call, in parameter order. They cannot
reference other parameters, and are not allowed in trait or trait-impl method
declarations. Bare shared defaults are valid and the temporary lives through
the call; `own` defaults are consumed. `mut` defaults are
rejected because mutations to a caller-invisible temporary would be lost.

See [examples/basics/default_arguments.au](../examples/basics/default_arguments.au).

## Builtin Named Arguments

Some builtins also support named arguments:

```aura check-pass
for value in range(stop=3):
    print(value)

for value in range(start=3, stop=5):
    print(value)

print(value=42)
```

See [examples/basics/named_builtin_arguments.au](../examples/basics/named_builtin_arguments.au).

## What Functions Can Return

Functions may return any concrete type accepted in a return annotation,
including scalars, tuples, strings, collections, numeric arrays, classes,
enums, generic specializations, function values, `Result[T, E]`, `Option[T]`,
`Task[T]`, and `None`. An ordinary `-> T` result is owned. A separate
`-> view T from source` or `-> view mut T from source` contract returns a
non-owning view tied to one receiver or parameter.

Every ordinary return is an owned value. Returning a copy type produces an
ordinary independent copy:

```aura check-pass
class User:
    score: int32

def score(user: User) -> int32:
    return user.score
```

The call produces an ordinary `int32` copy. Methods use the same `-> T`
return annotation.

When several shared parameters have copy types, the function can select and
return any one of their values without a source label:

```aura check-pass
def choose_positive(left: int32, right: int32) -> int32:
    if left > 0:
        return left
    return right
```

Returning a non-copy value requires ownership. Clone from shared input when the
type is clone-safe, accept an `own` parameter and move from it, or provide an
owner operation such as an `own self` method. A shared parameter cannot expose
one of its non-copy fields as a return value.

An ordinary result never names an argument, field, or lifetime source.
Returning a Copy value copies it; returning a non-Copy value requires
constructing, cloning, or moving a value the function owns.

Use an explicit returned-view contract when the result must keep borrowing one
caller-owned place:

```aura check-pass
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

def main():
    user = User(name="Ada")
    view display = name(user)
    print(display)
    print(name(user))

    mut counter = Counter(value=0)
    bump(value_mut(counter))
    print(counter.value)
```

The origin after `from` is part of the function type. The caller must supply
an addressable place. Binding the result with matching `view` or `view mut`
syntax is one option. A shared result may also be read directly within one
containing expression, and a mutable result may be immediately reborrowed into
a `mut` call. A returned view cannot be stored as an ordinary owned value or
inside an aggregate.

## Generic Functions

Functions can be generic over type parameters:

```aura check-pass
def identity[T](value: own T) -> T:
    return value
```

The compiler infers type arguments from the arguments you pass and, when needed, from the expected return type. See [15-generics.md](15-generics.md) for the full story.

## Function Values

A module-level named function can be stored and passed like any other copy
value. Write its type in declaration-shaped form:

```aura check-pass
class Pipeline:
    transform: def(int32) -> int32

def double(value: int32) -> int32:
    return value * 2

def apply(transform: def(int32) -> int32, value: int32) -> int32:
    return transform(value)

selected = double
pipeline = Pipeline(transform=selected)
transforms: list[def(int32) -> int32] = [selected]

print(apply(pipeline.transform, 3))
print(transforms[0](4))
```

`def(T1, mut T2, own T3) -> R` contains parameter modes and types, but no
parameter names or default expressions. Bare parameters are shared. An
inferred binding such as `selected = consume` retains the exact contract, and
you can also write it explicitly:

```aura fragment
mutate: def(mut Counter) -> None = increment
consume: def(own str) -> str = take
callbacks: list[def(mut Counter) -> None] = [mutate]
```

Calling `mutate` requires a mutable place; calling `consume` moves a non-copy
argument. A function with either contract does not fit a bare shared
`def(T) -> R` annotation.
A function binding whose target declaration is statically known keeps that
declaration's names and defaults, so `selected(name="Aura")` and
`selected()` work when the original parameter is named `name` and has a
default. The structural function type itself retains neither, so a value
returned through a structural annotation requires the complete positional
argument list. A direct conditional selection keeps names and default
availability when all candidates agree, and an omitted argument runs the
selected function's own default. Class fields and mutable collections preserve
the full parameter types and `mut`/`own` capabilities, but deliberately erase
names and defaults; call a value loaded from either with the complete
positional list.

Function values are code pointers, so they are copy values and satisfy
`Transfer`. You can use one as the target of `TaskGroup.start(...)` or
`start_soon(...)`. Specialize a generic function explicitly
(`show_int = show[int32]`) or give it a concrete expected function type. The
expected type may come from an annotation, argument, field, collection
element, or function-typed parameter default.

Bound instance methods, associated-method values, and trait-method values are
unavailable. Task targets may be direct associated methods without `self`;
that task-target form does not create a general associated-method value.

See [examples/basics/function_values.au](../examples/basics/function_values.au).

## Expression Closures

Use a lambda when the callable is one expression and its parameter types are
already clear from context:

```aura check-pass
def main():
    offset: int32 = 40
    add: def(int32) -> int32 = lambda value: value + offset
    print(add(2))
```

The annotation supplies `value: int32` and the `int32` result. A lambda does
not put types or defaults in its own parameter list. Use `own value` or
`mut value` only when the expected function type has that same capability.
Multi-statement logic still belongs in a named `def`.

With no parameters, context is optional: `lambda: 42` can infer
`def() -> int64` from its body. A lambda with parameters still needs all of
their types from context.

Captures happen when the lambda is created. Copy values such as `offset` are
snapshotted. A non-Copy owned value moves into the closure:

```aura check-pass
def main():
    name = "Aura"
    length: def() -> int64 = lambda: name.len()
    print(length())
    print(length())
```

This is repeatable because the body only reads `name`. A body that returns or
otherwise consumes a non-Copy capture makes the closure single-use. Clone
before creation when the outer code must keep an independent owner.

Without a capture list, shared or mutable enclosing parameters are not
captured. An explicit exhaustive list may request `[value]`, `[mut value]`, or
`[own value]`. Mutable-loan closures are called through a `mut` local and write
through to their source. A closure can cross a task boundary only when every
capture is owned and Transfer; any loan capture makes it non-Transfer.

Capture-free lambdas work anywhere a function value works. A capturing
closure may stay in an immutable local, be called directly, enter a
compiler-known repeatable callback, or move into a qualifying task start. It
cannot be stored in a `def` field or collection or returned through an
annotated `def` result.

See [examples/basics/closures.au](../examples/basics/closures.au) and the
normative [Closures](../docs/manual/closures.md) page.

## Current Limits

- ordinary `-> T` return values are owned; `-> view [mut] T from origin` is the
  explicit non-owning exception
- clone-based non-copy returns require the returned type to be clone-safe
- method values and multi-statement closure bodies are not part of this stage
