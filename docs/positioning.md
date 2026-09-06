# Why Aura

Aura 0.3.3 is a technical preview of a compiled, statically typed programming
language for reliable software. It combines Python-inspired syntax,
deterministic ownership, structured concurrency, typed failure, and native
executables.

The current language focuses on agents, ML infrastructure, evaluation workers,
network services, and the control-plane software around models. These
workloads benefit from readable source code, explicit resource lifetimes,
structured tasks, and failures represented directly in the type system.

Three commitments shape the language:

- **Deterministic ownership.** Bare access is shared, `mut` is exclusive
  mutation, `own` transfers a value, and the owning scope defines cleanup.
- **Structured concurrency.** A `TaskGroup` owns every child started in its
  scope, and scope exit accounts for all of them.
- **Typed failure.** Files, subprocesses, sockets, HTTP, retries, and
  supervisors surface recoverable failure through `Result`, `Option`, and
  focused outcome enums.

Ownership governs access, transfer, and cleanup — not scheduling. Concurrent
completion, cross-worker scheduling, and output order stay unspecified. The
[Ownership](/manual/ownership-and-borrowing),
[Concurrency](/manual/concurrency), and
[Control-Plane Modules](/manual/control-plane) chapters define the exact
contracts.

## Built For Agents And ML Infrastructure

Modern ML products extend far beyond model code. They include inference
gateways, queue workers, evaluation pipelines, tool executors, subprocess
supervisors, network clients, storage paths, timeouts, and retries. Agent
runtimes add long-lived task trees and repeated interaction with unreliable
external systems.

Aura gives this work one coherent contract. Static types describe the data.
Ownership describes resource lifetime. Structured concurrency accounts for
child tasks. Typed outcomes keep operational failure visible. Native
compilation produces deployable executables with no garbage collector.

The result is a focused language for the reliable control plane around models:

- model-serving and inference coordination;
- agent runtimes and tool execution;
- concurrent data and evaluation workers;
- process, queue, and network supervision; and
- infrastructure where cleanup and failure handling are correctness
  requirements.

## Long-Term Direction

Aura's long-term goal is to become a general-purpose systems language capable
of building every type of software. The intended scope spans applications,
services, databases, language runtimes, embedded software, operating systems,
and device drivers.

Aura 0.3 establishes the foundation through static typing, deterministic
ownership, native compilation, structured concurrency, typed failure,
packages, and integrated tooling. Later releases will extend that foundation
with freestanding compilation, low-level memory access, hardware interfaces,
portable layout controls, cross-compilation, and specialized runtime profiles.

## Familiar Source, Strong Guarantees

Python demonstrated the value of readable, low-friction source code. Rust
demonstrated that ownership can prevent broad classes of memory and concurrency
errors before execution. Aura combines those lessons in an indentation-based
language with a smaller control-plane focus.

The familiar surface lowers the cost of reading and writing compiled software.
The compiler requires exact types at public boundaries, validates ownership
and task transfer, checks exhaustive matches, and carries source context into
runtime diagnostics. Familiar syntax preserves the complete language contract.

## Adjacent Languages

These projects overlap with parts of Aura's motivation. The distinctions below
describe focus and language contracts. Primary sources were checked on 31 July
2026.

### Mojo

Mojo is a close neighbor in Python-shaped compiled syntax and compiler-tracked
ownership. Its roadmap centers
[high-performance kernels on CPUs, GPUs, and ASICs, with Python interoperability](https://mojolang.org/docs/roadmap/).
Its ownership documentation gives each value one owner and defines
[default immutable, `mut`, and `var` argument conventions](https://mojolang.org/docs/manual/values/ownership/).

Aura 0.3 centers the application control plane around models and agents:
scoped child tasks, transferable messages, typed I/O and process failures,
timeouts, retries, and supervision. GPU programming, heterogeneous hardware,
and Python-library interoperability remain future surface areas.

### Nim

Nim is a broad, established systems language. The Nim project describes it as
a [statically typed compiled language combining ideas from Python, Ada, and
Modula](https://nim-lang.org/), with native executables and deterministic,
customizable memory management. Its documentation recommends
[ORC for newly written code](https://nim-lang.org/2.2.6/mm.html), and its
[typed-threads documentation](https://nim-lang.org/docs/typedthreads.html)
covers shared-heap and explicit thread facilities.

Aura's distinction is its smaller integrated contract around call-boundary
capabilities, structurally transferable task values, `TaskGroup` scope, and
typed control-plane APIs. Nim provides greater metaprogramming, backend,
ecosystem, and portability breadth today.

### Go

Go is a production reference point for simple concurrent service software. Its
documentation defines lightweight
[goroutines and channel communication](https://go.dev/doc/effective_go#concurrency),
treats [errors as values](https://go.dev/blog/errors-are-values), and explains
that the standard toolchain ships a
[tracing garbage collector](https://go.dev/doc/gc-guide).

Aura uses a different lifetime contract. Non-copy task captures and messages
must satisfy structural `Transfer`, resources have owners, and a `TaskGroup`
accounts for the children it starts. This provides scoped task lifetime and
deterministic resource cleanup without a garbage collector.

### Free-threaded Python 3.13+

CPython has supported an optional free-threaded build since Python 3.13. The
official guide says that this build can run threads in parallel with the GIL
disabled, while some extension modules may
[re-enable the GIL](https://docs.python.org/3/howto/free-threading-python.html).
The language retains its shared-object, dynamically typed programming model.

Aura checks ownership and task-transfer boundaries before execution and gives
common control-plane failures concrete result types. Python offers much greater
runtime flexibility and ecosystem compatibility. Aura offers a compiled,
ownership-based contract for teams that want those decisions checked.

## Performance And Technical Scope

The [Performance](/manual/performance) chapter records current measurements,
known gaps, reproduction evidence, and the optimization direction for later
releases.

Aura 0.3 is an executable technical preview. Its current surface includes the
language, native compiler, ownership model, structured task runtime, numeric
arrays, control-plane modules, package tooling, Manual, and editor extension.
The [Current Limits](/manual/current-limits) chapter lists the precise
boundaries of that surface.
