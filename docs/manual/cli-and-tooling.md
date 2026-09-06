# CLI And Tooling

The `aura` CLI is the product surface for checking, running, building, inspecting, and editor integration.

During repository development, commands are usually run through Cargo:

```bash
cargo run -p aura -- check examples/classes/point_distance.au
```

In an installed environment, the command shape is the same without the Cargo prefix:

```bash
aura check app.au
```

## Commands

| Command | Purpose |
| --- | --- |
| `aura check file.au` | Parse and type-check without executing. |
| `aura run file.au` | Execute through the MIR runtime, which is the default backend. |
| `aura run --backend mir\|direct\|auto file.au` | Choose the execution backend explicitly. |
| `aura run file.au -- args...` | Execute with program arguments available through `sys.args()`. |
| `aura build -o path file.au` | Build a native binary. |
| `aura ast file.au` | Print the syntax tree. |
| `aura ast-json file.au` | Print syntax tree JSON. |
| `aura mir file.au` | Print lowered MIR. |
| `aura analyze file.au` | Emit diagnostics, symbols, hover data, and definition data. |
| `aura complete --line N --character C --trigger . file.au` | Emit completion items. |
| `aura deps update [name]` | Refresh all git dependencies or one named dependency. |
| `aura new path` | Create `Aura.toml` and `src/main.au` without overwriting an existing path. |
| `aura fmt [--check] [paths...]` | Normalize Aura source whitespace or verify formatting. |
| `aura test [-k substring] [--format json] [--timeout-ms N] [paths...]` | Discover package-aware `.au` tests, select canonical case names, and report one result per case; defaults to `tests/` and a 30-second per-case timeout. |
| `aura upgrade` | Download and run the verified installer to replace the compiler and bundled runtime with the latest published release. |
| `aura lsp` | Run the persistent JSON-lines compiler service used by the language server. |
| `aura help` / `aura --help` | Print usage. |
| `aura version` / `aura --version` | Print the build channel and 12-hex-digit source commit. Release archives print `aura 0.3.3-preview (0123456789ab)`; source builds print `aura 0.3.3-dev (0123456789ab)`. |

## Checking

`check` is the fastest way to validate syntax, types, imports, ownership, and package resolution:

```bash
cargo run -p aura -- check examples/collections/list_basics.au
```

Use `check` before `run` when you are editing a package or diagnosing type errors.

## Running

`run` executes a source file through the MIR runtime:

```bash
cargo run -p aura -- run examples/control_flow/while_break_continue.au
```

Runtime diagnostics include source context where possible.

Task execution uses the available parallelism reported by the host by default.
The provisional
`AURA_WORKERS` environment override accepts a positive integer, including a
count larger than the host's available-core count. For example,
`AURA_WORKERS=4 aura run app.au` selects four pinned task workers.
`AURA_WORKERS=1` preserves single-worker cooperative execution through the
same pinned-worker architecture.

MIR runs, forced-direct runs, and standalone direct binaries use the same
override. Empty, zero, signed, whitespace-padded, nonnumeric, and overflowing
values stop execution with `AU4006` and identify the raw invalid value.
Checking, analysis, completion, and formatting do not start the task runtime.

Blocking host operations use a separate process-wide pool. Its operational
settings are:

- `AURA_BLOCKING_WORKERS=<positive integer>` requests that exact blocking
  worker count without clamping. When absent, the runtime uses available host
  parallelism, falls back to `4`, and clamps that derived default to `2..=8`.
- `AURA_BLOCKING_QUEUE_CAPACITY=<positive integer>` bounds accepted jobs
  waiting in the pool's FIFO queue. Running jobs and callers waiting for
  admission do not consume this capacity. When absent, the pending queue is
  unbounded.

MIR execution, direct execution, and launched standalone native binaries
validate both settings before any user code runs. Empty, zero, signed,
whitespace-padded, non-decimal, and overflowing values stop execution with
`AU4006`, naming the setting and rendering the supplied value; a non-Unicode
value is displayed lossily. The first runtime preflight reads both settings
once, and the resulting configuration is immutable for the process lifetime.
Valid preflight creates no blocking worker threads. First submission creates
the complete configured set, which production reuses until process exit
without an Aura shutdown/join surface. A full bounded queue parks a
lightweight task through the scheduler; timeout or cancellation before queue
insertion prevents submission. Once inserted, host work cannot be retracted
and any late result is discarded. A bound limits accepted pending backlog, not
running work or admission waiters, so it cannot guarantee progress for
unrelated blocking I/O while all workers are stuck.

