# Ownership And Borrowing

If you are coming from Python, this is the most important chapter in the tutorial. Aura does not use a garbage collector. Instead, it tracks who owns each value and when that value can be freed. This system is called **ownership**, and the way you temporarily lend values without giving them away is called **borrowing**.

This chapter walks through the full model with practical examples, explains why the rules exist, and shows you how to fix every common compiler error you will encounter.

## Why Ownership?

In Python, every value lives on a heap and a garbage collector cleans up when nothing points to it anymore. This is simple, but it has costs: unpredictable pauses, higher memory use, and no deterministic cleanup.

Aura takes a different approach. Every value has exactly **one owner** at any point in time. When the owner goes out of scope, the value is freed immediately. No garbage collector, no reference counting, no surprises.

This gives you:

- **Predictable performance** -- no GC pauses
- **Deterministic cleanup** -- resources like files and connections close at a known point
- **Memory safety** -- the compiler rejects programs that would read freed or invalid memory

The trade-off is that you need to think about who owns what. The compiler enforces the rules and gives you clear error messages when something is wrong.

## Copy Types vs Move Types

Aura divides all types into two categories: **copy types** and **move types**. Understanding this distinction is the foundation of everything that follows.

### Copy types

Copy types are small, fixed-size values that are cheap to duplicate. When you assign a copy type to a new binding or pass it to a function, Aura silently makes a copy. Both the original and the new binding are fully independent.

The built-in copy types are:

- all integer types: `int` (the `int64` alias), `int8`, `int16`, `int32`, `int64`, `int128`, `intsize`
- all unsigned types: `uint8`, `uint16`, `uint32`, `uint64`, `uint128`, `uintsize`
- `float32`, `float64`
- `bool`
- `Duration`

Copy types behave the way Python developers expect:

```aura check-pass
x: int32 = 10
y = x          # copies the value
print(x)       # 10 -- still usable
print(y)       # 10 -- independent copy
```

There is no surprise here. You can use `x` and `y` freely because integers are copy types.

### Move types

Move types are values that own heap-allocated data or manage a unique resource. When you assign a move type to a new binding, Aura **moves** ownership. The original binding becomes invalid.

The built-in move types include:

- `str`
- `list[T]`, `dict[K, V]`, `set[T]`
- `random.Rng`
- `TaskGroup`
- user-defined classes (by default)

`Queue[T]` is a copy handle to shared runtime state. `Task[T]` is always safe
to transfer between tasks, but it is copyable only when its result can be
observed repeatedly: `T` must be copyable, a `Queue[...]` handle, or a
recursively repeatable `Task[...]` handle. A task returning `str`,
`list[...]`, or another non-copy owned value therefore has a move-only handle.
Copying an allowed handle never copies a queued value or task result.

Here is where Python intuition breaks down:

```aura check-pass
def main():
    name: str = "aura"
    other = name          # ownership moves to `other`
    print(other)          # "aura" -- works fine
```

If you try to use `name` after the move:

```aura check-fail:AU3001
def main():
    name: str = "aura"
    other = name
    print(other)
    print(name)           # COMPILE ERROR
```

The compiler rejects this with:

```

error: use of moved value `name`
```

**Why does this happen?** After `other = name`, the `other` binding owns the string data. If `name` were still valid, you would have two bindings pointing to the same heap memory. When both go out of scope, the memory would be freed twice -- a crash. Aura prevents this at compile time.

### The Python comparison

| Python | Aura |
|--------|--------|
| `y = x` always creates a reference, both point to the same object | `y = x` copies for copy types, moves for move types |
| Garbage collector handles cleanup | Owner handles cleanup when it goes out of scope |
| You never think about who owns what | You always know who owns what |

## Cloning: Explicit Copies Of Move Types

When a move type supports independent duplication, call `.clone()`:

```aura check-pass
name: str = "aura"
other = name.clone()   # explicit copy -- name stays valid
print(name)            # "aura"
print(other)           # "aura"
```

Collections expose `copy()`:

```aura check-pass
def main():
    mut xs: list[int32] = [1, 2, 3]
    ys = xs.copy()         # independent copy
    xs.append(4)
    print(xs.len())        # 4
    print(ys.len())        # 3 -- unaffected
```

Explicit duplication makes the allocation and element-copying cost visible.
Assignment continues to follow the ordinary copy-or-move rule.

