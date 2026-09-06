# Install Aura

Aura 0.3 is distributed as a self-contained command-line tool with its private
native runtime. Choose the guide for the operating system where `aura` will
run:

| Platform | Release archive | Guide |
| --- | --- | --- |
| macOS 15, Apple silicon | `aarch64-apple-darwin` | [Install on macOS](/install/macos) |
| macOS 15, Intel | `x86_64-apple-darwin` | [Install on macOS](/install/macos) |
| Ubuntu 24.04 or compatible glibc Linux, x86-64 | `x86_64-unknown-linux-gnu` | [Install on Linux](/install/linux) |
| Windows 11, x86-64 | Linux archive inside WSL 2 | [Install on Windows with WSL](/install/windows-wsl) |

The installer detects the supported archive automatically, verifies its
SHA-256 checksum, and installs under `~/.local`:

```bash
curl -fsSL https://johnolafenwa.github.io/Aura/install.sh | sh
```

Verify the result in the same terminal after adding `~/.local/bin` to `PATH`:

```bash
aura --version
```

The expected release identity begins with `aura 0.3.3-preview`. The remaining
text is the source commit used to build the binary.

## What Gets Installed

The default layout is:

```text
~/.local/
├── bin/aura
├── lib/aura/
│   ├── libaura_compiler.a
│   └── native-link-args.json
└── share/aura/
    ├── examples/
    ├── README.md
    └── LICENSE
```

Set `AURA_INSTALL_PREFIX` when another prefix is required:

```bash
AURA_INSTALL_PREFIX="$HOME/tools/aura" \
  sh -c "$(curl -fsSL https://johnolafenwa.github.io/Aura/install.sh)"
```

Add the selected prefix's `bin` directory to `PATH` after installation.

## Editor Setup

Install the [Aura Programming Language extension](/install/vscode) after the
CLI works. The extension supplies the editor client, syntax grammar, and
snippets. Compiler-backed diagnostics, completion, hover, definitions, and
symbols use the installed `aura lsp` server.

## Native Builds

`aura run` and `aura check` work after installing the archive. Direct native
execution and `aura build` also require a host C toolchain:

- macOS: Xcode command-line tools
- Ubuntu and WSL: `build-essential`

The platform guides include the exact commands.

## Next Step

Continue with [Getting Aura Running](/learn/install-and-run) to create a source
file, run it, check it, and build a native executable.