## Building

```bash
cargo run -p aura -- build --backend auto -o ./target/app app.au
cargo run -p aura -- build --backend direct -o ./target/app app.au
```

`auto` is the default. It first attempts the maintained direct backend and may fall back to a native launcher that embeds serialized MIR and the MIR runtime when direct emission is unavailable. `--backend direct` forbids that fallback. Both forms are standalone executables and must implement the same checked language behavior.

An installed release archive resolves its native runtime relative to `bin/aura`, under `lib/aura`, and needs only a host C compiler for the final link. A source-checkout binary falls back to Cargo-built runtime artifacts for contributor convenience.

## Stdin Buffers

Editor-style commands can read from stdin while using a supplied path for package roots and local imports:

```bash
cat examples/modules/simple_import.au | \
  cargo run -p aura -- analyze --stdin "$(pwd)/examples/modules/simple_import.au"
```

Stdin analysis and completion do not mutate package lockfiles.

## Analyze

`analyze` emits machine-readable data for editor tooling:

- diagnostics
- symbols
- hover information
- definition targets

The output is one JSON object with `diagnostics`, `symbols`, and `occurrences`
arrays. Positions are zero-based. Diagnostics contain `code`, `line`,
`start_character`, `end_character`, `message`, numeric `severity`,
`secondary_spans`, `notes`, `help`, `edits`, and always-present `call_frames`
and `task_ancestry` arrays. Analysis frame spans use zero-based coordinates and
an optional `file_path`; symbols contain `name`, `kind`, `detail`, and
recursive `children`; occurrences contain `hover` and an optional `definition`
range, whose `file_path` may identify another module. An edit includes its
range, replacement text, and applicability.

`analyze` exits successfully even when the JSON contains source diagnostics: the request itself succeeded and the diagnostics are data. The language server prefers this compiler-backed analysis when it succeeds.

## Complete

`complete` emits completion items at a zero-based line and character position:

```bash
cargo run -p aura -- complete --line 12 --character 8 --trigger . app.au
```

Completion output is intended for tools, not humans, but it is useful when debugging the LSP.

The JSON result is an array of `{ "name": str, "kind": str, "detail": str }` objects. `line` and `character` are zero-based and `--trigger` uses its first character.

## Machine-Readable And Inspection Formats

`ast-json`, `analyze`, `complete`, and `lsp` emit JSON. The `analyze` and `complete` shapes described here are maintained tooling contracts for Aura 0.3. `ast`, `ast-json`, and `mir` expose compiler inspection data for people and tests; their exact formatting and internal node/block shape are not a stable cross-version serialization API.

`aura lsp` is a persistent JSON-lines compiler service. Every request requires
`semantic_interface_version: 6` plus string fields `method`, `path`, and
`source`; `id` is optional and is echoed in the response. Supported requests
are:

```json
{"id":1,"semantic_interface_version":6,"method":"analyze","path":"/absolute/app.au","source":"print(1)\n"}
{"id":2,"semantic_interface_version":6,"method":"complete","path":"/absolute/app.au","source":"value.\n","line":0,"character":6,"trigger":"."}
```

Each response is one line containing the same `id`,
`semantic_interface_version: 6`, and either `result` or an `error` string.
Paths give the virtual source a package/import context; ranges and completion
positions are zero-based. A missing or different semantic interface version is
an incompatible request and returns a schema-mismatch error.

## Output And Exit Status

| Outcome | Exit status and streams |
| --- | --- |
| help/version | `0`; result on stdout |
| malformed command usage | `2`; usage on stderr |
| `check` success | `0`; exactly `ok` plus a newline on stdout |
| compile, build, or runtime failure | `1`; rendered diagnostic on stderr |
| `run` with `main() -> None` | `0` |
| `run` with `main() -> int32` | the returned integer requested as the host process status |
| successful `analyze` containing source diagnostics | `0`; JSON on stdout |
| completed human `test` run | `0` when every selected case passes; `1` otherwise; case output and summary on stdout, failures on stderr |
| completed JSON `test` run | `0` when every selected case passes; `1` otherwise; exactly one schema-version-1 document on stdout and no human progress lines |

A broken stdout pipe is intentional clean termination and exits `0`; this lets commands compose with consumers such as `head` without printing a secondary failure.

## Testing

