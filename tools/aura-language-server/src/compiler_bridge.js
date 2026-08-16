"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { pathToFileURL } = require("node:url");
const { spawn } = require("node:child_process");
const { uriToPath } = require("./uri");

let workspaceRoots = [];
let compilerService = null;
let compilerServiceKey = null;
let compilerSchemaMismatchHandler = null;
const COMPILER_REQUEST_TIMEOUT_MS = 15_000;
const COMPILER_RESPONSE_LIMIT_BYTES = 16 * 1024 * 1024;
// The compiler owns the canonical identity; this transport declares the one
// compiler interface it can safely decode.
const SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION = 6;

function setCompilerSchemaMismatchHandler(handler) {
  compilerSchemaMismatchHandler = typeof handler === "function" ? handler : null;
}

function notifyCompilerSchemaMismatch(details) {
  if (compilerSchemaMismatchHandler) {
    compilerSchemaMismatchHandler(details);
  }
}

function setWorkspaceRoots(roots) {
  const nextRoots = Array.isArray(roots) ? roots.filter(Boolean) : [];
  if (JSON.stringify(nextRoots) !== JSON.stringify(workspaceRoots)) {
    disposeCompilerService();
  }
  workspaceRoots = nextRoots;
}

async function analyzeWithCompiler(uri, source) {
  try {
    return await requestCompiler("analyze", {
      path: uriToPath(uri) || uri,
      source
    });
  } catch (error) {
    reportCompilerFailure("analysis", error);
    return null;
  }
}

async function completeWithCompiler(
  uri,
  source,
  line,
  character,
  triggerCharacter,
  cancellationToken
) {
  try {
    return await requestCompiler(
      "complete",
      {
        path: uriToPath(uri) || uri,
        source,
        line,
        character,
        trigger: triggerCharacter || null
      },
      cancellationToken
    );
  } catch (error) {
    reportCompilerFailure("completion", error);
    return null;
  }
}

function requestCompiler(method, params, cancellationToken) {
  const command = resolveCompilerCommand();
  const key = JSON.stringify(command);
  if (!compilerService || compilerService.closed || compilerServiceKey !== key) {
    disposeCompilerService();
    compilerService = new CompilerService(command, {
      onSemanticSchemaMismatch: notifyCompilerSchemaMismatch
    });
    compilerServiceKey = key;
  }
  return compilerService.request(method, params, cancellationToken);
}

class CompilerService {
  constructor(
    command,
    {
      requestTimeoutMs = COMPILER_REQUEST_TIMEOUT_MS,
      responseLimitBytes = COMPILER_RESPONSE_LIMIT_BYTES,
      onSemanticSchemaMismatch = () => {}
    } = {}
  ) {
    this.command = command;
    this.nextId = 1;
    this.pending = new Map();
    this.stdoutBuffer = "";
    this.stderrBuffer = "";
    this.closed = false;
    this.requestTimeoutMs = requestTimeoutMs;
    this.responseLimitBytes = responseLimitBytes;
    this.onSemanticSchemaMismatch = onSemanticSchemaMismatch;
    this.child = spawn(command.cmd, [...command.args, "lsp"], {
      cwd: command.cwd,
      stdio: ["pipe", "pipe", "pipe"]
    });
    this.child.stdout.setEncoding("utf8");
    this.child.stderr.setEncoding("utf8");
    this.child.stdout.on("data", (chunk) => this.handleStdout(chunk));
    this.child.stderr.on("data", (chunk) => {
      this.stderrBuffer = (this.stderrBuffer + chunk).slice(-65_536);
    });
    this.child.on("error", (error) => this.fail(error));
    this.child.on("close", (code) => {
      this.fail(
        new Error(
          this.stderrBuffer || `Aura compiler service exited with status ${code}`
        )
      );
    });
  }

