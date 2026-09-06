# Downloads

Aura 0.3.3 is a technical preview. The compiler, command-line tools, editor
extension, reference manual, and source are distributed from the
[Aura GitHub repository](https://github.com/johnolafenwa/Aura).

## Aura CLI

Install the current preview with one command on a supported macOS or Linux
host, including x86-64 Ubuntu 24.04 inside Windows WSL 2:

```bash
curl -fsSL https://johnolafenwa.github.io/Aura/install.sh | sh
```

The installer downloads the matching release archive, verifies it against the
published `SHA256SUMS`, and installs Aura under `~/.local` by default. Set
`AURA_INSTALL_PREFIX` to select another prefix.

Choose a detailed platform guide:

- [Installation overview](/install/)
- [macOS: Apple silicon and Intel](/install/macos)
- [Linux: Ubuntu 24.04 and compatible x86-64 glibc hosts](/install/linux)
- [Windows 11 through Ubuntu on WSL 2](/install/windows-wsl)

Download the archive for your platform from the
[v0.3.3-preview release](https://github.com/johnolafenwa/Aura/releases/tag/v0.3.3-preview).
Each release includes Linux x64, macOS x64, and macOS arm64 archives together
with a `SHA256SUMS` manifest.

After extracting an archive, put its `bin` directory on `PATH` and verify the
installation:

```bash
aura --version
```

## VS Code Extension

Install **Aura Programming Language** from either public extension registry:

- [Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=JohnOlafenwa.vscode-aura-lang)
- [Open VSX](https://open-vsx.org/extension/JohnOlafenwa/vscode-aura-lang)

The registry packages are identical and carry the plain extension version
`0.3.4`. The extension needs the `aura` executable on `PATH` because semantic
editor features run through the compiler-owned `aura lsp` server.

Install from a terminal with:

```bash
code --install-extension JohnOlafenwa.vscode-aura-lang
```

For a manual installation, download
[`aura-language.vsix`](https://github.com/johnolafenwa/Aura/releases/download/v0.3.3-preview/aura-language.vsix)
from the GitHub Release, then choose **Extensions: Install from VSIX...** in
VS Code.

The [complete VS Code guide](/install/vscode) covers Marketplace, Open VSX,
manual VSIX, custom compiler paths, verification, and installation into a WSL
remote extension host.

## Documentation And Source

The release also includes the static Aura documentation archive. The current
book is available on [GitHub Pages](https://johnolafenwa.github.io/Aura/), and
the complete source is available from the
[Aura repository](https://github.com/johnolafenwa/Aura).