With no paths, `aura test` recursively reads `.au` files under `tests/`. Given
files or directories, it uses those inputs. Files are visited in normalized
path order. Within a file, module functions are discovered in declaration
order. A parameterless `test_*` function returning `None` is one case. Its
canonical name is `path::test_name`. A module `test_*` function with parameters
is an invalid test declaration. Class, trait, and implementation methods are
not discovered. If a file declares no module `test_*` function, the file
remains one case named `path`, entered through `main()` or its top-level
statements.

`-k substring` performs a literal, case-sensitive substring match over the
complete canonical case name. Selection happens after parameter registrations
are expanded, so the substring may select a bracketed case label. `-k` may
appear once and its value must be non-empty. A valid filter that selects no
cases succeeds with `0 passed; 0 failed`. Missing, empty, or repeated filter
values are usage errors and exit with status 2.

### Setup And Teardown

A file may declare ordinary parameterless `setup()` and `teardown()` module
functions returning `None`. For each selected case, the runner invokes setup,
then the case only when setup succeeds, then teardown. Teardown runs after an
attempted setup even when setup traps, and after a case trap or non-zero
file-level `main()` result. It is not run during discovery. A hook with
parameters, a non-`None` result, or a collision with a non-function declaration
is a check-time test failure.

Setup, case, and teardown are isolated entries into one already checked and
lowered module. The runner does not re-read or re-check the source between
phases. Aura values and module-runtime state do not pass between phases or
cases; external effects such as file writes remain observable. The first
failure is primary. When teardown also fails, human output prints
`teardown also failed for ...` after the primary failure, while JSON stores the
teardown failure in the case record's `secondary` object with
`stage: "teardown"`. If setup succeeds and only teardown fails, the teardown
failure is primary. The timeout covers the complete lifecycle. A timed-out
worker cannot be forcibly stopped, so teardown is not promised after timeout.

### Parameterized Registration

A parameterized `test_*` function is parameterless and returns
`list[(str, def() -> None)]`. The runner invokes it once during discovery and
expands its list in order. Each tuple contains a non-empty label and a named,
capture-free, repeatable, parameterless function returning `None`. Labels must
be unique within that registration. The canonical case name is
`path::test_name[label]`.

Registration finishes before filtering, and it never executes a returned case.
The required `def() -> None` element type excludes capturing closures and keeps
every expanded case independently invocable. A registration trap, timeout,
invalid returned value, empty label, duplicate label, or invalid case function
is one discovery failure for that registration; none of its cases run. An
empty registration contributes no cases. Registration itself never invokes
setup or teardown.

Registration stdout is captured once. Human mode writes all registration
stdout before case results. JSON mode records non-empty registration stdout in
the top-level `discovery` array, whose entries contain `name`, `file`, and
`stdout` in registration order.

### Test Output Contract

Human mode writes each case's captured stdout, then `ok <canonical-name>` for a
passing case. A failed case writes `FAILED <canonical-name>` and its ordinary
source diagnostic, or a runner reason such as a timeout, to stderr. Standard
output ends with `<passed> passed; <failed> failed`. Test records and the
summary retain canonical discovery order.

`aura test --format json` writes exactly one JSON document to stdout and no
human progress lines. The top-level object has integer `schema_version: 1`, a
`summary` object with integer `selected`, `passed`, and `failed` counts, and an
ordered `tests` array. Every test record contains `name`, `file`, `outcome`
(`passed` or `failed`), and a non-negative integer `duration_ms` covering its
complete lifecycle. Non-empty captured output appears as `stdout`.

A trapped test record contains `diagnostic`, using the existing structured
diagnostic schema including optional assertion operand records. A runner
failure contains `reason`; a failed record has exactly one primary failure
form. A second teardown failure appears as `secondary`, with `stage` plus
either `diagnostic` or `reason`. Invalid command usage still goes to stderr and
exits 2. Assertions execute normally in every mode; `aura test` has no
assertion-stripping option.

## VS Code And LSP

The VS Code extension keeps one persistent `aura lsp` process for diagnostics, symbols, hover, go-to-definition, and completions. Requests are debounced, cancellable, version-guarded, and invalidated by dependency. If the compiler process cannot start, a small lexical recovery layer provides declarations and top-level completion; it intentionally does not duplicate compiler semantics.

Compiler-backed method hover and completion details include the receiver
contract. They render shared receivers canonically as `self`, consuming
receivers as `own self`, and mutable receivers as `mut self`.

Ordinary parameter signatures preserve bare, `own`, and `mut` spelling, and
built-in hover/completion detail exposes retained-value contracts such as
`list.append(value: own T)`. Class field and enum payload completion detail also
renders their implicit constructor ownership as `own`.