Move types are not automatically cloneable. `random.Rng` exposes no clone
route, and a class, enum, or collection containing one cannot be cloned through
a public clone-producing operation. Generic clone helpers infer this
requirement and reject an unsafe concrete specialization with `AU3007`.

List and str slices are another explicit owned-copy boundary:

```aura check-pass
names = ["Ada", "Grace", "Margaret"]
selected = names[1:]       # fresh owned list[str]
label = "A🎉Z"[1:2]       # fresh owned str containing 🎉
print(names.len())         # the sources remain valid
```

A list slice copies Copy elements and clones non-Copy elements, so its element
type must be clone-safe. It rejects a value containing `random.Rng` with
`AU3007` and a non-repeatable Task result right with `AU3009`. A str slice
copies its Unicode-scalar range. Neither slice is a view: mutating the returned
List cannot mutate the source, and the slice cannot be an assignment target.

## Closures Capture By Value

A contextually typed lambda owns every outer local it uses:

```aura check-pass
def main():
    label = "compile"
    length: def() -> int64 = lambda: label.len()

    print(length())
    print(length())
```

`label` moves into the closure when the lambda expression is evaluated. Both
calls work because the body only reads its capture. If the body consumed a
non-copy capture, the call would consume the closure and a second call would
report `AU3001`.

Copy captures are snapshots and leave their sources usable. When outer code
also needs a non-copy value, clone before creating the closure:

```aura check-pass
def main():
    label = "compile"
    captured = label.clone()
    length: def() -> int64 = lambda: captured.len()

    print(label)
    print(length())
```

Without a capture list, bare and `mut` enclosing parameters are not captured.
Use an explicit exhaustive list for a live shared or mutable loan. A by-value
closure may cross a task boundary only when every captured value is Transfer;
a loan closure is always local and non-Transfer.

Stored and arbitrary parameter `def` types remain capture-free. Keep a
capturing closure in an immutable local, call it directly, pass it to a
compiler-known repeatable callback, or move a qualifying closure into one task
start; do not erase its environment metadata through a field, collection, or
annotated return.

## Local Views, Reborrowing, And Inferred Lifetimes

A view names a live place without taking ownership:

```aura check-pass
class Counter:
    value: int64

def main():
    mut counter = Counter(value=1)
    view mut value = counter.value
    view mut nested = value
    nested = nested + 1
    print(counter.value)
```

`nested` is a reborrow of `value`, and its assignment writes immediately to
`counter.value`. The compiler ends both loans after their final possible use,
so the later source read is legal even though the view bindings remain in
lexical scope. Shared views may overlap shared views; a mutable view excludes
all overlapping source access. Proven-disjoint fields and fixed tuple
positions can be loaned independently. Collection indexes are not view places
in Aura 0.3.

## Returned Views

A function can return access tied to one named receiver or parameter:

```aura check-pass
class User:
    name: str

def name(user: User) -> view str from user:
    return view user.name

def rename(user: mut User) -> view mut str from user:
    return view mut user.name

def main():
    mut user = User(name="Ada")
    view current = name(user)
    print(current)

    view mut editable = rename(user)
    editable = "Grace"
    print(user.name)
```

The `from` origin is part of the function contract. Mutable results require a
mutable origin and a mutable view binding. A local, temporary, owned/defaulted
parameter, or different root cannot escape as the result. Ordinary `-> T`
returns remain owned.

## Explicit Loan Captures

Capture lists are exhaustive and make live access visible:

```aura check-pass
class Counter:
    value: int64

    def add(mut self, amount: int64):
        self.value += amount

def main():
    mut counter = Counter(value=1)
    mut update: def(int64) -> None = lambda [mut counter] amount: counter.add(amount)
    update(2)
    update(3)
    print(counter.value)
```

`[counter]` is a shared loan, `[mut counter]` is a mutable loan, and `[own
counter]` is the original by-value Copy/move capture. A mutable-loan closure is
repeatable through a `mut` closure local. Its source remains exclusively
loaned until the closure's final use, and the closure cannot enter a task,
Queue, aggregate, or arbitrary structural `def` boundary.

Run the combined maintained example at
[examples/basics/views.au](../examples/basics/views.au).

## Passing Values To Functions

