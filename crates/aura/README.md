# aura CLI

This package contains the Aura bootstrap compiler CLI.

## Build The Binary

From the repository root:

```bash
cargo build -p aura --release
```

That produces the standalone binary at:

```text
target/release/aura
```

## Use The Binary Without Cargo

After the release build completes, run the binary directly:

```bash
./target/release/aura check examples/classes/point_distance.au
./target/release/aura run examples/classes/point_distance.au
./target/release/aura run --backend direct examples/classes/point_distance.au
./target/release/aura run examples/control_flow/match_literals.au
./target/release/aura run examples/generics/box_and_wrapper.au
./target/release/aura run examples/basics/default_arguments.au
./target/release/aura run examples/collections/list_basics.au
./target/release/aura run examples/collections/list_polish.au
./target/release/aura run examples/collections/list_algorithms.au
./target/release/aura run examples/collections/slices.au
./target/release/aura run examples/collections/dict_basics.au
./target/release/aura run examples/collections/set_basics.au
./target/release/aura run examples/basics/pass_keyword.au
./target/release/aura run examples/basics/assertions.au
./target/release/aura run examples/modules/simple_import.au
./target/release/aura run examples/packages/local_path_dependencies/app/src/main.au
./target/release/aura run examples/packages/workspace/app/src/main.au
./target/release/aura run examples/traits/greeter.au
./target/release/aura run examples/traits/generic_trait_impl.au
./target/release/aura run examples/traits/generic_trait_bounds.au
./target/release/aura run examples/traits/operator_traits.au
./target/release/aura run examples/traits/ordering_traits.au
./target/release/aura run examples/traits/specialized_trait_dispatch.au
./target/release/aura run examples/basics/numbers.au
./target/release/aura run examples/numbers/numeric_casts.au
./target/release/aura run examples/numbers/numeric_builtins.au
./target/release/aura run examples/numbers/numeric_arrays.au
./target/release/aura run examples/strings/string_methods.au
./target/release/aura run examples/strings/string_parsing_and_formatting.au
./target/release/aura run examples/io/read_text_file.au
./target/release/aura run examples/io/bytes_file_io.au
./target/release/aura run examples/io/process_run.au
./target/release/aura run examples/io/process_pipes.au
./target/release/aura run examples/io/process_supervisor.au
./target/release/aura run examples/io/tcp_echo.au
./target/release/aura run examples/io/tcp_bytes.au
./target/release/aura run examples/io/udp_echo.au
./target/release/aura run examples/io/http_roundtrip.au
./target/release/aura run examples/io/websocket_roundtrip.au
./target/release/aura run examples/io/unix_tls_roundtrip.au
./target/release/aura run examples/concurrency/sleep_builtin.au
./target/release/aura run examples/concurrency/yield_now.au
./target/release/aura build -o ./target/aura-point examples/point.au
./target/release/aura build --backend direct -o ./target/aura-direct examples/basic_addition.au
./target/release/aura ast examples/classes/point_distance.au
./target/release/aura ast-json examples/classes/point_distance.au
./target/release/aura mir examples/control_flow/while_break_continue.au
./target/release/aura analyze examples/classes/point_distance.au
./target/release/aura complete --line 5 --character 11 --trigger . examples/point.au
```

You can do the same with the other current examples:

```bash
./target/release/aura run examples/basics/main_function.au
./target/release/aura run examples/basics/top_level_script.au
./target/release/aura run examples/collections/list_iteration.au
./target/release/aura run examples/collections/list_polish.au
./target/release/aura run examples/collections/list_algorithms.au
./target/release/aura run examples/collections/slices.au
./target/release/aura run examples/collections/dict_basics.au
./target/release/aura run examples/collections/set_basics.au
./target/release/aura run examples/generics/box_and_wrapper.au
./target/release/aura run examples/traits/greeter.au
./target/release/aura run examples/basics/numbers.au
./target/release/aura run examples/numbers/numeric_casts.au
./target/release/aura run examples/numbers/numeric_builtins.au
./target/release/aura run examples/strings/string_methods.au
./target/release/aura run examples/io/read_text_file.au
./target/release/aura run examples/io/bytes_file_io.au
./target/release/aura run examples/io/process_run.au
./target/release/aura run examples/io/process_pipes.au
./target/release/aura run examples/io/process_supervisor.au
./target/release/aura run examples/io/tcp_echo.au
./target/release/aura run examples/io/tcp_bytes.au
./target/release/aura run examples/io/udp_echo.au
./target/release/aura run examples/io/http_roundtrip.au
./target/release/aura run examples/io/websocket_roundtrip.au
./target/release/aura run examples/io/unix_tls_roundtrip.au
./target/release/aura run examples/concurrency/bounded_queue.au
```

## Install The Binary Somewhere On Your Path

If you want to use `aura` without typing the full path, copy it into a directory on your shell `PATH`.

Example:

```bash
mkdir -p "$HOME/.local/bin"
cp target/release/aura "$HOME/.local/bin/aura"
```

Then run:

```bash
aura run examples/classes/point_distance.au
aura help
aura --version
aura deps update
aura deps update util
```

## Command Summary

