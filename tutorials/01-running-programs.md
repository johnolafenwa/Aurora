# Running Programs

Aura currently runs through the bootstrap CLI, `aura`. You invoke it from the repository root using `cargo run -p aura --`.

## Your First Program

Create a file called `hello.au`:

```aura check-pass
print("hello, aura")
```

Run it:

```bash
cargo run -p aura -- run hello.au
```

You should see `hello, aura` printed to the terminal.

## The Core Commands

The three commands you will use most often:

```bash
cargo run -p aura -- check examples/classes/point_distance.au
cargo run -p aura -- run examples/classes/point_distance.au
cargo run -p aura -- build -o ./target/aura-point examples/point.au
```

- **`check`** -- parse and type-check the file without running it. Use this for fast feedback while editing.
- **`run`** -- execute the program through the MIR runtime. This is the easiest way to test your code.
- **`build`** -- compile to a standalone native binary. The output binary does not depend on the original `.au` source files at runtime.

## Build Backends

The `build` command accepts a `--backend` flag:

- `--backend auto` (the default) -- first tries direct native emission and may fall back to a standalone launcher with embedded MIR
- `--backend direct` -- requires direct native emission and rejects programs that cannot use it

In practice, the default is what you want. Use `direct` for backend testing or when an embedded-MIR fallback is unacceptable.

The build step requires a host C compiler. A source-checkout CLI may use Cargo to refresh the native runtime; installed release archives carry that runtime and do not require Rust. Built binaries preserve file, line, and caret context for runtime failures.

## Inspection Commands

These commands are for debugging and understanding your code:

- **`ast`** -- print the parsed syntax tree
- **`ast-json`** -- print the syntax tree as machine-readable JSON
- **`mir`** -- print the lowered MIR for the checked program
- **`analyze`** -- print machine-readable compiler analysis (diagnostics, symbols, hover, definition)
- **`complete`** -- print completion items for a position in the file

`check`, `run`, and `build` accept `--format human|json` for diagnostics. The
JSON form is schema-versioned and preserves the compiler's stable `AU####`
code, primary and related spans, notes, help, machine-applicable edits, and
typed runtime `call_frames` and `task_ancestry`. The two frame arrays are
always present and are empty for diagnostics without runtime frames.

```bash
cargo run -p aura -- ast examples/classes/point_distance.au
cargo run -p aura -- mir examples/control_flow/while_break_continue.au
cargo run -p aura -- analyze examples/classes/point_distance.au
cargo run -p aura -- complete --line 5 --character 11 --trigger . examples/point.au
```

For `complete`, `--line` and `--character` use zero-based positions. Member completion expects the cursor positioned just after `.`.

Use `help` and `--version` to see CLI usage and the current version:

```bash
cargo run -p aura -- help
cargo run -p aura -- --version
```

The source-checkout command prints the build channel and a 12-hex-digit source
commit, such as `aura 0.3.3-dev (0123456789ab)`. Release archives identify
their channel as `aura 0.3.3-preview (0123456789ab)`.

Use `deps update` to refresh git dependencies without deleting `Aura.lock` manually:

```bash
cargo run -p aura -- deps update
cargo run -p aura -- deps update util
```

## Stdin Mode For Editors

All commands support stdin for editor integration. Provide a virtual path so the compiler can resolve local imports:

```bash
cat examples/modules/simple_import.au | cargo run -p aura -- run --stdin "$(pwd)/examples/modules/simple_import.au"
```

This is how the VS Code language server communicates with the compiler for unsaved editor buffers.

## Package-Aware Commands

When a file lives under a package with `Aura.toml`, the CLI automatically resolves local modules from `src/`, resolves path and git dependencies by package name, and updates `Aura.lock`:

```bash
cargo run -p aura -- run examples/packages/local_path_dependencies/app/src/main.au
cargo run -p aura -- run examples/packages/workspace/app/src/main.au
```

Run the dependency update command from a package or workspace directory when you want to refresh moving git references:

```bash
cd examples/packages/local_path_dependencies/app
cargo run -p aura -- deps update
cargo run -p aura -- deps update util
```

See [18-packages-and-workspaces.md](18-packages-and-workspaces.md) for details.

## Scripts And `main`

Aura supports two entry styles.

### Top-level script

Write executable statements directly at the top level. This is the simplest way to start:

```aura check-pass
a = 56
b = 100
print(a + b)
```

See [examples/basics/top_level_script.au](../examples/basics/top_level_script.au).

### Explicit `main`

For programs that return an exit code, declare a `main` function:

```aura check-pass
def main() -> int32:
    print(5)
    return 0
```

See [examples/classes/point_distance.au](../examples/classes/point_distance.au).

Do not mix top-level executable statements with `main` in the same file. Choose one style.

## Editor Tooling

The VS Code language server uses compiler-backed `analyze` and `complete` output, which means the editor and CLI share the same type-checking engine for:

- diagnostics
- symbols
- hover
- go-to-definition
- completions

See [08-tooling.md](08-tooling.md) for setup instructions.