Useful repo commands:

```bash
npm run check:lsp
npm run test:lsp
npm run check:extension
npm run test:extension
```

## Documentation Site

The VitePress book is served with:

```bash
npm run docs:dev
```

Build it with:

```bash
npm run docs:build
```

Validate the normative reference structure and navigation with:

```bash
npm run check:reference
```

GitHub Pages builds use the same command with `VITEPRESS_BASE=/Aura/` so project-page asset URLs are rooted correctly.

## Repository Gates

The local repo gate is:

```bash
npm run ci
```

That gate checks Rust formatting, Rust tests, native/MIR parity, LSP tests and coverage, VS Code extension tests, compiler coverage, reference integrity, docs build, npm and RustSec audits, Clippy with warnings treated as errors, and repository hygiene.

GitHub Actions runs the repo gate on Linux and macOS. The release workflow publishes `v*` tag builds as GitHub Release assets, including platform CLI archives, the packaged VS Code extension, and a static docs archive.

## Grammar

The command line is a tooling protocol, not part of Aura source grammar. Its maintained invocation forms are the command forms in the table above and the usage text printed by `aura help`. The single-source compiler commands use either one `.au` path or their documented `--stdin <virtual-path>` form; the virtual path supplies module and package context while standard input supplies the source text. `fmt` and `test` instead accept their documented path lists. `aura run` alone accepts program arguments after `--`. `--format human|json` is accepted by `check`, `run`, and `build` and does not change source-language parsing.

Aura source accepted by these commands is governed by the [Grammar](/manual/grammar), not by this page. Command names, options, output formats, and exit statuses are case-sensitive.

## Typing Rules

`check`, `run`, and `build` use the same package resolver, parser, static checker, and ownership checker. A program that fails those stages is not executed or emitted. `analyze` exposes the same semantic model in a recoverable editor-oriented report, and `complete` queries completion at a zero-based source position. Inspection commands expose intermediate compiler data but do not define additional source types.

For `check`, `run`, and `build`, JSON diagnostic mode has schema version `1` and contains a `diagnostics` array. The current compile pipeline stops at its first failure, so a failed invocation contains exactly one diagnostic and a successful `check` contains none; tools must not treat that cap as proof that the rest of an invalid source file has no errors. Each diagnostic carries its stable code, severity, message, optional primary span, secondary spans, notes, help, machine-applicable edits, `call_frames`, and `task_ancestry`. The frame arrays are always present. Call frames are ordered innermost first; task ancestry is ordered youngest first. Every public schema-version-1 frame span owns a required `path` and its coordinates, so multi-file failures do not rely on the primary span's path. The `analyze` and persistent-service representations carry the same semantic diagnostic information in their documented zero-based editor-coordinate shapes, where `file_path` is optional only for source-only analysis.

## Runtime Semantics

`check` performs no program execution. `run` executes checked MIR and forwards arguments after `--` to `sys.args()`. `build` emits a standalone host executable: `auto` tries the direct backend and may use the MIR-launcher fallback, while `direct` makes inability to emit directly an error. Both built forms must preserve the checked language semantics.

Human-format `check` success writes exactly `ok` followed by a newline. JSON-format success writes a schema-version-1 object with an empty diagnostic array. `analyze` returning source diagnostics is a successful tooling request and therefore exits `0`; malformed CLI usage exits `2`; compile, build, and runtime failures exit `1`. A successful `main() -> int32` requests that integer as the process status. The complete stream and status rules are in the table above.

## Ownership And Evaluation Order

Selecting a CLI command or output format does not alter Aura ownership, borrowing, cleanup, or evaluation order. `run`, a directly built program, and a MIR-launcher build must observe the same left-to-right source evaluation and the same resource cleanup rules.

Tool-side mutations are explicit: `fmt` without `--check`, `deps update`, and successful lockfile-producing package commands may write files; `analyze --stdin` and `complete --stdin` do not write a lockfile. Source received through `--stdin` is not retained after the command or service request, but its virtual path remains semantically significant for imports, module identity, and diagnostic locations.

## Diagnostics