Bare function parameters grant logical shared access for every type. An
implementation may pass copy bits directly, but that does not change the
source-level contract. To transfer a move value to a function, write `own`:

```aura check-fail:AU3001
class Document:
    title: str
    pages: int32

def archive(doc: own Document):
    print(doc.title)

def main():
    doc = Document(title="Report", pages=42)
    archive(doc)
    print(doc.pages)       # COMPILE ERROR: use of moved value `doc`
```

The explicit `own` parameter took ownership of `doc`. After the call, `doc` is no longer valid in the calling scope. If the declaration were simply `doc: Document`, it would borrow and the caller could keep using it.

For copy types, shared access can be implemented by passing copied bits:

```aura check-pass
def double(x: int32) -> int32:
    return x * 2

value: int32 = 5
print(double(value))   # 10
print(value)           # 5 -- still valid, it was copied
```

## Borrowing: Lending Without Giving Away

Most of the time you want a function to read or modify a value without taking ownership. This is what **borrowing** does. A borrow is a temporary loan: the function can access the value, but the caller keeps ownership.

Aura has two kinds of borrows:

- `T` -- shared, read-only access
- `mut T` -- exclusive, mutable access

### Shared access with a bare type

A shared borrow lets a function read a value without consuming it:

```aura check-pass
class Counter:
    value: int32

def read(counter: Counter) -> int32:
    return counter.value

mut counter = Counter(value=41)
print(read(counter))       # 41
print(counter.value)       # 41 -- counter still belongs to us
```

The bare `counter: Counter` declaration is the shared contract: this function
is looking, not taking. After the call returns, the borrow ends and the caller
still owns the value.

You can have multiple shared borrows active at the same time because none of them can modify the value:

```aura fragment
def sum_values(a: Counter, b: Counter) -> int32:
    return a.value + b.value

c1 = Counter(value=10)
c2 = Counter(value=20)
print(sum_values(c1, c2))   # 30 -- both still valid
```

### Mutable borrows with `mut T`

A mutable borrow lets a function modify the value in place:

```aura fragment
def bump(counter: mut Counter):
    counter.value += 1

mut counter = Counter(value=41)
bump(counter)
print(counter.value)       # 42 -- the change persisted
```

The caller must declare the binding as `mut` because the function will modify it. If the binding is not mutable, the compiler rejects the call:

```aura fragment
counter = Counter(value=41)  # not mutable
bump(counter)                # COMPILE ERROR
```

```

error: argument for parameter `counter` in function `bump` must be a mutable place
```

### The exclusivity rule

You cannot have mutable access and another overlapping access to the same
value at the same time. This prevents data races and aliasing bugs:

```aura fragment
def bad(a: mut Counter, b: Counter):
    a.value += b.value

mut c = Counter(value=1)
bad(c, c)    # COMPILE ERROR: overlapping access
```

**Why does this rule exist?** Imagine `bad` increments `a.value` while reading `b.value` -- but `a` and `b` are the same object. The final result would depend on the order of operations inside the function, creating a subtle bug. Aura prevents this entirely.

Think of it like a library book: many people can read it at the same time (shared borrows), or one person can take it home to annotate it (mutable borrow), but you cannot do both at once.

## Method Receivers

Methods on classes use the same borrowing system through **receivers**. The receiver determines what the method can do with the instance:

### `self` -- read the instance

```aura check-pass
class Account:
    balance: float64

    def display(self) -> str:
        return f"Balance: {self.balance}"
```

Bare `self` is shared access. The method can read fields but cannot modify
them, and the caller retains ownership.

```aura fragment
account = Account(balance=100.0)
print(account.display())    # "Balance: 100.0"
print(account.balance)      # still accessible
```

### `mut self` -- modify the instance

```aura check-pass
class Account:
    balance: float64

    def deposit(mut self, amount: float64):
        self.balance += amount

    def display(self) -> str:
        return f"Balance: {self.balance}"
```

The method can read and write fields. The instance must be declared `mut`:

```aura fragment
mut account = Account(balance=100.0)
account.deposit(50.0)
print(account.display())    # "Balance: 150.0"
```

If you forget `mut`:

```aura fragment
account = Account(balance=100.0)
account.deposit(50.0)       # COMPILE ERROR: must be a mutable place
```

### `own self` -- consume the instance

```aura check-pass
class Connection:
    host: str

    def into_host(own self) -> str:
        return self.host
```