  request(method, params, cancellationToken) {
    if (this.closed) {
      return Promise.reject(new Error("Aura compiler service is closed"));
    }
    if (cancellationToken && cancellationToken.isCancellationRequested) {
      return Promise.reject(new Error("Aura compiler request was cancelled"));
    }

    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.fail(
          new Error(
            `Aura compiler ${method} request timed out after ${this.requestTimeoutMs}ms`
          ),
          true
        );
      }, this.requestTimeoutMs);
      const cancellation = cancellationToken
        ? cancellationToken.onCancellationRequested(() => {
            this.fail(new Error(`Aura compiler ${method} request was cancelled`), true);
          })
        : null;
      this.pending.set(id, { resolve, reject, timer, cancellation });
      this.child.stdin.write(
        `${JSON.stringify({
          id,
          method,
          ...params,
          semantic_interface_version: SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION
        })}\n`
      );
    });
  }

  handleStdout(chunk) {
    if (this.closed) {
      return;
    }
    this.stdoutBuffer += chunk;
    if (Buffer.byteLength(this.stdoutBuffer, "utf8") > this.responseLimitBytes) {
      this.fail(new Error("Aura compiler response exceeded 16 MiB"), true);
      return;
    }

    let newline = this.stdoutBuffer.indexOf("\n");
    while (newline >= 0) {
      const line = this.stdoutBuffer.slice(0, newline);
      this.stdoutBuffer = this.stdoutBuffer.slice(newline + 1);
      if (line) {
        let response;
        try {
          response = JSON.parse(line);
        } catch (error) {
          this.fail(new Error(`invalid Aura compiler response: ${error.message}`), true);
          return;
        }
        if (
          response.semantic_interface_version !==
          SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION
        ) {
          const actualSchema = Object.prototype.hasOwnProperty.call(
            response,
            "semantic_interface_version"
          )
            ? response.semantic_interface_version
            : "<missing>";
          const error = new Error(
            `Aura compiler semantic schema mismatch: received \`${actualSchema}\`; expected \`${SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION}\``
          );
          this.fail(error, true);
          this.onSemanticSchemaMismatch({
            actual: actualSchema,
            expected: SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION
          });
          return;
        }
        const pending = this.pending.get(response.id);
        if (pending) {
          this.pending.delete(response.id);
          clearTimeout(pending.timer);
          if (pending.cancellation) {
            pending.cancellation.dispose();
          }
          if (Object.prototype.hasOwnProperty.call(response, "error")) {
            pending.reject(new Error(String(response.error)));
          } else {
            pending.resolve(response.result);
          }
        }
      }
      newline = this.stdoutBuffer.indexOf("\n");
    }
  }

  fail(error, kill = false) {
    if (this.closed) {
      return;
    }
    this.closed = true;
    if (kill) {
      this.child.kill();
    }
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      if (pending.cancellation) {
        pending.cancellation.dispose();
      }
      pending.reject(error);
    }
    this.pending.clear();
  }

  dispose() {
    if (!this.closed) {
      this.fail(new Error("Aura compiler service disposed"), true);
    }
  }
}

function disposeCompilerService() {
  if (compilerService) {
    compilerService.dispose();
  }
  compilerService = null;
  compilerServiceKey = null;
}

function reportCompilerFailure(operation, error) {
  process.stderr.write(
    `[aura-lsp] compiler ${operation} unavailable; using recovery analysis: ${error.message}\n`
  );
}

function findOccurrence(analysis, line, character) {
  if (!analysis || !Array.isArray(analysis.occurrences)) {
    return null;
  }

  return (
    analysis.occurrences.find(
      (occurrence) =>
        occurrence.line === line &&
        character >= occurrence.start_character &&
        character < occurrence.end_character
    ) || null
  );
}

function compilerDiagnosticsToLsp(analysis, documentUri) {
  return (analysis.diagnostics || []).map((diagnostic) => {
    const result = {
      severity: diagnostic.severity,
      range: {
        start: { line: diagnostic.line, character: diagnostic.start_character },
        end: { line: diagnostic.line, character: diagnostic.end_character }
      },
      message: diagnostic.message,
      source: "aura-compiler"
    };

    if (diagnostic.code) {
      result.code = diagnostic.code;
    }
    if (documentUri && diagnostic.secondary_spans?.length) {
      result.relatedInformation = diagnostic.secondary_spans.map((secondary) => ({
        location: {
          uri: documentUri,
          range: {
            start: {
              line: secondary.line,
              character: secondary.start_character
            },
            end: {
              line: secondary.line,
              character: secondary.end_character
            }
          }
        },
        message: secondary.label
      }));
    }
    result.data = {
      notes: diagnostic.notes || [],
      help: diagnostic.help || [],
      edits: diagnostic.edits || [],
      call_frames: diagnostic.call_frames || [],
      task_ancestry: diagnostic.task_ancestry || []
    };
    if (diagnostic.assertion_operands?.length) {
      result.data.assertion_operands = diagnostic.assertion_operands;
    }
    return result;
  });
}