- `aura help`
  - print CLI usage and exit successfully
- `aura --version`
  - print the build channel and 12-hex-digit source commit, for example
    `aura 0.3.3-preview (0123456789ab)` from a release archive or
    `aura 0.3.3-dev (0123456789ab)` from a source build, then exit successfully
- `aura check <file.au>`
  - parse and type check a program
  - add `--format json` for the schema-versioned structured diagnostic document; human diagnostics remain the default
  - nested package modules can be checked directly, with the CLI inferring the nearest package root that satisfies their imports
  - package entrypoints under `src/` resolve `Aura.toml`, local path dependencies, git dependencies, workspaces, and `Aura.lock`
- `aura deps update [package]`
  - refresh git dependencies for the current package or workspace and rewrite `Aura.lock`
  - with no package name, all branch/tag/default-main git dependencies are refreshed
  - with a package name such as `util`, only that dependency is refreshed
- `aura run <file.au>`
  - run a program through the MIR runtime
  - this includes the maintained `pass` and `assert` statements plus the `sleep(duration)` and `yield_now()` builtins
  - the maintained user-facing surface includes explicit numeric and Duration floor division, signed computed Duration values, integer `.to_float()`, the expanded `str` utility and parsing surface, numeric helper builtins, `list[T]` with stable sorting and eager callable-powered map/filter, `dict[K, V]`, `set[T]`, `control.retry`, deterministic and OS-secure randomness through `random`, bounded `Queue[T]`, structural `Transfer` checks on task/Queue boundaries, single-consumer non-repeatable task results, scheduler-aware text/binary file I/O plus the maintained socket/networking and shell-free process/supervisor surface through `io`, `fs`, `net`, and `process`, specialized generic trait bounds, and the current operator-trait subset
  - local file imports and `public` module boundaries work for file-backed programs
  - manifest-rooted packages resolve sibling path dependencies, git dependencies, and workspace members when the entry file lives under a package `src/`
  - append `-- <program-args>...` to expose arguments through `sys.args()`
  - add `--format json` to select structured output when checking or execution fails
- `aura new <project-path>`
  - create `Aura.toml` and `src/main.au`; existing paths are never overwritten
- `aura fmt [--check] [path ...]`
  - normalize line endings/trailing whitespace/final newlines, or verify without writing
- `aura test [--timeout-ms N] [path ...]`
  - run package-aware Aura tests; defaults to `tests/` and a 30-second per-test timeout
  - a file declaring `def test_*()` functions reports one result per function, labelled `path::function`
  - a file declaring none keeps the file-level model and reports one result for the path
- `aura lsp`
  - run the persistent JSON-lines compiler service for editor tooling
  - every request and response carries compiler-owned semantic-interface version `6`; a schema mismatch closes the connection before analysis
- `aura run [--backend mir|direct|auto] <file.au> [-- <program-args>...]`
  - `mir` executes the lowered MIR and is the default
  - `direct` builds a native binary and runs it, reporting build or launch failures rather than degrading
  - `auto` prefers `direct` and degrades to the MIR runtime; human mode prints the reason before the fallback program runs, while JSON mode includes it in the final structured report
  - successful native builds are cached by content under `AURA_CACHE_DIR`, defaulting to `~/.cache/aura/native`; every hit verifies the entry identity, artifact SHA-256, regular-file/execute state, size bound, and executable shape, then launches a private copy of those verified bytes without a shell fallback
  - on maintained Unix hosts, concurrent cold runs of the same content key coordinate through cross-process locks: one process builds and atomically publishes the entry, while the remaining processes wait and then reuse the verified result; established warm hits do not wait on that key's writer lock
  - human output flushes `aura: waiting for a concurrent build...` before blocking and `aura: building native program...` before building a native program artifact; JSON mode currently buffers these notices so stderr remains exactly one JSON document, reporting them through `progress` on success or diagnostic `notes` on failure, while an `auto` fallback also records its direct-to-MIR transition and reason in `fallback`
  - malformed entries and executable-format/architecture failures are discarded and rebuilt; temporary-directory, process-resource, and other environmental launch failures preserve the verified entry and follow the selected backend's ordinary error/fallback policy
  - the cache directory is a trust boundary: use only a location private to the current OS account; on the maintained Unix hosts, Aura rejects roots owned by another user or writable by group/other
  - cache keys independently include native cache format `v5`, semantic-interface schema `v6`, the exact linked runtime archive, and ordered native link arguments; inherited launch leases and owner-aware staging cleanup prevent interrupted-run cleanup from deleting a live native child
  - caching is optional for an installed immutable runtime layout: an empty or unavailable cache does not prevent an otherwise valid direct build, but that build is not retained for a later hit