Compiler-backed commands can surface the complete append-only registry. `AU1001` means invalid lexical input; `AU1002` means an invalid f-string delimiter; and `AU1101` means invalid syntax. `AU2001` means name-resolution failure; `AU2002` means type mismatch; `AU2003` means unsupported operator; `AU2004` means argument-binding failure; `AU2005` means unsupported syntax or feature; `AU2006` means a builtin method collision; `AU2007` means builtin function redefinition; `AU2008` means equality unavailable; and `AU2999` means a general compile-time rejection without a narrower code. `AU3001` means use of a moved value; `AU3002` means a borrow violation; `AU3003` means a mutability violation; `AU3004` means an invalid ownership mode; `AU3005` means a non-copy indexed read; `AU3006` means a non-copy indexed compound assignment; `AU3007` means non-cloneable state duplication; `AU3008` means a non-transferable task or Queue boundary; `AU3009` means single-consumer task-result duplication; and `AU3010` means a view escape or returned-view provenance failure. `AU4001` means a general runtime trap; `AU4002` means arithmetic overflow or underflow; `AU4003` means a bounds or lookup violation; `AU4004` means a zero divisor; `AU4005` means a resource, allocation, or I/O failure; `AU4006` means invalid runtime configuration; and `AU4007` means a numeric Array shape or reduction violation. The structured schema is defined in [Diagnostics](/manual/diagnostics).

Human diagnostics render as `error[AU####]` with source context when a span is
available. Ordinary notes are followed by readable call-chain and task-entry
notes synthesized from the typed frame arrays; those generated lines are not
duplicated in the structured `notes` field. `--format json` emits the
schema-version-1 report on standard error for a failing `check`, `run`, or
`build`. Usage errors, missing command-line operands, and host failures that
prevent the tool itself from starting are CLI errors rather than
Aura-language diagnostics; they print usage or a tool error and have no
`AU####` code.

## Backend Support

The parser, checker, package resolver, diagnostic model, analysis engine, and MIR lowering are shared by all maintained execution routes. `aura run --backend mir` executes the lowered MIR and is the default. `aura run --backend direct` builds a native binary with the direct backend and executes it, reporting a build or launch failure as an error. For `--format json` on maintained Unix hosts, the CLI supplies a private trap-signal pipe plus a separate diagnostic-data pipe bounded to 1,048,576 bytes. A native child signals a trap and writes exactly one EOF-delimited compiler-owned diagnostic JSON record, suppressing human stderr only after that write succeeds. Native initialization owns both descriptors, marks them close-on-exec, and removes their internal environment entries before user code, so an Aura-started subprocess cannot observe them or delay EOF. No signal or record is written for a normal `main` result, including status `1`, so the CLI does not infer a trap from a process status or parse human text. A trap signal without one valid record is a hard host execution failure. Human direct runs create no private protocol; the child renders its complete human diagnostic.

`aura run --backend auto` prefers the direct backend and degrades to the MIR runtime only when direct building or launching is unavailable. Once a direct child runs, an Aura trap, signal termination, wait failure, or diagnostic-protocol failure is a final program/execution outcome and never triggers MIR fallback. Human mode prints an actual fallback reason on standard error before the MIR program runs; JSON mode includes it in the final structured report after execution. A forced `direct` run never degrades, so a parity or benchmark caller cannot silently measure the other backend. Every backend observes the same program arguments, standard output, exit code, and complete runtime diagnostic, including typed call frames and task ancestry.

The native path is content-addressed. A successful direct build atomically publishes its binary, that artifact's SHA-256, and a key-bound unique entry identity into a cache keyed by native cache format `v5`, compiler-owned semantic-interface schema version `6`, this compiler's version, the host target, the backend, the exact linked runtime archive content, its ordered native link arguments, and the complete lowered program, which already incorporates the entry source and every resolved dependency source. The format and semantic identities are independent key fields: changing compiler-owned type or ownership metadata invalidates artifacts even if the native container format remains readable. Cache artifacts above 512 MiB are simply not retained; the just-built program still runs. A later run with the same inputs requires a regular directory and bounded regular sidecars, verifies the entry identity, digest, artifact size, execute permission, and platform-native executable shape, and only then uses the entry. It launches a private copy of exactly those verified bytes through a no-shell-fallback native execution path, so replacement of the shared cache pathname after verification cannot substitute different bytes. Missing or mismatched metadata, truncation, a non-regular member, a lost execute permission, or an executable-format/architecture rejection makes the entry a cache miss: Aura quarantines and removes that exact entry, then rebuilds before running. A temporary-directory failure, process-resource failure, `noexec` mount, or other environmental launch failure is not evidence that verified cache bytes are corrupt; Aura preserves the entry and reports or falls back according to the selected backend.