function compilerSymbolsToLsp(analysis) {
  return (analysis.symbols || []).map(toDocumentSymbol);
}

function compilerDefinitionToLspLocation(documentUri, definition) {
  if (!definition) {
    return null;
  }

  const uri = definition.file_path
    ? pathToFileURL(definition.file_path).toString()
    : documentUri;
  return {
    uri,
    range: {
      start: { line: definition.line, character: definition.start_character },
      end: { line: definition.line, character: definition.end_character }
    }
  };
}

function compilerHoverAtPosition(analysis, line, character) {
  const occurrence = findOccurrence(analysis, line, character);
  if (!occurrence) {
    return null;
  }

  return {
    value: occurrence.hover,
    range: {
      start: {
        line: occurrence.line,
        character: occurrence.start_character
      },
      end: {
        line: occurrence.line,
        character: occurrence.end_character
      }
    }
  };
}

function compilerDefinitionAtPosition(documentUri, analysis, line, character) {
  const occurrence = findOccurrence(analysis, line, character);
  if (!occurrence) {
    return null;
  }
  return compilerDefinitionToLspLocation(documentUri, occurrence.definition);
}

function toDocumentSymbol(symbol) {
  const range = {
    start: { line: symbol.line, character: symbol.start_character || 0 },
    end: { line: symbol.line, character: symbol.end_character || symbol.start_character || 0 }
  };

  return {
    name: symbol.name,
    detail: symbol.detail || "",
    kind: symbolKind(symbol.kind),
    range,
    selectionRange: range,
    children: (symbol.children || []).map(toDocumentSymbol)
  };
}

function symbolKind(kind) {
  switch (kind) {
    case "class":
      return 5;
    case "function":
      return 12;
    case "method":
      return 6;
    case "field":
      return 8;
    case "enum":
      return 10;
    case "trait":
      return 11;
    case "variant":
      return 22;
    case "constant":
      return 14;
    default:
      return 13;
  }
}

function resolveCompilerCommand() {
  const envPath = process.env.AURA_LSP_AURA_PATH;
  if (envPath && fs.existsSync(envPath)) {
    return { cmd: envPath, args: [], cwd: undefined };
  }

  for (const root of workspaceRoots) {
    const debugBinary = path.join(root, "target", "debug", binaryName());
    if (fs.existsSync(debugBinary)) {
      return { cmd: debugBinary, args: [], cwd: root };
    }

    const releaseBinary = path.join(root, "target", "release", binaryName());
    if (fs.existsSync(releaseBinary)) {
      return { cmd: releaseBinary, args: [], cwd: root };
    }
  }

  for (const root of workspaceRoots) {
    if (
      fs.existsSync(path.join(root, "Cargo.toml")) &&
      fs.existsSync(path.join(root, "crates", "aura", "Cargo.toml"))
    ) {
      return { cmd: "cargo", args: ["run", "-q", "-p", "aura", "--"], cwd: root };
    }
  }

  return { cmd: "aura", args: [], cwd: workspaceRoots[0] };
}

function binaryName(platform = process.platform) {
  return platform === "win32" ? "aura.exe" : "aura";
}

function runCommand(cmd, args, input, cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, args, {
      cwd,
      stdio: ["pipe", "pipe", "pipe"]
    });

    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) {
        resolve(stdout);
      } else {
        reject(new Error(stderr || `command exited with status ${code}`));
      }
    });

    child.stdin.write(input);
    child.stdin.end();
  });
}

module.exports = {
  analyzeWithCompiler,
  binaryName,
  CompilerService,
  completeWithCompiler,
  compilerDefinitionAtPosition,
  compilerDefinitionToLspLocation,
  compilerDiagnosticsToLsp,
  compilerHoverAtPosition,
  compilerSymbolsToLsp,
  findOccurrence,
  disposeCompilerService,
  resolveCompilerCommand,
  runCommand,
  SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION,
  setCompilerSchemaMismatchHandler,
  setWorkspaceRoots,
  uriToPath
};