- `aura build -o <output> <file.au>`
  - compile a standalone native binary for a program
  - this accepts `--backend auto|direct`
  - this also accepts `--format human|json` for compile and build diagnostics
  - in human mode, a source-checkout build flushes
    `aura: waiting for a concurrent build...` before blocking on another
    process that is refreshing the shared native runtime
  - `auto` is the default; it first tries the direct native backend and may fall back to a standalone embedded-MIR launcher when direct emission is unavailable
  - `direct` forces the low-level native backend for the full currently implemented Aura language surface
  - source-checkout builds can refresh the runtime through Cargo; packaged release builds use the bundled runtime and require only a host C compiler
  - file-backed and stdin-backed programs with local module imports and package dependencies build correctly through this path
  - the maintained direct build path covers builtin scheduler-aware text/binary file I/O, poll-driven TCP/UDP/WebSocket/Unix/TLS socket I/O, higher-level HTTP helpers, and the shell-free `process` surface including supervised child processes with restart policies
- `aura ast <file.au>`
  - print the parsed syntax tree
- `aura ast-json <file.au>`
  - print the parsed syntax tree as JSON
- `aura mir <file.au>`
  - print the lowered MIR for the checked program
- `aura analyze <file.au>`
  - print machine-readable compiler analysis as JSON
  - file-backed and stdin-backed analysis resolve local imports relative to the supplied path
  - nested package modules can be analyzed directly without false import diagnostics
  - compiler-backed definitions point across files for imported symbols
- `aura complete --line <n> --character <n> [--trigger .] <file.au>`
  - print machine-readable completion items as JSON
  - `--line` and `--character` are zero-based
  - member completion expects the cursor to be positioned just after `.`
  - the CLI tolerates the common incomplete-editor state where the buffer contains one or more dangling member accesses such as `counter.` or `helpers.math.`, including at EOF
  - local imported modules participate in compiler-backed completions for both file-backed and stdin-backed buffers, including imported trait methods
- built binaries preserve file, line, and caret context for arithmetic runtime failures such as division by zero
- MIR and directly generated native failures preserve the same typed Aura
  call frames and child-task ancestry. Human output renders those records as
  call-chain/task notes; JSON output exposes `call_frames` and
  `task_ancestry` arrays without requiring tools to parse prose.

## Stdin Mode

Compiler-facing JSON commands use stdin for editor integration, and the ordinary `check`, `run`, and `build` commands honor the supplied stdin path when resolving local module imports.

Examples:

```bash
cat examples/classes/point_distance.au | ./target/release/aura analyze --stdin /virtual/point.au
cat examples/classes/point_distance.au | ./target/release/aura ast-json --stdin /virtual/point.au
cat examples/point.au | ./target/release/aura complete --line 5 --character 11 --trigger . --stdin /virtual/point.au
cat examples/point.au | ./target/release/aura build -o ./target/aura-point --stdin /virtual/point.au
cat examples/modules/simple_import.au | ./target/release/aura analyze --stdin "$(pwd)/examples/modules/simple_import.au"
cat examples/modules/simple_import.au | ./target/release/aura check --stdin "$(pwd)/examples/modules/simple_import.au"
cat examples/modules/simple_import.au | ./target/release/aura run --stdin "$(pwd)/examples/modules/simple_import.au"
./target/release/aura check examples/packages/local_path_dependencies/app/src/main.au
./target/release/aura run examples/packages/workspace/app/src/main.au
```

## Diagnostics

When a compiler-facing command fails, the default human renderer prints:

- the stable `AU####` diagnostic code and error message
- file, line, and column
- the relevant source line
- a caret under the failure location
- labeled related spans, notes, help, and machine-applicable fixes when present

`aura check --format json` emits `{"schema_version":1,"diagnostics":[...]}`.
`run` and `build` use the same structure for failures. Each diagnostic carries
its code, severity, message, primary and secondary spans, notes, help, and
edits, plus `call_frames` in innermost-first order and `task_ancestry` in
youngest-first order. The frame arrays are always present, including when
empty. Editor tooling consumes the same compiler-owned fields.

For `aura run --format json --backend direct`, the CLI gives the native child a
private trap-signal pipe and a separate bounded diagnostic-data pipe. The
child signals and writes one compiler-owned JSON record only when Aura
traps; ordinary nonzero `main` results write neither. Native initialization
hides both internal descriptors from user code and marks them close-on-exec.
This lets JSON mode distinguish a program returning status `1` from an
`AU####` runtime failure without scraping human stderr, and a signalled
missing/malformed record becomes a hard post-launch failure rather than an
`auto` fallback. Human direct runs create no private channel and retain the
native renderer.

## Current Limitation

Building the Aura compiler itself still uses Cargo. Installed release archives are relocatable and do not use Cargo when compiling an Aura program.

The supported build path today is:

1. build once with `cargo build -p aura --release`
2. use the resulting `aura` binary directly after that

The current `aura build` matrix is:

1. `--backend auto` is the default
2. `--backend direct` uses the true direct native backend for the full currently implemented Aura language surface
3. built binaries run without the original `.au` source files
4. both backend paths need a host C compiler; installed archives load the bundled runtime and link manifest without Cargo or the source checkout

The maintained execution architecture is:

1. `aura run` executes through the MIR runtime
2. `aura build --backend direct` requires direct native emission; `--backend auto` may instead package MIR with the native runtime when direct emission is unavailable
3. both execution paths cover the maintained Aura language surface, including builtin text/binary file I/O, shell-free subprocess helpers, plus TCP, UDP, HTTP, WebSocket, Unix-socket, and TLS networking