An `own self` receiver takes ownership. A non-copy instance is consumed after the call:

```aura fragment
conn = Connection(host="example.com")
host = conn.into_host()
print(host)               # "example.com"
print(conn.host)          # COMPILE ERROR: use of moved value `conn`
```

Use `own self` when the method needs to disassemble the instance or transfer ownership of its fields.

### No receiver -- associated methods

Methods without a receiver are called on the class itself, not on an instance:

```aura check-pass
class Counter:
    value: int32

    def zero() -> Counter:
        return Counter(value=0)
```

```aura fragment
c = Counter.zero()
```

### Choosing the right receiver

| Receiver | When to use | Example |
|----------|-------------|---------|
| `self` | Read-only shared access, the default | getters, display, serialization |
| `mut self` | Modify the instance in place | setters, increment, append |
| `own self` | Consume the instance to extract data | `into_*` conversions, one-shot use |
| no receiver | Factory methods and utilities that do not need an instance | `Counter.zero()` |

If you are not sure, start with bare `self`. Add `own` only when the method
must consume the instance, or `mut` when it must mutate in place.

## Field Access And Move Semantics

When you own a value, reading a non-copy field **moves** that field out of the instance:

```aura check-fail:AU3001
class User:
    name: str
    age: int32

def main():
    user = User(name="Ada", age=36)
    greeting = user.name     # moves `name` out of `user`
    print(greeting)          # "Ada"
    print(user.age)          # 36 -- copy field, still fine
    print(user.name)         # COMPILE ERROR: use of moved field `name` from `user`
```

```

error: use of moved field `name` from `user`
```

**Why?** The `str` in `user.name` is a move type. Reading it transfers ownership to `greeting`. The `user` instance no longer has a valid `name` field. The `age` field is `int32` (a copy type), so it is unaffected.

### Reading fields from borrowed values

When you borrow a value, you cannot move non-copy fields out of it because you do not own it:

```aura fragment
def get_name(user: User) -> str:
    return user.name       # COMPILE ERROR
```

```

error: cannot move non-copy field `name` out of borrowed value `user`
```

The function only borrowed `user` -- it has no right to take the `name` away. The fix depends on what you need:

**Option 1: clone the field**

```aura fragment
def get_name(user: User) -> str:
    return user.name.clone()   # explicit copy, user keeps its name
```

**Option 2: take ownership of the whole value**

```aura fragment
def get_name(user: own User) -> str:
    return user.name           # consumes user, moves name out
```

**Option 3: return a copy-type field instead**

```aura fragment
def get_age(user: User) -> int32:
    return user.age            # int32 is copy, no move needed
```

## Copy Classes

By default, user-defined classes are move types. You can make a class copyable with `copy class`, but only if every field is itself a copy type:

```aura check-pass
copy class Point:
    x: int32
    y: int32

p1 = Point(x=1, y=2)
p2 = p1               # copies, both valid
print(p1.x)           # 1
print(p2.x)           # 1
```

If any field is a move type, the compiler rejects the `copy` annotation:

```aura check-fail:AU2002
copy class Bad:
    name: str       # COMPILE ERROR
    value: int32
```

```

error: field `name` on `copy class Bad` must be a copy type, found `str`
```

**When to use `copy class`:** Use it for small, value-like types where copying is cheap and expected -- coordinates, colors, dimensions, ranges. Do not use it for types that hold resources or large data.

## Borrowing In Loops

Loops use the same readable default. Bare `list` and `set` iteration borrows the
collection, so it remains usable:

```aura check-pass
mut names: list[str] = ["Ada", "Grace", "Margaret"]
for name in names:
    print(name)
print(names.len())     # 3 -- still usable
```

Write `own` when you intend to move each element out and consume the list:

```aura check-pass
def main():
    names: list[str] = ["Ada", "Grace", "Margaret"]
    for name in own names:
        print(name)
    # names is moved
```

**Note:** Even `list[int32]` is itself a move type, but its bare loop still
borrows. Only `own` consumes it:

```aura check-pass
mut xs: list[int32] = [1, 2, 3]
for x in xs:
    print(x)
for x in own xs:
    print(x)
# another use of xs would now be an error
```

### Bare shared iteration

Bare iteration is the shared form:

```aura check-pass
mut names: list[str] = ["Ada", "Grace", "Margaret"]
for name in names:
    print(name)
print(names.len())     # 3 -- names is still valid

for name in names:   # can iterate again
    print(name)
```

For copy element types, the loop variable receives a copy of each element. For non-copy element types, the loop variable is a temporary borrow.

### Mutable borrow iteration with `mut`

To modify elements during iteration, use `for ... in mut`:

```aura check-pass
class Score:
    value: int32

    def double(mut self):
        self.value = self.value * 2

mut scores: list[Score] = [Score(value=1), Score(value=2), Score(value=3)]
for score in mut scores:
    score.double()

for score in scores:
    print(score.value)
# prints: 2, 4, 6
```

This requires the collection binding to be `mut`.

### Which iteration form to use

| Form | Effect | Use when |
|------|--------|----------|
| `for x in collection` | Shared borrow, collection stays valid | Ordinary read-only iteration |
| `for x in own collection` | Consumes the collection | You are done with the collection after the loop |
| `for x in mut collection` | Mutable borrow, can modify elements | You want to update elements in place |

**Default recommendation:** Use bare `for x in collection` for reads, `own` to
consume, and `mut` to update.

### Comprehensions use the bare form

A comprehension is the eager expression counterpart of nested bare loops:

```aura check-pass
names = ["Ada", "Grace"]
lengths = [name.len() for name in names]
copies = [name.clone() for name in names]
```

The result collection is newly owned, while a list or set clause shares and
freezes its source. `name.len()` only reads the shared `str`. Storing the
non-copy `str` itself requires the explicit `.clone()` shown in `copies`;
the compiler never inserts that clone.

Comprehension clauses have no `mut` or `own` modifier. Use a statement loop for
mutable or consuming collection traversal. Queue preserves its bare-loop
exception: each received item arrives owned and may move directly into the
eager result. Every target disappears after the closing delimiter.

## Borrowing In Match

Pattern matching follows the same ownership rules. Bare `match` shares the
value, so the caller keeps ownership:

```aura check-pass
result: Result[str, str] = Result.Ok("success")
match result:
    case Ok(msg):
        print(msg)
    case Err(e):
        print(e)
print(result)          # still valid
```

To consume the value and receive owned payloads, use `match own`:

```aura check-pass
def main():
    result: Result[str, str] = Result.Ok("success")
    match own result:
        case Ok(msg):
            print(msg)     # msg is owned
        case Err(e):
            print(e)
    # result is moved
```

To match and mutate the payload, use `match mut`:

```aura check-pass
mut result: Result[str, str] = Result.Ok("hello")
match mut result:
    case Ok(msg):
        # msg is mut str -- can call mutating methods
        pass
    case Err(e):
        pass
```

## Borrowing And Concurrency

Queues transfer ownership of sent values. When you put a value into a queue, it moves:

```aura check-pass
jobs = Queue[str]()
jobs.put("hello")      # "hello" moves into the queue
# the sent string is now owned by whichever task receives it
```

Queue construction and sending require the payload type to satisfy Aura's
compiler-derived `Transfer` rule. Copy values, `str`, and aggregates whose
stored components are all `Transfer` may cross. `random.Rng`, `TaskGroup`,
shared or mutable access, and live file, process, or network resources may
not. Keep a live resource on the task that owns it and exchange owned
descriptions, bytes, snapshot results, or queue/task handles instead.

Queue handles are cheap copy references. Passing a queue to
`TaskGroup.start(...)` shares the same underlying queue; you do not need
`.clone()` for the common case:

```aura check-pass
def send_message(jobs: Queue[str]):
    jobs.put("from task")
    jobs.close()

jobs = Queue[str]()
with TaskGroup() as group:
    task = group.start(send_message, jobs)
    match jobs.get():
        case QueueReceive.Item(value):
            print(value)   # "from task"
        case QueueReceive.Closed:
            pass
        case QueueReceive.TimedOut:
            pass
        case QueueReceive.Cancelled:
            pass
    task.result()
```

Every task argument and result must also be structurally `Transfer`. This rule
is checked after generic specialization. A task target may borrow from its
task-owned capture through a bare parameter, but the captured value itself
crosses by ownership.

