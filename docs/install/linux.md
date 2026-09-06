# Install Aura On Linux

The Aura 0.3 preview supports x86-64 Ubuntu 24.04 and compatible glibc-based
Linux distributions. The release does not currently include Linux ARM64 or
musl archives.

## 1. Confirm The Host

```bash
uname -s
uname -m
```

The supported release path reports `Linux` and `x86_64` or `amd64`.

## 2. Install Download And Verification Tools

On Ubuntu 24.04:

```bash
sudo apt update
sudo apt install -y curl ca-certificates tar coreutils
```

`coreutils` supplies `sha256sum`, which the installer uses to verify the
downloaded archive.

## 3. Install Aura

```bash
curl -fsSL https://johnolafenwa.github.io/Aura/install.sh | sh
```

The installer selects `x86_64-unknown-linux-gnu`, downloads the archive and
`SHA256SUMS`, rejects a checksum mismatch, and installs under `~/.local`.

## 4. Add Aura To Bash

Add `~/.local/bin` to the login environment once:

```bash
grep -qxF 'export PATH="$HOME/.local/bin:$PATH"' "$HOME/.profile" || \
  printf '%s\n' 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.profile"
export PATH="$HOME/.local/bin:$PATH"
```

New login shells will read `~/.profile`. The final command updates the current
terminal immediately.

## 5. Verify The Installation

```bash
command -v aura
aura --version
```

The version should begin with `aura 0.3.3-preview`.

## 6. Run A Program

Create `hello.au`:

```aura
def main():
    print("hello from Aura on Linux")
```

Then run it:

```bash
aura run hello.au
```

## 7. Enable Native Builds

Install the C compiler and linker required by direct native execution:

```bash
sudo apt install -y build-essential
```

Verify the toolchain and build the example:

```bash
cc --version
aura build -o hello hello.au
./hello
```

## Upgrade Aura

```bash
aura upgrade
aura --version
```

`aura upgrade` downloads the current installer, verifies the published release
checksums, and replaces the compiler and bundled runtime in the same install
prefix. Set `AURA_INSTALL_PREFIX` when upgrading an installation in a custom
location.

## Troubleshooting

### `aura: command not found`

```bash
ls -l "$HOME/.local/bin/aura"
export PATH="$HOME/.local/bin:$PATH"
```

Persist the export in the startup file used by the current shell.

### `sha256sum` is missing

Install `coreutils`, then rerun the installer:

```bash
sudo apt install -y coreutils
```

### The archive will not start on the distribution

The published Linux binary targets x86-64 glibc systems. Confirm the
architecture with `uname -m` and the C library with `ldd --version`. Alpine
Linux and other musl systems are outside the current distribution matrix.

Continue with the [VS Code extension guide](/install/vscode) or
[Getting Aura Running](/learn/install-and-run).
