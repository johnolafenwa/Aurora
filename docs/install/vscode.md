# Install The VS Code Extension

The **Aura Programming Language** extension provides `.au` syntax
highlighting, indentation, snippets, diagnostics, completion, hover,
go-to-definition, and document symbols.

The extension includes its JavaScript editor client and language-server
transport. Semantic analysis comes from the actual compiler server exposed by
`aura lsp`, so install the [Aura CLI](/install/) first and verify:

```bash
aura --version
aura help
```

## Install From Visual Studio Marketplace

Open the Extensions view in VS Code, search for **Aura Programming Language**
from publisher **JohnOlafenwa**, and select **Install**.

- [Open the Visual Studio Marketplace listing](https://marketplace.visualstudio.com/items?itemName=JohnOlafenwa.vscode-aura-lang)

The equivalent terminal command is:

```bash
code --install-extension JohnOlafenwa.vscode-aura-lang
```

Reload VS Code after installation.

## Install From Open VSX

Editors using Open VSX, including VSCodium, can install the same extension:

- [Open the Open VSX listing](https://open-vsx.org/extension/JohnOlafenwa/vscode-aura-lang)

In VSCodium, search for **Aura Programming Language** in Extensions or run:

```bash
codium --install-extension JohnOlafenwa.vscode-aura-lang
```

## Install The Release VSIX Manually

Download `aura-language.vsix` from the
[v0.3.3-preview release](https://github.com/johnolafenwa/Aura/releases/tag/v0.3.3-preview).
Then open the Command Palette and choose **Extensions: Install from VSIX...**.

The command-line form is:

```bash
code --install-extension ./aura-language.vsix
```

VS Code does not automatically update extensions installed from a VSIX by
default. Install the next release's VSIX manually when upgrading through this
path.

## Install In WSL

First complete [Install Aura On Windows With WSL](/install/windows-wsl).
Open the project from the Ubuntu terminal with `code .` and confirm that the
remote status bar shows **WSL: Ubuntu**.

Open Extensions in that remote window, find **Aura Programming Language**, and
select **Install in WSL: Ubuntu**. The Aura extension and the `aura` CLI must
both run in the WSL environment. Installing the extension only in the local
Windows extension host cannot reach the Linux compiler server reliably.

Verify the remote environment in VS Code's integrated terminal:

```bash
command -v aura
aura --version
```

## Use A Specific Aura Binary

The extension normally launches `aura` from `PATH`. To use another binary,
start VS Code with `AURA_LSP_AURA_PATH` set to its absolute path:

```bash
AURA_LSP_AURA_PATH="$HOME/tools/aura/bin/aura" code /path/to/project
```

For WSL, run that command from the WSL terminal so the path is a Linux path.

## Verify Language Support

Create or open a file ending in `.au`:

```aura
def greet(name: str) -> str:
    return f"hello {name}"

print(greet("Aura"))
```

Confirm all of the following:

1. The language mode in the lower-right corner reads **Aura**.
2. Keywords, strings, types, and interpolation receive Aura highlighting.
3. An incomplete or invalid expression produces an `AU####` diagnostic.
4. Completion appears after a binding or member-access prefix.
5. Hover shows compiler-owned type information.

## Troubleshooting

### The file opens as plain text

Confirm the filename ends in `.au`. Select the language mode in the lower-right
corner and choose **Aura**.

### Syntax colors work but semantic features do not

Syntax highlighting is bundled with the extension, while semantic features
need `aura lsp`. Open VS Code's integrated terminal and run:

```bash
command -v aura
aura --version
```

Restart VS Code after fixing `PATH`, or launch it with
`AURA_LSP_AURA_PATH` as shown above.

### Inspect the language-server output

Open **View → Output**, then select **Aura Language Server**. Startup and
request failures appear there without requiring a separate server package.