Task result observation has a separate repeatability rule. A copy result, a
`Queue[...]` result, or a recursively repeatable `Task[...]` result may be
observed repeatedly. For any other transferable result,
`result()`, `result_or_none()`, and `result_or()` consume the task handle on
the first attempt, even if that attempt times out, is cancelled, fails, or
returns a fallback. `wait_any` and `wait_all` consume the complete task list
for such results; `wait_any` deliberately abandons the unchosen observation
rights.

## Common Patterns And Fixes

### Pattern: "I need to use a value after passing it to a function"

**Problem:**
```aura fragment
def archive(doc: own Document):
    print(doc.title)

doc = Document(title="Report", pages=10)
archive(doc)
print(doc.title)       # COMPILE ERROR: use of moved value
```

**Fix 1 -- remove `own` to use the bare shared-borrow default:**
```aura fragment
def archive(doc: Document):
    print(doc.title)
```

The bare `doc: Document` declaration is the shared spelling.

**Fix 2 -- keep the owned parameter and clone before passing:**
```aura fragment
archive(doc.clone())
print(doc.title)       # doc still valid
```

### Pattern: "I need to read a str field without consuming the owner"

**Problem:**
```aura fragment
def get_title(doc: Document) -> str:
    return doc.title   # COMPILE ERROR: cannot move out of shared access
```

**Fix -- clone the field:**
```aura fragment
def get_title(doc: Document) -> str:
    return doc.title.clone()
```

### Pattern: "I need to consume collection elements"

**Problem:**
```aura fragment
for item in items:
    inspect(item)
print(items.len())     # still available
```

**Use `own` when the consumer needs owned items:**
```aura fragment
for item in own items:
    process(item)
# items is now moved
```

### Pattern: "I need to modify elements in a collection"

**Problem:**
```aura fragment
for score in scores:
    score.double()     # COMPILE ERROR: not mutable
```

**Fix -- mutable borrow iterate:**
```aura fragment
for score in mut scores:
    score.double()
```

### Pattern: "The compiler says my binding must be mutable"

**Problem:**
```aura fragment
counter = Counter(value=0)
counter.bump()         # COMPILE ERROR: must be a mutable place
```

**Fix -- declare with `mut`:**
```aura fragment
mut counter = Counter(value=0)
counter.bump()
```

## Mental Model For Python Developers

Here is how to translate your Python intuition:

| Python concept | Aura equivalent |
|----------------|-------------------|
| `x = y` (always a reference) | `x = y` copies if copy type, moves if move type |
| `x = copy.deepcopy(y)` | `x = y.copy()` for collections; `x = y.clone()` for other clone-safe move types that expose it |
| `def f(x): ...` reads x | `def f(x: T): ...` for shared access |
| `def f(x): x.mutate()` | `def f(x: mut T): ...` |
| `del x` (deferred to GC) | Automatic when owner goes out of scope |
| `for x in list: ...` (list survives) | `for x in list: ...` (shared; list survives) |
| No direct equivalent | `for x in own list: ...` (list consumed) |

The key shift is: in Python, assignment creates aliases. In Aura, assignment transfers ownership. Once you internalize this, the rest of the system follows naturally.

## Summary

1. Every value has one owner. When the owner goes out of scope, the value is freed.
2. Copy types (numbers, `bool`, `Duration`) are duplicated on assignment. Move types (`str`, `list`, `random.Rng`, classes) transfer ownership.
3. Use collection `.copy()` or the `.clone()` method exposed by another
   clone-safe move type when you need an independent owned value;
   `random.Rng` and values containing it support neither operation.
4. Bare parameters grant logical shared access for every type. Use `mut T` to
   lend mutable access and `own T` to transfer ownership.
5. `mut` access is exclusive -- no other overlapping access can exist at the
   same time.
6. Method receivers follow the same rules: `self` reads, `mut self` modifies,
   and `own self` consumes.
7. Bare collection iteration is shared. Use `for x in own collection` to consume and `for x in mut collection` to modify elements.
8. Use `match value` to pattern-match without consuming.
9. Queues transfer ownership of sent values and admit only structurally
   `Transfer` payloads. Queue handles are copy values.
10. Task captures and results must be structurally `Transfer`. A `Task[T]`
    handle is copyable only for a repeatable `T`; otherwise the first result
    attempt consumes its unique observation right.

The compiler enforces all of these rules. When you see an error about moved values or borrowing, come back to this chapter -- the fix is almost always one of the patterns listed above.
