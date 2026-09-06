# Install Aura On macOS

Aura publishes separate macOS 15 archives for Apple silicon and Intel. The
installer uses `uname` to select the correct archive.

## 1. Confirm The Mac Architecture

Open Terminal and run:

```bash
uname -m
```

- `arm64` means Apple silicon.
- `x86_64` means Intel.

Both results are supported. Other macOS architectures do not have a release
archive.

## 2. Install Aura

macOS includes `curl`, `tar`, and `shasum`, which are the tools used by the
verified installer:

```bash
curl -fsSL https://johnolafenwa.github.io/Aura/install.sh | sh
```

The script downloads the matching `v0.3.3-preview` archive and checks it
against the release's `SHA256SUMS` file before copying anything into the
installation prefix.

## 3. Add Aura To zsh

The default installation location is `~/.local/bin/aura`. Add that directory
to the zsh login environment once:

```bash
grep -qxF 'export PATH="$HOME/.local/bin:$PATH"' "$HOME/.zshrc" || \
  printf '%s\n' 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.zshrc"
source "$HOME/.zshrc"
```

If `aura --version` already works, the directory was already on `PATH` and
this step is unnecessary.

## 4. Verify The Installation

```bash
command -v aura
aura --version
```

The command path should end in `.local/bin/aura`, and the version should begin
with:

```text
aura 0.3.3-preview
```

## 5. Run A Program

Create `hello.au`:

```aura
def main():
    print("hello from Aura on macOS")
```

Run it:

```bash
aura run hello.au
```

## 6. Enable Native Builds

The default MIR execution path does not need Xcode. Direct native execution
and `aura build` need Apple's linker and C toolchain. Install them with:

```bash
xcode-select --install
```

After the installer completes, verify and build:

```bash
xcode-select -p
aura build -o hello hello.au
./hello
```

## Upgrade Aura

Upgrade the installed CLI and runtime to the current published preview:

```bash
aura upgrade
aura --version
```

The command preserves the active install prefix and uses the same verified
installer as a fresh installation.

## Troubleshooting

### `aura: command not found`

Confirm the file exists and reload the shell:

```bash
ls -l "$HOME/.local/bin/aura"
source "$HOME/.zshrc"
```

### The installer reports an unsupported architecture

Run `uname -m`. Aura currently publishes macOS archives only for `arm64` and
`x86_64`.

### Native linking fails

Run `xcode-select -p`. If it fails, install or repair the Xcode command-line
tools before using `aura build` or `--backend direct`.

Continue with the [VS Code extension guide](/install/vscode) or
[Getting Aura Running](/learn/install-and-run).
