# Getting Aura Running

Aura release archives ship a command-line tool called `aura` plus its private native runtime under `lib/aura`. The tool parses, type-checks, runs, and builds Aura source files, and it also serves as the entry point for editor tooling.

Aura 0.3 is a technical preview. This chapter covers both a release archive and a source checkout.

## Install A Release Archive

The fastest installation path supports Linux x64, macOS x64, and macOS arm64:

```bash
curl -fsSL https://johnolafenwa.github.io/Aura/install.sh | sh
```

The script verifies the release checksum and installs the compiler plus its
native runtime under `~/.local`. If `~/.local/bin` is absent from `PATH`, the
installer prints the exact export command. Set `AURA_INSTALL_PREFIX` before
running the command to choose another prefix.

After Aura is installed, update the compiler and its bundled native runtime
with:

```bash
aura upgrade
```

For a manual installation, download the archive for a supported host, extract
it, and keep its directory layout intact:

```text
aura-v0.3.3-preview-<target>/
├── bin/aura
├── lib/aura/
    ├── libaura_compiler.a
    └── native-link-args.json
└── examples/
    ├── basic_addition.au
    └── agents/retrying_network_worker.au
```

Add the extracted `bin` directory to `PATH`. Running and checking programs need
no Rust installation. Building a native executable needs a host C compiler
because `aura` performs the final host link itself.

Aura does not publish a native Windows archive. Windows 11 users can run the
Linux x86-64 release inside Ubuntu on WSL 2. See the detailed
[platform installation guides](/install/) and the repository's
supported-platform matrix before relying on an unlisted host.

## Choose Your Platform Guide

Use the guide for the system where the `aura` command will run:

- [Install on macOS](/install/macos) covers Apple silicon and Intel Macs,
  persistent `PATH` setup, Xcode command-line tools, and verification.
- [Install on Linux](/install/linux) covers Ubuntu 24.04 and compatible
  x86-64 glibc systems, required packages, and the native build toolchain.
- [Install on Windows with WSL 2](/install/windows-wsl) covers Ubuntu setup,
  Linux filesystem placement, Aura installation inside WSL, and remote VS Code.
- [Install the VS Code extension](/install/vscode) covers Marketplace, Open
  VSX, manual VSIX, WSL, compiler paths, and editor verification.

## Build From Source

Contributors building Aura itself need the pinned Rust toolchain and a host C compiler.

- **Rust**: install through [rustup](https://rustup.rs). `rust-toolchain.toml` selects Rust 1.95.0.
- **C compiler**: macOS provides one through the Xcode command-line tools
  (`xcode-select --install`). On Linux and Ubuntu under WSL 2,
  `build-essential` supplies the supported host toolchain. Native Windows
  source builds remain outside the distribution matrix.

## Build The Compiler

Clone the repository and build a release binary:

```bash
git clone https://github.com/johnolafenwa/Aura.git
cd Aura
cargo build --release -p aura
```

The release build lives at `./target/release/aura`. In a source checkout, `aura build` can use the sibling Cargo-built runtime. A distributed archive instead uses the runtime installed beside the executable.

Put `aura` on your path so the rest of the commands in this book read naturally:

```bash
export PATH="$PWD/target/release:$PATH"
aura --version
```

Preview builds identify both their channel and source commit, for example
`aura 0.3.3-preview (0123456789ab)`. Source-checkout builds identify their
channel as `aura 0.3.3-dev (0123456789ab)`.

On Unix shells, consider adding that export to your shell profile.

## Install The VS Code Extension

Install the CLI first and confirm that VS Code will be able to find it:

```bash
command -v aura
aura --version
```

Install **Aura Programming Language** from the Visual Studio Marketplace, or
run this command from a terminal where `code` is available:

```bash
code --install-extension JohnOlafenwa.vscode-aura-lang
```

Open an `.au` file and confirm that the language mode reads **Aura**. Syntax
highlighting is bundled with the extension. Diagnostics, completion, hover,
definitions, and symbols come from the compiler server that the extension
launches through `aura lsp`.

On Windows with WSL 2, open the project from the Ubuntu terminal with `code .`.
In the resulting **WSL: Ubuntu** window, select **Install in WSL: Ubuntu** for
the Aura extension. The extension and `aura` CLI must both run inside WSL.

The [complete VS Code installation guide](/install/vscode) also covers Open
VSX, manual VSIX installation, custom compiler paths, and troubleshooting.

## Your First Program

Save the following as `hello.au`:

```aura
print("hello from aura")
```

Run it:

```bash
aura run hello.au
```

You should see:

```
hello from aura
```

The program is a **top-level script**. Aura runs the file line by line and exits when it reaches the end.

## Using `main`

For programs that want an explicit entry point, define a function named `main`:

```aura
def main() -> int32:
    print("ready")
    return 0
```

`main` takes no parameters. It returns either `int32` or `None`. A returned `int32` becomes the process exit code when the program is built as a native binary. A file may use script-style top-level statements **or** define `main`, but not both.

## The CLI At A Glance

The commands you will use day to day are:

| Command | What it does |
| --- | --- |
| `aura run file.au` | Parse, type-check, and execute the program. |
| `aura check file.au` | Parse and type-check without running. |
| `aura check --format json file.au` | Emit schema-versioned structured diagnostics for tooling. |
| `aura build -o path file.au` | Compile a standalone native binary to `path`. |
| `aura ast file.au` | Print the parsed syntax tree. |
| `aura mir file.au` | Print the lowered intermediate representation. |
| `aura analyze file.au` | Emit compiler-backed analysis used by editor tooling. |
| `aura complete --line N --character M file.au` | Emit completion items at a source position. |
| `aura deps update [name]` | Refresh git dependencies and rewrite `Aura.lock`. |

Use `aura help` for the full list and `aura --version` to confirm the preview
channel and exact source revision you are running.

`aura run` defaults to the MIR runtime for a fast edit-run loop. Use
`--backend direct` to require native execution, or `--backend auto` to prefer
native execution while visibly falling back to MIR when direct execution is
unavailable.

## Building A Native Binary

```bash
aura build -o ./hello hello.au
./hello
```

`aura build` defaults to `auto`, which first tries direct native emission and may fall back to a standalone launcher containing embedded MIR plus the MIR runtime. The resulting binary does not need the original `.au` source at runtime; it does still need the host C compiler to produce the artifact. Use `--backend direct` when fallback is unacceptable.

The [Running And Shipping](/learn/native-builds) chapter covers when to pick `run` versus `build` and what each path gives you.

## When Something Goes Wrong

Aura's error messages usually point at the exact place in the source where the compiler or runtime found the problem:

```
error[AU4002]: integer value `2147483648` does not fit in `int32`
 --> overflow.au:3:14
  |
3 |     c: int32 = a + b
  |              ^
```

The bracketed `AU####` identifier is stable. The `-->` line names the file,
line, and column, and the caret points at the offending expression. Related
spans, guidance, and safe source edits follow when available. A program with a
checker error will not run; a program with a runtime error prints the
diagnostic and exits with a non-zero status. Use `--format json` with `check`,
`run`, or `build` when a tool needs the same fields without parsing this human
layout. Runtime diagnostics also carry typed `call_frames` (innermost first)
and `task_ancestry` (youngest child first); both arrays are present in every
schema-version-1 diagnostic, including as `[]` when no runtime frames apply.

## Next

The next chapter builds a small program that counts and classifies values, and in doing so introduces bindings, functions, control flow, and `match`.