On maintained Unix hosts, cache establishment is coordinated across processes. A short runtime-identity lock protects source-checkout runtime discovery, and a separate writer lock for each content key protects the miss/recheck/build/publish sequence. Therefore, N concurrent cold runs of the same program perform one build; after that publication, the other N-1 processes recheck and consume the verified entry. Existing verified hits take the optimistic read path and do not wait for a writer holding that key. Locks are released before linking output is executed, while atomic publication and invalidation continue to ensure that readers never observe a partial entry and a stale invalidator cannot delete a replacement published for the same key.

In human mode, a native `run` flushes the exact line
`aura: waiting for a concurrent build...` before it blocks on another builder
and `aura: building native program...` before it starts building a native
program artifact. A
source-checkout `aura build` flushes the same exact wait line before
blocking on another process refreshing the shared runtime. The reporter
deduplicates each notice within one invocation. JSON `run` mode provisionally
prioritizes the one-document stderr contract over immediate progress: it
buffers the same exact strings and emits them in a successful report's
`progress` array or a failed diagnostic's `notes`. A JSON build failure
likewise retains a buffered wait notice in the diagnostic's `notes`. A
successful `auto` run fallback also carries
`fallback: {"from":"direct","to":"mir","reason":"..."}`; a failed MIR fallback
retains the direct failure and progress as diagnostic notes. Tools should
therefore not expect real-time progress in JSON mode until a structured
streaming contract is ratified.

`AURA_CACHE_DIR` selects the cache directory; the default is `~/.cache/aura/native`. The directory is a trust boundary. Its colocated SHA-256 detects corruption but does not authenticate bytes written by a hostile account, so the root must be private to the current OS user and every writer with access to it must be trusted. On the maintained Unix hosts, Aura rejects a root that is owned by another user or writable by group/other and creates or tightens accepted cache directories to mode `0700`. Private launch copies are removed after the child exits. Each launch carries an inherited exclusive lease, so later cleanup preserves the directory while either the `aura` parent or native child is still using it. Interrupted cache-publication, memo, and quarantine stages are collected only after their encoded 24-hour grace period and confirmation that their owner process is gone. An installed immutable runtime can still perform a direct build when caching is disabled or unavailable; no cache lock is required merely to build, and the uncached artifact is not retained.

ADR-0031 ratifies the command split: `aura run` defaults to `mir` for the interactive edit-run path, while `aura build` defaults to `auto` for artifact production. Maintained measurements put a cold miss at about 1.3 seconds; a direct hello-world executable is roughly 57 MB of statically linked runtime. Each cache hit reads, hashes, and privately materializes the artifact, and workloads dominated by programs seen once, including CI, still pay the cold path on every program. `aura build --backend direct` uses native direct emission, and `--backend auto` may select the checked MIR-launcher fallback. The language server delegates semantic analysis and completion to the persistent compiler service; every JSON-lines request and response identifies semantic-interface schema version `6`. A missing or different identity closes the incompatible service and invalidates all document analysis before the lexical recovery path is used, so cached function-type or ownership metadata cannot cross compiler versions. The lexical fallback is recovery-only and is not a second language implementation.

Backend parity is a release gate. A construct accepted by one maintained
execution backend must have the same observable result or complete diagnostic
in the other, including frame records and their source paths, subject only to
the platform limits documented below. The parity harness performs no
MIR-specific frame-note masking.

## Limits And Implementation-Defined Behavior

Native linking requires a supported host C compiler and the installed Aura runtime layout described above. `ast`, `ast-json`, and `mir` are inspection formats, not stable serialization APIs. The formatter currently normalizes the maintained whitespace surface; it is not a configurable style engine. `aura test` discovers tests by the `test_` name prefix rather than by annotation, and a timed-out worker cannot be forcibly stopped inside the CLI process.

Filesystem path interpretation, process exit-code width, executable format, linker selection, and availability of Unix-only APIs follow the maintained host platform. Package graph, source-size, recursion, runtime, and backend limits are collected in [Current Limits](/manual/current-limits).

## Status

The commands and contracts documented as maintained on this page are implemented in Aura 0.3 and covered by CLI, compiler, LSP, extension, backend-parity, and repository-gate tests. `analyze`, `complete`, and diagnostic schema version `1` are maintained tooling contracts; internal AST and MIR layouts are intentionally unstable.

Aura 0.3 has no package registry, publishing and installation workflow,
Windows support, configurable formatter, or annotation-based test discovery.
Its maintained execution engines are the MIR runtime and direct native
backend.
