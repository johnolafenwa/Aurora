"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const childProcess = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const {
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
} = require("../src/compiler_bridge");
const { createDocumentStateCache } = require("../src/document_state");

test.after(() => {
  setCompilerSchemaMismatchHandler(null);
  disposeCompilerService();
});

const repoRoot = path.join(__dirname, "../../..");
const pointPath = path.join(repoRoot, "examples/point.au");
const pointUri = `file://${pointPath}`;
const pointSource = fs.readFileSync(pointPath, "utf8");
const traitPath = path.join(repoRoot, "examples/traits/greeter.au");
const traitUri = `file://${traitPath}`;
const traitSource = fs.readFileSync(traitPath, "utf8");
const modulesPath = path.join(repoRoot, "examples/modules/simple_import.au");
const modulesUri = `file://${modulesPath}`;
const modulesSource = fs.readFileSync(modulesPath, "utf8");
const namespaceTypesPath = path.join(repoRoot, "examples/modules/namespace_import_types.au");
const namespaceTypesUri = `file://${namespaceTypesPath}`;
const namespaceTypesSource = fs.readFileSync(namespaceTypesPath, "utf8");

test("compiler bridge helper conversions cover diagnostics, symbols, and definition ranges", () => {
  assert.deepEqual(compilerDiagnosticsToLsp({}), []);

  const diagnostics = compilerDiagnosticsToLsp(
    {
      diagnostics: [
        {
          code: "AU2001",
          severity: 1,
          line: 2,
          start_character: 4,
          end_character: 9,
          message: "unknown name",
          secondary_spans: [
            {
              line: 0,
              start_character: 4,
              end_character: 9,
              label: "declared here"
            }
          ],
          notes: ["names are lexically scoped"],
          help: ["declare the name before using it"],
          edits: []
        }
      ]
    },
    "file:///workspace/main.au"
  );
  assert.deepEqual(diagnostics, [
    {
      severity: 1,
      range: {
        start: { line: 2, character: 4 },
        end: { line: 2, character: 9 }
      },
      message: "unknown name",
      source: "aura-compiler",
      code: "AU2001",
      relatedInformation: [
        {
          location: {
            uri: "file:///workspace/main.au",
            range: {
              start: { line: 0, character: 4 },
              end: { line: 0, character: 9 }
            }
          },
          message: "declared here"
        }
      ],
      data: {
        notes: ["names are lexically scoped"],
        help: ["declare the name before using it"],
        edits: [],
        call_frames: [],
        task_ancestry: []
      }
    }
  ]);

  assert.deepEqual(compilerSymbolsToLsp({}), []);
  const symbols = compilerSymbolsToLsp({
    symbols: [
      {
        name: "Point",
        kind: "class",
        detail: "class Point",
        line: 0,
        start_character: 0,
        end_character: 5,
        children: [
          {
            name: "x",
            kind: "field",
            detail: "x: int32",
            line: 1,
            start_character: 4,
            end_character: 5,
            children: []
          }
        ]
      },
      {
        name: "distance",
        kind: "function",
        detail: "function distance",
        line: 2,
        start_character: 0,
        end_character: 8,
        children: []
      },
      {
        name: "greet",
        kind: "method",
        detail: "method greet",
        line: 4,
        start_character: 4,
        end_character: 9,
        children: []
      },
      {
        name: "Status",
        kind: "enum",
        detail: "enum Status",
        line: 6,
        start_character: 0,
        end_character: 6,
        children: [
          {
            name: "Ready",
            kind: "variant",
            detail: "variant Ready",
            line: 7,
            start_character: 4,
            end_character: 9,
            children: []
          }
        ]
      },
      {
        name: "Greeter",
        kind: "trait",
        detail: "trait Greeter",
        line: 8,
        start_character: 0,
        end_character: 7,
        children: []
      },
      {
        name: "answer",
        kind: "constant",
        detail: "answer: int64",
        line: 9,
        start_character: 0,
        end_character: 6,
        children: []
      },
      {
        name: "mystery",
        kind: "unknown",
        detail: undefined,
        line: 3,
        start_character: undefined,
        end_character: undefined,
        children: undefined
      }
    ]
  });
  assert.equal(symbols[0].kind, 5);
  assert.equal(symbols[0].children[0].kind, 8);
  assert.equal(symbols[1].kind, 12);
  assert.equal(symbols[2].kind, 6);
  assert.equal(symbols[3].kind, 10);
  assert.equal(symbols[3].children[0].kind, 22);
  assert.equal(symbols[4].kind, 11);
  assert.equal(symbols[5].kind, 14);
  assert.equal(symbols[6].kind, 13);

  assert.equal(
    findOccurrence(
      {
        occurrences: [{ line: 1, start_character: 2, end_character: 5, hover: "hover" }]
      },
      1,
      3
    ).hover,
    "hover"
  );
  assert.equal(findOccurrence({ occurrences: [] }, 0, 0), null);
  assert.equal(findOccurrence(null, 0, 0), null);

  assert.equal(compilerDefinitionToLspLocation("file:///workspace/main.au", null), null);
  assert.deepEqual(
    compilerDefinitionToLspLocation("file:///workspace/main.au", {
      file_path: null,
      line: 2,
      start_character: 1,
      end_character: 4
    }),
    {
      uri: "file:///workspace/main.au",
      range: {
        start: { line: 2, character: 1 },
        end: { line: 2, character: 4 }
      }
    }
  );

  assert.deepEqual(
    compilerHoverAtPosition(
      {
        occurrences: [
          {
            line: 4,
            start_character: 2,
            end_character: 6,
            hover: "```aura\nfunction greet() -> str\n```"
          }
        ]
      },
      4,
      3
    ),
    {
      value: "```aura\nfunction greet() -> str\n```",
      range: {
        start: { line: 4, character: 2 },
        end: { line: 4, character: 6 }
      }
    }
  );
  assert.equal(compilerHoverAtPosition({ occurrences: [] }, 1, 1), null);

  assert.deepEqual(
    compilerDefinitionAtPosition(
      "file:///workspace/main.au",
      {
        occurrences: [
          {
            line: 1,
            start_character: 0,
            end_character: 3,
            hover: "hover",
            definition: {
              file_path: path.join(repoRoot, "examples/modules/pkg/types.au"),
              line: 7,
              start_character: 4,
              end_character: 9
            }
          }
        ]
      },
      1,
      1
    ),
    {
      uri: `file://${path.join(repoRoot, "examples/modules/pkg/types.au")}`,
      range: {
        start: { line: 7, character: 4 },
        end: { line: 7, character: 9 }
      }
    }
  );
  assert.equal(
    compilerDefinitionAtPosition(
      "file:///workspace/main.au",
      {
        occurrences: [
          {
            line: 1,
            start_character: 0,
            end_character: 3,
            hover: "hover",
            definition: null
          }
        ]
      },
      1,
      1
    ),
    null
  );
  assert.equal(compilerDefinitionAtPosition("file:///workspace/main.au", { occurrences: [] }, 9, 9), null);
});

test("compiler bridge defaults metadata omitted by older compatible records", () => {
  const diagnostic = {
    code: "AU3001",
    severity: 1,
    line: 1,
    start_character: 4,
    end_character: 5,
    message: "use of moved value"
  };
  const convert = (metadata) =>
    compilerDiagnosticsToLsp({ diagnostics: [{ ...diagnostic, ...metadata }] })[0];

  assert.deepEqual(convert({}).data, {
    notes: [],
    help: [],
    edits: [],
    call_frames: [],
    task_ancestry: []
  });
  assert.deepEqual(convert({ notes: ["one owner"] }).data, {
    notes: ["one owner"],
    help: [],
    edits: [],
    call_frames: [],
    task_ancestry: []
  });
  assert.deepEqual(convert({ help: ["use shared access"] }).data, {
    notes: [],
    help: ["use shared access"],
    edits: [],
    call_frames: [],
    task_ancestry: []
  });
  assert.deepEqual(convert({ edits: [{ replacement: ".clone()" }] }).data, {
    notes: [],
    help: [],
    edits: [{ replacement: ".clone()" }],
    call_frames: [],
    task_ancestry: []
  });
  assert.equal(
    Object.prototype.hasOwnProperty.call(convert({}).data, "assertion_operands"),
    false
  );
  assert.equal(
    Object.prototype.hasOwnProperty.call(
      convert({ assertion_operands: [] }).data,
      "assertion_operands"
    ),
    false
  );
});

test("compiler bridge preserves populated assertion operand metadata without rewriting it", () => {
  const assertionOperands = [
    { label: "left", type: "str", value: "actual", truncated: false },
    {
      label: "right",
      type: "str",
      value: "expected... (truncated)",
      truncated: true
    }
  ];
  const [diagnostic] = compilerDiagnosticsToLsp({
    diagnostics: [
      {
        code: "AU4001",
        severity: 1,
        line: 3,
        start_character: 4,
        end_character: 10,
        message: "values differ",
        assertion_operands: assertionOperands
      }
    ]
  });

  assert.deepEqual(diagnostic.data.assertion_operands, assertionOperands);
});

test("compiler bridge preserves populated runtime frame metadata without rewriting it", () => {
  const diagnostic = compilerDiagnosticsToLsp({
    diagnostics: [
      {
        code: "AU4003",
        severity: 1,
        line: 8,
        start_character: 17,
        end_character: 18,
        message: "list index is out of bounds",
        secondary_spans: [],
        notes: [],
        help: [],
        edits: [],
        call_frames: [
          {
            function: "worker.child",
            span: {
              file_path: "/workspace/worker.au",
              line: 2,
              start_character: 4,
              end_character: 5
            }
          },
          {
            function: "worker.outer",
            span: {
              file_path: "/workspace/worker.au",
              line: 0,
              start_character: 0,
              end_character: 1
            }
          }
        ],
        task_ancestry: [
          {
            task_function: "worker.child",
            task_entry_span: {
              file_path: "/workspace/worker.au",
              line: 2,
              start_character: 4,
              end_character: 5
            },
            parent_function: "main",
            spawn_span: {
              file_path: "/workspace/main.au",
              line: 7,
              start_character: 14,
              end_character: 15
            }
          }
        ]
      }
    ]
  }).at(0);

  assert.deepEqual(diagnostic.data, {
    notes: [],
    help: [],
    edits: [],
    call_frames: [
      {
        function: "worker.child",
        span: {
          file_path: "/workspace/worker.au",
          line: 2,
          start_character: 4,
          end_character: 5
        }
      },
      {
        function: "worker.outer",
        span: {
          file_path: "/workspace/worker.au",
          line: 0,
          start_character: 0,
          end_character: 1
        }
      }
    ],
    task_ancestry: [
      {
        task_function: "worker.child",
        task_entry_span: {
          file_path: "/workspace/worker.au",
          line: 2,
          start_character: 4,
          end_character: 5
        },
        parent_function: "main",
        spawn_span: {
          file_path: "/workspace/main.au",
          line: 7,
          start_character: 14,
          end_character: 15
        }
      }
    ]
  });
});

test("compiler bridge resolves compiler commands across env, cargo, binaries, and fallback", () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-bridge-command-"));
  const originalEnvPath = process.env.AURA_LSP_AURA_PATH;
  try {
    const fakeAura = path.join(tempRoot, binaryName());
    fs.writeFileSync(fakeAura, "");
    process.env.AURA_LSP_AURA_PATH = fakeAura;
    setWorkspaceRoots([]);
    assert.deepEqual(resolveCompilerCommand(), { cmd: fakeAura, args: [], cwd: undefined });

    delete process.env.AURA_LSP_AURA_PATH;
    const cargoRoot = path.join(tempRoot, "cargo-root");
    fs.mkdirSync(path.join(cargoRoot, "crates", "aura"), { recursive: true });
    fs.writeFileSync(path.join(cargoRoot, "Cargo.toml"), "[workspace]\n");
    fs.writeFileSync(path.join(cargoRoot, "crates", "aura", "Cargo.toml"), "[package]\nname=\"aura\"\nversion=\"0.1.0\"\n");
    setWorkspaceRoots([cargoRoot]);
    assert.deepEqual(resolveCompilerCommand(), {
      cmd: "cargo",
      args: ["run", "-q", "-p", "aura", "--"],
      cwd: cargoRoot
    });

    const binaryRoot = path.join(tempRoot, "binary-root");
    fs.mkdirSync(path.join(binaryRoot, "target", "debug"), { recursive: true });
    const debugBinary = path.join(binaryRoot, "target", "debug", binaryName());
    fs.writeFileSync(debugBinary, "");
    setWorkspaceRoots([binaryRoot]);
    assert.deepEqual(resolveCompilerCommand(), {
      cmd: debugBinary,
      args: [],
      cwd: binaryRoot
    });

    fs.rmSync(debugBinary, { force: true });
    fs.mkdirSync(path.join(binaryRoot, "target", "release"), { recursive: true });
    const releaseBinary = path.join(binaryRoot, "target", "release", binaryName());
    fs.writeFileSync(releaseBinary, "");
    assert.deepEqual(resolveCompilerCommand(), {
      cmd: releaseBinary,
      args: [],
      cwd: binaryRoot
    });

    setWorkspaceRoots([tempRoot]);
    assert.deepEqual(resolveCompilerCommand(), {
      cmd: "aura",
      args: [],
      cwd: tempRoot
    });

    setWorkspaceRoots(null);
    assert.deepEqual(resolveCompilerCommand(), {
      cmd: "aura",
      args: [],
      cwd: undefined
    });
  } finally {
    if (originalEnvPath === undefined) {
      delete process.env.AURA_LSP_AURA_PATH;
    } else {
      process.env.AURA_LSP_AURA_PATH = originalEnvPath;
    }
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge uri and command helpers handle direct utility cases", async () => {
  const encodedPath = path.join(repoRoot, "examples/modules/simple_import.au").replace(/ /g, "%20");
  assert.equal(uriToPath(`file://${encodedPath}`), path.join(repoRoot, "examples/modules/simple_import.au"));
  assert.equal(uriToPath("file:///C:/aura/examples/main.au", "win32"), "C:\\aura\\examples\\main.au");
  assert.equal(
    uriToPath("file://server/share/project/main.au", "win32"),
    "\\\\server\\share\\project\\main.au"
  );
  assert.equal(uriToPath("not-a-file-uri"), null);
  assert.equal(binaryName("win32"), "aura.exe");
  assert.equal(binaryName("linux"), "aura");

  const stdout = await runCommand(
    process.execPath,
    ["-e", "process.stdin.resume();let data='';process.stdin.on('data',chunk=>data+=chunk);process.stdin.on('end',()=>process.stdout.write(data.toUpperCase()))"],
    "aura",
    repoRoot
  );
  assert.equal(stdout, "AURA");

  await assert.rejects(
    runCommand(
      process.execPath,
      ["-e", "process.stderr.write('boom');process.exit(2)"],
      "",
      repoRoot
    ),
    /boom/
  );
});

test("compiler bridge returns null when compiler output fails or is not valid JSON", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-bridge-null-"));
  const mainPath = path.join(tempRoot, "main.au");
  const mainUri = `file://${mainPath}`;
  const originalEnvPath = process.env.AURA_LSP_AURA_PATH;
  try {
    const failingScript = path.join(tempRoot, "fail.js");
    fs.writeFileSync(failingScript, "process.stderr.write('nope');process.exit(1);");
    process.env.AURA_LSP_AURA_PATH = process.execPath;
    setWorkspaceRoots([]);
    assert.equal(
      await analyzeWithCompiler(mainUri, "def main() -> int32:\n    return 0\n"),
      null
    );
    assert.equal(
      await completeWithCompiler(mainUri, "def main() -> int32:\n    value.\n    return 0\n", 1, 10, "."),
      null
    );

    const invalidJsonScript = path.join(tempRoot, "invalid-json.js");
    fs.writeFileSync(
      invalidJsonScript,
      "process.stdin.once('data',()=>process.stdout.write('not json\\n'));"
    );
    process.env.AURA_LSP_AURA_PATH = path.join(tempRoot, "aura-invalid-json");
    fs.writeFileSync(
      process.env.AURA_LSP_AURA_PATH,
      `#!/bin/sh\nexec "${process.execPath}" "${invalidJsonScript}" "$@"\n`
    );
    fs.chmodSync(process.env.AURA_LSP_AURA_PATH, 0o755);
    assert.equal(
      await analyzeWithCompiler(mainUri, "def main() -> int32:\n    return 0\n"),
      null
    );
  } finally {
    if (originalEnvPath === undefined) {
      delete process.env.AURA_LSP_AURA_PATH;
    } else {
      process.env.AURA_LSP_AURA_PATH = originalEnvPath;
    }
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge reuses one persistent compiler process", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-bridge-persistent-"));
  const originalEnvPath = process.env.AURA_LSP_AURA_PATH;
  try {
    const counterPath = path.join(tempRoot, "starts.txt");
    const fakeCompiler = path.join(tempRoot, "aura-persistent");
    const script = path.join(tempRoot, "persistent.js");
    fs.writeFileSync(
      script,
      [
        "const fs = require('node:fs');",
        "const readline = require('node:readline');",
        `fs.appendFileSync(${JSON.stringify(counterPath)}, 'start\\n');`,
        "if (process.argv[2] !== 'lsp') process.exit(2);",
        "const lines = readline.createInterface({ input: process.stdin });",
        "lines.on('line', (line) => {",
        "  const request = JSON.parse(line);",
        "  const result = request.method === 'analyze'",
        "    ? { diagnostics: [], symbols: [], occurrences: [] }",
        "    : [{ name: 'len', kind: 'method', detail: 'len() -> intsize' }];",
        `  process.stdout.write(JSON.stringify({ id: request.id, semantic_interface_version: ${JSON.stringify(
          SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION
        )}, result }) + '\\n');`,
        "});"
      ].join("\n")
    );
    fs.writeFileSync(fakeCompiler, `#!/bin/sh\nexec "${process.execPath}" "${script}" "$@"\n`);
    fs.chmodSync(fakeCompiler, 0o755);
    process.env.AURA_LSP_AURA_PATH = fakeCompiler;
    setWorkspaceRoots([]);

    const analysis = await analyzeWithCompiler("file:///workspace/main.au", "print(1)\n");
    const completions = await completeWithCompiler(
      "file:///workspace/main.au",
      "value.\n",
      0,
      6,
      null
    );
    assert.deepEqual(analysis.diagnostics, []);
    assert.equal(completions[0].name, "len");
    assert.equal(fs.readFileSync(counterPath, "utf8").trim().split("\n").length, 1);
  } finally {
    disposeCompilerService();
    if (originalEnvPath === undefined) {
      delete process.env.AURA_LSP_AURA_PATH;
    } else {
      process.env.AURA_LSP_AURA_PATH = originalEnvPath;
    }
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("persistent compiler service sends and accepts the current semantic schema", async () => {
  assert.equal(SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION, 6);
  const script = [
    "const readline = require('node:readline');",
    "const lines = readline.createInterface({ input: process.stdin });",
    "lines.on('line', (line) => {",
    "  const request = JSON.parse(line);",
    "  process.stdout.write(JSON.stringify({",
    "    id: request.id,",
    `    semantic_interface_version: ${JSON.stringify(
      SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION
    )},`,
    "    result: { received_schema: request.semantic_interface_version }",
    "  }) + '\\n');",
    "});"
  ].join("\n");
  const service = new CompilerService({
    cmd: process.execPath,
    args: ["-e", script],
    cwd: repoRoot
  });
  assert.equal(service.responseLimitBytes, 16 * 1024 * 1024);

  assert.deepEqual(await service.request("analyze", {}), {
    received_schema: SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION
  });
  assert.equal(service.closed, false);
  service.dispose();
});

test("persistent compiler service rejects and disposes a mismatched semantic schema", async () => {
  const script = [
    "const readline = require('node:readline');",
    "const lines = readline.createInterface({ input: process.stdin });",
    "lines.on('line', (line) => {",
    "  const request = JSON.parse(line);",
    "  process.stdout.write(JSON.stringify({",
    "    id: request.id,",
    "    semantic_interface_version: 1,",
    "    result: { diagnostics: [], symbols: [], occurrences: [] }",
    "  }) + '\\n');",
    "});"
  ].join("\n");
  let invalidations = 0;
  const service = new CompilerService(
    { cmd: process.execPath, args: ["-e", script], cwd: repoRoot },
    { onSemanticSchemaMismatch: () => invalidations++ }
  );

  await assert.rejects(
    service.request("analyze", {
      path: "/virtual/main.au",
      source: "def main():\n    pass\n"
    }),
    /semantic schema mismatch.*received `1`.*expected `6`/
  );
  assert.equal(service.closed, true);
  assert.equal(invalidations, 1);
  service.handleStdout(
    `${JSON.stringify({
      id: 99,
      semantic_interface_version: 1,
      result: {}
    })}\n`
  );
  assert.equal(invalidations, 1);
  await assert.rejects(service.request("analyze", {}), /closed/);
});

test("persistent compiler service treats a missing semantic schema as incompatible", async () => {
  const script = [
    "process.stdin.once('data', (line) => {",
    "  const request = JSON.parse(line);",
    "  process.stdout.write(JSON.stringify({ id: request.id, result: {} }) + '\\n');",
    "});"
  ].join("\n");
  const service = new CompilerService({
    cmd: process.execPath,
    args: ["-e", script],
    cwd: repoRoot
  });

  await assert.rejects(service.request("analyze", {}), /received `<missing>`/);
  assert.equal(service.closed, true);
});

test("compiler schema mismatch invalidates cached function-type metadata", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-schema-mismatch-"));
  const fakeCompiler = path.join(tempRoot, "aura-schema");
  const script = path.join(tempRoot, "compiler.js");
  const originalEnvPath = process.env.AURA_LSP_AURA_PATH;
  let analyses = 0;
  let invalidations = 0;
  const cache = createDocumentStateCache(async () => ({
    stamp: ++analyses,
    function_type: analyses === 1 ? "pre-function-values" : "def(int32) -> int32"
  }));
  const document = {
    uri: "file:///workspace/main.au",
    version: 1,
    getText: () => "def main():\n    pass\n"
  };

  try {
    assert.equal(
      (await cache.get(document)).compilerAnalysis.function_type,
      "pre-function-values"
    );
    setCompilerSchemaMismatchHandler(() => {
      invalidations += 1;
      cache.invalidateAll();
    });
    fs.writeFileSync(
      script,
      [
        "const readline = require('node:readline');",
        "const lines = readline.createInterface({ input: process.stdin });",
        "lines.on('line', (line) => {",
        "  const request = JSON.parse(line);",
        "  process.stdout.write(JSON.stringify({",
        "    id: request.id,",
        "    semantic_interface_version: 1,",
        "    result: { diagnostics: [], symbols: [], occurrences: [] }",
        "  }) + '\\n');",
        "});"
      ].join("\n")
    );
    fs.writeFileSync(fakeCompiler, `#!/bin/sh\nexec "${process.execPath}" "${script}" "$@"\n`);
    fs.chmodSync(fakeCompiler, 0o755);
    process.env.AURA_LSP_AURA_PATH = fakeCompiler;
    disposeCompilerService();
    setWorkspaceRoots([]);

    assert.equal(await analyzeWithCompiler(document.uri, document.getText()), null);
    assert.equal(invalidations, 1);
    assert.equal(
      (await cache.get(document)).compilerAnalysis.function_type,
      "def(int32) -> int32"
    );
    assert.equal(analyses, 2);
  } finally {
    setCompilerSchemaMismatchHandler(null);
    disposeCompilerService();
    if (originalEnvPath === undefined) {
      delete process.env.AURA_LSP_AURA_PATH;
    } else {
      process.env.AURA_LSP_AURA_PATH = originalEnvPath;
    }
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("persistent compiler service handles errors, cancellation, and closed requests", async () => {
  const script = [
    "const readline = require('node:readline');",
    "const lines = readline.createInterface({ input: process.stdin });",
    "lines.on('line', (line) => {",
    "  const request = JSON.parse(line);",
    "  if (request.method === 'error') {",
    `    process.stdout.write(JSON.stringify({ id: request.id, semantic_interface_version: ${JSON.stringify(
      SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION
    )}, error: 'compiler boom' }) + '\\n');`,
    "  }",
    "});"
  ].join("\n");
  const command = { cmd: process.execPath, args: ["-e", script], cwd: repoRoot };
  const service = new CompilerService(command);
  let responseDisposals = 0;
  const responseToken = {
    isCancellationRequested: false,
    onCancellationRequested() {
      return { dispose: () => responseDisposals++ };
    }
  };
  await assert.rejects(service.request("error", {}, responseToken), /compiler boom/);
  assert.equal(responseDisposals, 1);
  await assert.rejects(
    service.request("ignored", {}, { isCancellationRequested: true }),
    /cancelled/
  );

  let cancel;
  let cancellationDisposals = 0;
  const cancellationToken = {
    isCancellationRequested: false,
    onCancellationRequested(handler) {
      cancel = handler;
      return { dispose: () => cancellationDisposals++ };
    }
  };
  const pending = service.request("hang", {}, cancellationToken);
  await new Promise((resolve) => setImmediate(resolve));
  cancel();
  await assert.rejects(pending, /cancelled/);
  assert.equal(cancellationDisposals, 1);
  await assert.rejects(service.request("after-close", {}), /closed/);
  service.fail(new Error("already closed"));
  service.dispose();
});

test("persistent compiler service enforces timeout and response-size limits", async () => {
  const hanging = new CompilerService(
    {
      cmd: process.execPath,
      args: ["-e", "process.stdin.resume()"],
      cwd: repoRoot
    },
    { requestTimeoutMs: 10, responseLimitBytes: 1024 }
  );
  await assert.rejects(hanging.request("analyze", {}), /timed out after 10ms/);
  hanging.dispose();

  const oversized = new CompilerService(
    {
      cmd: process.execPath,
      args: ["-e", "process.stdin.once('data',()=>process.stdout.write('x'.repeat(32)))"],
      cwd: repoRoot
    },
    { requestTimeoutMs: 1000, responseLimitBytes: 16 }
  );
  await assert.rejects(oversized.request("analyze", {}), /exceeded 16 MiB/);
  oversized.dispose();
});

test("persistent compiler service reports spawn and empty-stderr exit failures", async () => {
  const missing = new CompilerService({
    cmd: path.join(os.tmpdir(), "definitely-missing-aura-compiler"),
    args: [],
    cwd: repoRoot
  });
  await assert.rejects(missing.request("analyze", {}), /ENOENT/);
  missing.dispose();

  const exited = new CompilerService({
    cmd: process.execPath,
    args: ["-e", "process.exit(7)"],
    cwd: repoRoot
  });
  await assert.rejects(exited.request("analyze", {}), /status 7/);
  exited.dispose();
});

test("compiler bridge accepts non-file URIs when using the compiler subprocess helpers", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-bridge-memory-uri-"));
  const originalEnvPath = process.env.AURA_LSP_AURA_PATH;

  try {
    const fakeCompiler = path.join(tempRoot, "aura-fake");
    const fakeCompilerScript = path.join(tempRoot, "fake-compiler.js");
    fs.writeFileSync(
      fakeCompilerScript,
      [
        "const readline = require('node:readline');",
        "if (process.argv[2] !== 'lsp') process.exit(2);",
        "const lines = readline.createInterface({ input: process.stdin });",
        "lines.on('line', (line) => {",
        "  const request = JSON.parse(line);",
        "  const result = request.method === 'analyze'",
        "    ? { diagnostics: [], symbols: [], occurrences: [{ line: 0, start_character: 0, end_character: 3, hover: request.path, definition: null }] }",
        "    : [{ label: request.path }];",
        `  process.stdout.write(JSON.stringify({ id: request.id, semantic_interface_version: ${JSON.stringify(
          SUPPORTED_SEMANTIC_INTERFACE_SCHEMA_VERSION
        )}, result }) + '\\n');`,
        "});"
      ].join("\n")
    );
    fs.writeFileSync(fakeCompiler, `#!/bin/sh\nexec "${process.execPath}" "${fakeCompilerScript}" "$@"\n`);
    fs.chmodSync(fakeCompiler, 0o755);

    process.env.AURA_LSP_AURA_PATH = fakeCompiler;
    setWorkspaceRoots([]);

    const analysis = await analyzeWithCompiler("untitled:aura-buffer", "def main():\n    pass\n");
    assert.equal(analysis.occurrences[0].hover, "untitled:aura-buffer");

    const completions = await completeWithCompiler(
      "untitled:aura-buffer",
      "def main():\n    value.\n",
      1,
      10,
      "."
    );
    assert.equal(completions[0].label, "untitled:aura-buffer");
  } finally {
    if (originalEnvPath === undefined) {
      delete process.env.AURA_LSP_AURA_PATH;
    } else {
      process.env.AURA_LSP_AURA_PATH = originalEnvPath;
    }
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("runCommand reports exit status when stderr is empty", async () => {
  await assert.rejects(
    runCommand(process.execPath, ["-e", "process.exit(7)"], "", repoRoot),
    /status 7/
  );
});

test("compiler bridge returns machine-readable analysis for a real example", async () => {
  setWorkspaceRoots([repoRoot]);
  const analysis = await analyzeWithCompiler(pointUri, pointSource);

  assert.ok(analysis);
  assert.equal(analysis.diagnostics.length, 0);
  assert.ok(Array.isArray(analysis.symbols));
  assert.ok(Array.isArray(analysis.occurrences));
  assert.ok(analysis.symbols.some((symbol) => symbol.name === "Point"));
});

test("compiler bridge exposes extern C symbols, hover, definitions, and completions", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-ffi-"));
  const source = [
    'public extern "C" opaque class ProcessHandle',
    'public extern "C" def getpid() -> int32',
    "",
    "def main() -> int32:",
    "    return getpid()",
    ""
  ].join("\n");

  try {
    fs.writeFileSync(
      path.join(tempRoot, "Aura.toml"),
      [
        "[package]",
        'name = "ffi_lsp"',
        'version = "0.1.0"',
        'edition = "2026"',
        "allow_ffi = true",
        ""
      ].join("\n")
    );
    fs.mkdirSync(path.join(tempRoot, "src"));
    const mainPath = path.join(tempRoot, "src", "main.au");
    const mainUri = `file://${mainPath}`;
    setWorkspaceRoots([repoRoot, tempRoot]);

    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    assert.ok(
      analysis.symbols.some(
        (symbol) =>
          symbol.name === "ProcessHandle" &&
          symbol.kind === "class" &&
          symbol.detail === 'extern "C" opaque'
      )
    );
    assert.ok(
      analysis.symbols.some(
        (symbol) =>
          symbol.name === "getpid" &&
          symbol.kind === "function" &&
          symbol.detail === 'extern "C" -> int32'
      )
    );

    const callCharacter = source.split("\n")[4].indexOf("getpid") + 2;
    assert.match(
      compilerHoverAtPosition(analysis, 4, callCharacter)?.value || "",
      /extern "C" function getpid\(\) -> int32/
    );
    assert.deepEqual(
      compilerDefinitionAtPosition(mainUri, analysis, 4, callCharacter)?.range,
      {
        start: { line: 1, character: 22 },
        end: { line: 1, character: 28 }
      }
    );

    const completions = await completeWithCompiler(mainUri, source, 4, 4, null);
    const getpid = completions.find((item) => item.name === "getpid");
    assert.equal(getpid?.kind, "function");
    assert.match(getpid?.detail || "", /extern "C".*int32/);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge analyzes and completes inside continued delimiters", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-continuation-"));
  const source = [
    "def add(left: int32, right: int32) -> int32:",
    "    return left + right",
    "",
    "def main() -> int32:",
    "    base: int32 = 40",
    "    result = add(",
    "        base,",
    "        2",
    "    )",
    "    return result",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);

    const hover = compilerHoverAtPosition(analysis, 6, 9);
    assert.deepEqual(hover, {
      value: "```aura\nbinding base: int32\n```",
      range: {
        start: { line: 6, character: 8 },
        end: { line: 6, character: 12 }
      }
    });

    const definition = compilerDefinitionAtPosition(mainUri, analysis, 6, 9);
    assert.equal(
      definition?.uri,
      `file://${path.join(fs.realpathSync(tempRoot), "main.au")}`
    );
    assert.deepEqual(definition?.range, {
      start: { line: 4, character: 4 },
      end: { line: 4, character: 8 }
    });

    const completions = await completeWithCompiler(mainUri, source, 6, 12, null);
    assert.ok(
      completions.some(
        (completion) => completion.name === "add" && completion.kind === "function"
      )
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge recovers member completion when an earlier line owns the open delimiter", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-multiline-dangling-"));
  const source = [
    "def main() -> int32:",
    "    text = \"hello\"",
    "    print(",
    "        text."
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.ok(analysis.symbols.length > 0);
    assert.ok(analysis.occurrences.length > 0);

    const completions = await completeWithCompiler(mainUri, source, 3, 13, ".");
    assert.ok(completions);
    assert.ok(completions.some((item) => item.name === "len"));
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge maps mismatched delimiter diagnostics to the opening delimiter", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-delimiter-error-"));
  const source = [
    "def main():",
    "    values = [",
    "        1,",
    "        2)",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 1);
    assert.equal(analysis.diagnostics[0].code, "AU1001");
    assert.match(
      analysis.diagnostics[0].message,
      /mismatched closing delimiter `\)`; expected `]`/
    );
    assert.deepEqual(analysis.diagnostics[0].secondary_spans, [
      {
        line: 1,
        start_character: 13,
        end_character: 14,
        label: "opening delimiter `[` is here"
      }
    ]);

    const [diagnostic] = compilerDiagnosticsToLsp(analysis, mainUri);
    assert.deepEqual(diagnostic.range, {
      start: { line: 3, character: 9 },
      end: { line: 3, character: 10 }
    });
    assert.deepEqual(diagnostic.relatedInformation, [
      {
        location: {
          uri: mainUri,
          range: {
            start: { line: 1, character: 13 },
            end: { line: 1, character: 14 }
          }
        },
        message: "opening delimiter `[` is here"
      }
    ]);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge keeps unclosed-delimiter EOF ranges inside a document without a final newline", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-unclosed-delimiter-"));
  const source = ["def main():", "    print("].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 1);
    assert.equal(analysis.diagnostics[0].code, "AU1001");
    assert.match(analysis.diagnostics[0].message, /unclosed delimiter `\(`/);

    const [diagnostic] = compilerDiagnosticsToLsp(analysis, mainUri);
    assert.deepEqual(diagnostic.range, {
      start: { line: 1, character: 10 },
      end: { line: 1, character: 11 }
    });
    assert.equal(diagnostic.relatedInformation?.[0]?.location.uri, mainUri);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves assert operand occurrences and keyword completion", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-assert-"));
  const source = [
    "def main():",
    "    ready = true",
    "    message = \"ready assertion\"",
    "    assert ready, message",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    const conditionUse = analysis.occurrences.find(
      (occurrence) =>
        occurrence.line === 3 &&
        occurrence.start_character === 11 &&
        occurrence.end_character === 16
    );
    const messageUse = analysis.occurrences.find(
      (occurrence) =>
        occurrence.line === 3 &&
        occurrence.start_character === 18 &&
        occurrence.end_character === 25
    );
    assert.ok(conditionUse, "assert condition should expose its identifier use");
    assert.ok(messageUse, "assert message should expose its identifier use");
    assert.equal(conditionUse.hover, "```aura\nbinding ready: bool\n```");
    assert.equal(messageUse.hover, "```aura\nbinding message: str\n```");
    assert.equal(conditionUse.definition?.line, 1);
    assert.equal(messageUse.definition?.line, 2);

    const completions = await completeWithCompiler(mainUri, source, 3, 10, null);
    assert.ok(
      completions.some(
        (completion) => completion.name === "assert" && completion.kind === "keyword"
      ),
      "compiler completion should include the assert keyword"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes yield_now completion and hover metadata", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-yield-now-"));
  const source = ["def main():", "    yield_now()", ""].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    const hover = compilerHoverAtPosition(analysis, 1, 5);
    assert.ok(hover, "yield_now call should expose builtin hover");
    assert.ok(
      hover.value.startsWith("```aura\nyield_now() -> None\n```"),
      `yield_now hover should expose its signature, found ${hover.value}`
    );

    const completions = await completeWithCompiler(mainUri, source, 1, 4, null);
    assert.ok(completions);
    assert.deepEqual(
      completions.find((completion) => completion.name === "yield_now"),
      {
        name: "yield_now",
        kind: "function",
        detail: "yield_now() -> None"
      }
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes guarded TaskGroup stack override completion and hover", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-task-stack-"));
  const source = [
    "def produce(value: int64) -> int64:",
    "    return value",
    "def announce(value: int64):",
    "    print(value)",
    "def main():",
    "    with group = TaskGroup():",
    "        task = group.start_with_stack(262144, produce, 7)",
    "        group.start_soon_with_stack(524288, announce, 8)",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    for (const [line, name, signature] of [
      [6, "start_with_stack", "start_with_stack(bytes: int64, function, own ...) -> Task[T]"],
      [
        7,
        "start_soon_with_stack",
        "start_soon_with_stack(bytes: int64, function, own ...) -> None"
      ]
    ]) {
      const character = source.split("\n")[line].indexOf(name) + 1;
      const hover = compilerHoverAtPosition(analysis, line, character);
      assert.ok(hover, `${name} should expose builtin hover`);
      assert.ok(
        hover.value.includes(signature),
        `${name} hover should expose ${signature}, found ${hover.value}`
      );
      assert.ok(
        hover.value.includes(
          "The 256 KiB minimum is opt-in for a measured shallow task; ordinary starts use the safe 512 KiB default."
        ),
        `${name} hover should distinguish the opt-in minimum from the safe default`
      );
    }

    const completionSource = [
      "def main():",
      "    with group = TaskGroup():",
      "        group.",
      ""
    ].join("\n");
    const completions = await completeWithCompiler(
      mainUri,
      completionSource,
      2,
      completionSource.split("\n")[2].length,
      "."
    );
    assert.ok(completions);
    const details = new Map(completions.map((item) => [item.name, item.detail]));
    assert.equal(
      details.get("start_with_stack"),
      "start_with_stack(bytes: int64, function, own ...) -> Task[T]"
    );
    assert.equal(
      details.get("start_soon_with_stack"),
      "start_soon_with_stack(bytes: int64, function, own ...) -> None"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes invalid assert diagnostics at the keyword", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-invalid-assert-"));
  const source = ["def main():", "    assert 1", ""].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 1);
    assert.deepEqual(analysis.diagnostics[0].call_frames, []);
    assert.deepEqual(analysis.diagnostics[0].task_ancestry, []);
    const [diagnostic] = compilerDiagnosticsToLsp(analysis, mainUri);
    assert.equal(diagnostic.code, "AU2002");
    assert.equal(
      diagnostic.message,
      "`assert` condition must have type `bool`, found `int64`"
    );
    assert.equal(diagnostic.source, "aura-compiler");
    assert.equal(diagnostic.severity, 1);
    assert.deepEqual(diagnostic.range, {
      start: { line: 1, character: 4 },
      end: { line: 1, character: 5 }
    });
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves len and str builtin calls", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-len-str-"));
  const source = [
    "def report(hosts: list[str]):",
    "    print(len(hosts))",
    "    print(str(hosts))",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    const operand = analysis.occurrences.find(
      (candidate) => candidate.line === 1 && candidate.start_character === 14
    );
    assert.ok(operand, "missing len operand occurrence");
    assert.ok(operand.hover.includes("param hosts: list[str]"));

    const invalid = await analyzeWithCompiler(
      mainUri,
      ["def main():", "    print(len(1))", ""].join("\n")
    );
    assert.ok(invalid);
    assert.equal(invalid.diagnostics.length, 1);
    const [diagnostic] = compilerDiagnosticsToLsp(invalid, mainUri);
    assert.equal(diagnostic.code, "AU2002");
    assert.equal(
      diagnostic.message,
      "`len(...)` expects a value with a `len()` member, found `int64`"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes all public length members as int64", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-length-types-"));
  const source = [
    "def lengths_match(text: str, values: list[int32], counts: dict[str, int32], seen: set[int32]) -> bool:",
    "    text_length = text.len()",
    "    text_bytes = text.byte_len()",
    "    vector_length = values.len()",
    "    map_length = counts.len()",
    "    set_length = seen.len()",
    "    text_matches = len(text) == text_length",
    "    vector_matches = len(values) == vector_length",
    "    map_matches = len(counts) == map_length",
    "    set_matches = len(seen) == set_length",
    "    byte_count_is_wide = text_bytes >= text_length",
    "    return text_matches and vector_matches and map_matches and set_matches and byte_count_is_wide",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(
      analysis.diagnostics,
      [],
      "free len(...) and the corresponding member length must have the same int64 type"
    );

    const lines = source.split("\n");
    for (const [line, member, receiver] of [
      [1, "len", "str"],
      [2, "byte_len", "str"],
      [3, "len", "list"],
      [4, "len", "dict"],
      [5, "len", "set"]
    ]) {
      const character = lines[line].indexOf(`.${member}`) + 1;
      const hover = compilerHoverAtPosition(analysis, line, character);
      assert.ok(hover, `missing ${receiver}.${member}() hover`);
      assert.ok(
        hover.value.startsWith(`\`\`\`aura\n${member}() -> int64\n\`\`\``),
        `${receiver}.${member}() hover must expose an int64 result, found ${hover.value}`
      );
    }

    const builtinHover = compilerHoverAtPosition(
      analysis,
      6,
      lines[6].indexOf("len(") + 1
    );
    assert.ok(builtinHover, "missing len(...) builtin hover");
    assert.ok(
      builtinHover.value.startsWith(
        "```aura\nlen(value: str|list[T]|dict[K, V]|set[T]) -> int64\n```"
      ),
      `len(...) hover must expose an int64 result, found ${builtinHover.value}`
    );

    for (const [line, name] of [
      [6, "text_length"],
      [7, "vector_length"],
      [8, "map_length"],
      [9, "set_length"],
      [10, "text_bytes"]
    ]) {
      const character = lines[line].lastIndexOf(name) + 1;
      assert.equal(
        compilerHoverAtPosition(analysis, line, character)?.value,
        `\`\`\`aura\nbinding ${name}: int64\n\`\`\``,
        `${name} must retain the inferred int64 member-result type`
      );
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves enumerate and zip loop operands", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-lockstep-"));
  const source = [
    "def report(hosts: list[str], ports: list[int32]):",
    "    for index, host in enumerate(hosts):",
    "        print(index)",
    "    for host, port in zip(hosts, ports):",
    "        print(port)",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    const hosts = analysis.occurrences.find(
      (candidate) => candidate.line === 1 && candidate.start_character === 33
    );
    assert.ok(hosts, "missing enumerate operand occurrence");
    assert.ok(hosts.hover.includes("param hosts: list[str]"));

    const invalid = await analyzeWithCompiler(
      mainUri,
      [
        "def main():",
        "    for index, value in enumerate(range(3)):",
        "        print(index)",
        ""
      ].join("\n")
    );
    assert.ok(invalid);
    assert.equal(invalid.diagnostics.length, 1);
    const [diagnostic] = compilerDiagnosticsToLsp(invalid, mainUri);
    assert.equal(diagnostic.code, "AU2002");
    assert.equal(
      diagnostic.message,
      "`enumerate` requires a `list[T]` or `set[T]` iterable, found `Range`"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves membership and comparison chain operands", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-membership-"));
  const source = [
    "def probe(ports: list[int32], port: int32, low: int32, high: int32) -> bool:",
    "    return port in ports and low <= port < high",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    for (const [start, end, hover] of [
      [11, 15, "param port: int32"],
      [19, 24, "param ports: list[int32]"],
      [29, 32, "param low: int32"],
      [36, 40, "param port: int32"],
      [43, 47, "param high: int32"]
    ]) {
      const occurrence = analysis.occurrences.find(
        (candidate) =>
          candidate.line === 1 &&
          candidate.start_character === start &&
          candidate.end_character === end
      );
      assert.ok(occurrence, `missing membership occurrence at ${start}-${end}`);
      assert.ok(occurrence.hover.includes(hover));
    }

    const invalid = await analyzeWithCompiler(
      mainUri,
      ["def main():", "    print(1 in 5)", ""].join("\n")
    );
    assert.ok(invalid);
    assert.equal(invalid.diagnostics.length, 1);
    const [diagnostic] = compilerDiagnosticsToLsp(invalid, mainUri);
    assert.equal(diagnostic.code, "AU2003");
    assert.equal(
      diagnostic.message,
      "`in` requires a `list[T]`, `set[T]`, `dict[K, V]`, or `str` container, found `int64`"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves conditional operands and bool diagnostics", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-conditional-"));
  const source = [
    "def choose(ready: bool, left: str, right: str) -> str:",
    "    return left.clone() if ready else right.clone()",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    for (const [start, end, hover] of [
      [11, 15, "param left: str"],
      [27, 32, "param ready: bool"],
      [38, 43, "param right: str"]
    ]) {
      const occurrence = analysis.occurrences.find(
        (candidate) =>
          candidate.line === 1 &&
          candidate.start_character === start &&
          candidate.end_character === end
      );
      assert.ok(occurrence, `missing conditional occurrence at ${start}-${end}`);
      assert.ok(occurrence.hover.includes(hover));
    }

    const invalid = await analyzeWithCompiler(
      mainUri,
      ["def main():", "    value = \"yes\" if 1 else \"no\"", ""].join("\n")
    );
    assert.ok(invalid);
    assert.equal(invalid.diagnostics.length, 1);
    const [diagnostic] = compilerDiagnosticsToLsp(invalid, mainUri);
    assert.equal(diagnostic.code, "AU2002");
    assert.equal(
      diagnostic.message,
      "conditional expression condition must have type `bool`, found `int64`"
    );
    assert.deepEqual(diagnostic.range, {
      start: { line: 1, character: 21 },
      end: { line: 1, character: 22 }
    });
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves real ownership provenance, help, and safe edits", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-ownership-diag-"));
  const source = "def take(value: str) -> str:\n    return value\n";

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 1);
    const diagnostics = compilerDiagnosticsToLsp(analysis, mainUri);
    assert.equal(diagnostics[0].code, "AU3002");
    assert.deepEqual(diagnostics[0].range, {
      start: { line: 1, character: 11 },
      end: { line: 1, character: 12 }
    });
    assert.deepEqual(diagnostics[0].relatedInformation, [
      {
        location: {
          uri: mainUri,
          range: {
            start: { line: 0, character: 9 },
            end: { line: 0, character: 10 }
          }
        },
        message: "parameter `value` is borrowed here"
      }
    ]);
    assert.deepEqual(diagnostics[0].data.help, [
      "declare the parameter as `own str` when the function should consume it, or call `.clone()` to consume an independent copy"
    ]);
    assert.deepEqual(diagnostics[0].data.edits, [
      {
        line: 1,
        start_character: 16,
        end_character: 16,
        replacement: ".clone()",
        applicability: "machine-applicable"
      }
    ]);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves nested Transfer diagnostics and provenance", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-transfer-diag-"));
  const source = [
    "import random",
    "",
    "class Holder:",
    "    label: str",
    "    generator: random.Rng",
    "",
    "def consume(holder: own Holder):",
    "    print(holder.label)",
    "",
    "def launch(group: TaskGroup, holder: own Holder):",
    "    group.start_soon(consume, holder)",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 1);
    const [diagnostic] = compilerDiagnosticsToLsp(analysis, mainUri);
    assert.equal(diagnostic.code, "AU3008");
    assert.equal(
      diagnostic.message,
      "task argument `holder` cannot cross a task boundary because field `generator` of `Holder` -> `random.Rng` is a stateful generator and is not Transfer"
    );
    assert.deepEqual(diagnostic.range, {
      start: { line: 10, character: 30 },
      end: { line: 10, character: 31 }
    });
    assert.deepEqual(diagnostic.relatedInformation, [
      {
        location: {
          uri: mainUri,
          range: {
            start: { line: 6, character: 12 },
            end: { line: 6, character: 13 }
          }
        },
        message: "task parameter `holder` is declared here"
      }
    ]);
    assert.deepEqual(diagnostic.data.help, [
      "send owned data made only from Transfer components; keep capabilities and host resources on their owning worker"
    ]);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves single-consumer duplication diagnostics", async () => {
  const tempRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), "aura-lsp-task-result-diag-")
  );
  const source = [
    "def duplicate(tasks: list[Task[str]]) -> list[Task[str]]:",
    "    return tasks.copy()",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 1);
    const [diagnostic] = compilerDiagnosticsToLsp(analysis, mainUri);
    assert.equal(diagnostic.code, "AU3009");
    assert.equal(
      diagnostic.message,
      "cannot use `list.copy` because duplicating `list[Task[str]]` would create a second observation right for non-repeatable task result `str`"
    );
    assert.deepEqual(diagnostic.range, {
      start: { line: 1, character: 17 },
      end: { line: 1, character: 18 }
    });
    assert.deepEqual(diagnostic.data.help, [
      "transfer the unique Task handle instead; only copy-result tasks and synchronized Queue or repeatable Task results may have multiple observers"
    ]);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves single-consumer Task alias provenance", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-task-alias-"));
  const source = [
    "def make_text() -> str:",
    '    return "once"',
    "",
    "def main() -> int32:",
    "    with TaskGroup() as group:",
    "        task = group.start(make_text)",
    "        alias = task",
    "        print(task.result())",
    "        print(alias.result_or_none())",
    "    return 0",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 1);
    const [diagnostic] = compilerDiagnosticsToLsp(analysis, mainUri);
    assert.equal(diagnostic.code, "AU3001");
    assert.equal(diagnostic.message, "use of moved value `task`");
    assert.deepEqual(diagnostic.range, {
      start: { line: 7, character: 14 },
      end: { line: 7, character: 15 }
    });
    assert.deepEqual(diagnostic.relatedInformation, [
      {
        location: {
          uri: mainUri,
          range: {
            start: { line: 6, character: 16 },
            end: { line: 6, character: 17 }
          }
        },
        message: "value moved here"
      }
    ]);
    assert.deepEqual(diagnostic.data.help, [
      "pass shared access when ownership is not needed, or transfer this non-cloneable value only once"
    ]);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge teaches conditional task consumption and Queue Transfer", async () => {
  const tempRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), "aura-lsp-task-transfer-hover-")
  );
  const source = [
    "def inspect(task: own Task[str], tasks: own list[Task[str]], queue: Queue[str]):",
    "    print(task.result_or_none())",
    "    print(wait_all(tasks))",
    '    queue.put("hello")',
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    const hovers = analysis.occurrences.map((occurrence) => occurrence.hover);
    assert.ok(
      hovers.some(
        (hover) =>
          hover.includes("result_or_none(timeout: Duration = ...) -> Option[T]") &&
          hover.includes("consumes the unique `Task[T]` observation right") &&
          hover.includes("`Task[T]` is copyable only when `T` is repeatable")
      )
    );
    assert.ok(
      hovers.some(
        (hover) =>
          hover.includes("wait_all(tasks: list[Task[T]]") &&
          hover.includes("consumes the whole `list[Task[T]]` observation right") &&
          hover.includes("repeatable `T` leaves the list reusable")
      )
    );
    assert.ok(
      hovers.some(
        (hover) =>
          hover.includes("put(value: own T") &&
          hover.includes("Queue payload type `T` must be Transfer") &&
          !hover.includes("multiple workers")
      )
    );

    const globalCompletions = await completeWithCompiler(
      mainUri,
      "def main():\n    yield_now()\n",
      1,
      4,
      null
    );
    assert.ok(globalCompletions);
    for (const name of ["wait_any", "wait_all"]) {
      const item = globalCompletions.find((completion) => completion.name === name);
      assert.ok(item, `missing ${name} completion`);
      assert.ok(
        item.detail.includes("consumes tasks when T is non-repeatable"),
        `${name} completion must expose whole-list conditional consumption`
      );
    }

    const taskCompletions = await completeWithCompiler(
      mainUri,
      "def inspect(task: own Task[str]):\n    task.\n",
      1,
      9,
      "."
    );
    assert.ok(taskCompletions);
    for (const name of ["result", "result_or_none", "result_or"]) {
      const item = taskCompletions.find((completion) => completion.name === name);
      assert.ok(item, `missing Task.${name} completion`);
      assert.ok(
        item.detail.includes("consumes Task[T] when T is non-repeatable"),
        `Task.${name} completion must expose conditional consumption`
      );
    }

    const queueCompletions = await completeWithCompiler(
      mainUri,
      "def inspect(queue: Queue[str]):\n    queue.\n",
      1,
      10,
      "."
    );
    assert.ok(queueCompletions);
    for (const name of ["put", "try_put", "get", "get_or_none", "get_or"]) {
      const item = queueCompletions.find((completion) => completion.name === name);
      assert.ok(item, `missing Queue.${name} completion`);
      assert.ok(
        item.detail.includes("T must be Transfer"),
        `Queue.${name} completion must expose the structural boundary`
      );
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves Queue and Range iteration carve-out diagnostics", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-iteration-capability-"));
  const queueMessage =
    "Queue iteration receives values; each received item is already owned by the loop binding, and the Queue handle is a copy value, so ownership modifiers have nothing to modify; use the bare form `for item in queue:`";
  const rangeMessage =
    "Range iteration yields copy `int64` values, so ownership modifiers have nothing to modify or transfer; use the bare form `for item in range(...):`";
  const cases = [
    {
      name: "queue-mut",
      source: "def main():\n    queue = Queue[int32]()\n    for item in mut queue:\n        print(item)\n",
      message: queueMessage
    },
    {
      name: "queue-own",
      source: "def main():\n    queue = Queue[int32]()\n    for item in own queue:\n        print(item)\n",
      message: queueMessage
    },
    {
      name: "range-mut",
      source: "def main():\n    for item in mut range(0, 3):\n        print(item)\n",
      message: rangeMessage
    },
    {
      name: "range-own",
      source: "def main():\n    for item in own range(0, 3):\n        print(item)\n",
      message: rangeMessage
    }
  ];

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    for (const entry of cases) {
      const analysis = await analyzeWithCompiler(mainUri, entry.source);
      assert.ok(analysis, `${entry.name} should return compiler analysis`);
      assert.equal(analysis.diagnostics.length, 1, entry.name);
      assert.equal(analysis.diagnostics[0].code, "AU3004", entry.name);
      assert.equal(analysis.diagnostics[0].message, entry.message, entry.name);

      const [diagnostic] = compilerDiagnosticsToLsp(analysis, mainUri);
      assert.equal(diagnostic.code, "AU3004", entry.name);
      assert.equal(diagnostic.source, "aura-compiler", entry.name);
      assert.equal(diagnostic.message, entry.message, entry.name);
    }

    const bareRange = await analyzeWithCompiler(
      mainUri,
      "def main():\n    for item in range(0, 3):\n        print(item)\n"
    );
    assert.ok(bareRange);
    assert.deepEqual(bareRange.diagnostics, []);

    const bareQueue = await analyzeWithCompiler(
      mainUri,
      "def main():\n    queue = Queue[int32]()\n    for item in queue:\n        print(item)\n"
    );
    assert.ok(bareQueue);
    assert.deepEqual(bareQueue.diagnostics, []);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge returns member completions from the compiler", async () => {
  setWorkspaceRoots([repoRoot]);
  const lineIndex = pointSource.split("\n").findIndex((line) => line.includes("a.x"));
  const lineText = pointSource.split("\n")[lineIndex];
  const character = lineText.indexOf(".") + 1;

  const completions = await completeWithCompiler(pointUri, pointSource, lineIndex, character, ".");

  assert.ok(completions);
  const names = completions.map((item) => item.name).sort();
  assert.deepEqual(names, ["x", "y"]);
});

test("compiler bridge preserves the integer true-division teaching diagnostic", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-int-division-"));
  const message =
    "integer `/` is not supported; use `//` for floor division, or call `.to_float()` on both operands for true division";
  const cases = [
    {
      name: "binary division",
      source: [
        "def main() -> int32:",
        "    left: int32 = 7",
        "    right: int32 = 2",
        "    result = left / right",
        "    print(result)",
        "    return 0",
        ""
      ].join("\n")
    },
    {
      name: "augmented division",
      source: [
        "def main() -> int32:",
        "    mut value: int32 = 7",
        "    value /= 2",
        "    return value",
        ""
      ].join("\n")
    }
  ];

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    for (const entry of cases) {
      const mainPath = path.join(tempRoot, `${entry.name.replaceAll(" ", "-")}.au`);
      const analysis = await analyzeWithCompiler(`file://${mainPath}`, entry.source);

      assert.ok(analysis, `${entry.name} should return compiler analysis`);
      assert.equal(analysis.diagnostics.length, 1, entry.name);
      assert.equal(analysis.diagnostics[0].message, message, entry.name);
      assert.equal(
        compilerDiagnosticsToLsp(analysis)[0].message,
        message,
        `${entry.name} LSP conversion`
      );
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge accepts chained comparisons", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-chained-comparison-"));
  const source = [
    "def main():",
    "    if 1 < 2 < 3:",
    "        pass",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const analysis = await analyzeWithCompiler(`file://${mainPath}`, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);

    const mismatched = await analyzeWithCompiler(
      `file://${mainPath}`,
      ["def main():", "    if 1 < 2 < true:", "        pass", ""].join("\n")
    );
    assert.ok(mismatched);
    assert.equal(mismatched.diagnostics.length, 1);
    const diagnostic = compilerDiagnosticsToLsp(mismatched)[0];
    assert.ok(
      diagnostic.message.includes("binary operator operands must match"),
      diagnostic.message
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves indexed non-copy ownership diagnostic codes", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-index-ownership-"));
  const cases = [
    {
      code: "AU3005",
      source: [
        "def main():",
        "    values: list[str] = [\"one\"]",
        "    value: str = values[0]",
        ""
      ].join("\n")
    },
    {
      code: "AU3006",
      source: [
        "def main():",
        "    mut values: list[str] = [\"one\"]",
        "    values[0] += \"two\"",
        ""
      ].join("\n")
    }
  ];

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    for (const entry of cases) {
      const mainPath = path.join(tempRoot, `${entry.code}.au`);
      const mainUri = `file://${mainPath}`;
      const analysis = await analyzeWithCompiler(mainUri, entry.source);

      assert.ok(analysis, `${entry.code} should return compiler analysis`);
      assert.equal(analysis.diagnostics.length, 1, entry.code);
      assert.equal(analysis.diagnostics[0].code, entry.code);
      assert.equal(compilerDiagnosticsToLsp(analysis, mainUri)[0].code, entry.code);
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge propagates clone-safety-aware indexed read guidance", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-index-clone-safety-"));
  const cases = [
    {
      name: "clone_safe_vector",
      source: [
        "def main():",
        "    values: list[str] = [\"one\"]",
        "    value: str = values[0]",
        ""
      ].join("\n"),
      message:
        "cannot implicitly copy `str` out of a list index; use `get(index)` for an explicit cloned read instead"
    },
    {
      name: "rng_vector",
      source: [
        "import random",
        "",
        "def main():",
        "    mut generators = list[random.Rng]()",
        "    generators.append(random.Rng(seed=1))",
        "    chosen = generators[0]",
        ""
      ].join("\n"),
      message:
        "cannot implicitly copy `random.Rng` out of a list index; `get(index)` cannot clone it because `random.Rng` is directly non-cloneable, so use `pop(index)` to transfer ownership instead"
    },
    {
      name: "generic_map",
      source: [
        "def lookup[V](values: dict[str, V], key: str) -> V:",
        "    return values[key]",
        "",
        "def main():",
        "    print(\"ok\")",
        ""
      ].join("\n"),
      message:
        "cannot implicitly copy `V` out of a dict index; `get(key)` requires a clone-safe `V`, or use `remove(key)` to transfer ownership"
    }
  ];

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    for (const entry of cases) {
      const mainPath = path.join(tempRoot, `${entry.name}.au`);
      const mainUri = `file://${mainPath}`;
      const analysis = await analyzeWithCompiler(mainUri, entry.source);

      assert.ok(analysis, `${entry.name} should return compiler analysis`);
      assert.equal(analysis.diagnostics.length, 1, entry.name);
      assert.equal(analysis.diagnostics[0].code, "AU3005", entry.name);
      assert.equal(analysis.diagnostics[0].message, entry.message, entry.name);

      const [diagnostic] = compilerDiagnosticsToLsp(analysis, mainUri);
      assert.equal(diagnostic.code, "AU3005", entry.name);
      assert.equal(diagnostic.source, "aura-compiler", entry.name);
      assert.equal(diagnostic.message, entry.message, entry.name);
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge reports typed self with the receiver-forms diagnostic", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-typed-self-"));
  const source = [
    "class Counter:",
    "    def read(self: Counter) -> int32:",
    "        return 0",
    ""
  ].join("\n");
  const message =
    "`self: Type` is not a method receiver; use `self` for shared access, `own self` to consume, or `mut self` to mutate";

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const analysis = await analyzeWithCompiler(`file://${mainPath}`, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 1);
    assert.equal(analysis.diagnostics[0].message, message);
    assert.deepEqual(
      compilerDiagnosticsToLsp(analysis)[0].range,
      {
        start: { line: 1, character: 13 },
        end: { line: 1, character: 14 }
      }
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves canonical receiver contracts in hover and completion", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-receiver-contracts-"));
  const source = [
    "class Modes:",
    "    value: int32",
    "    def read(self) -> int32:",
    "        return self.value",
    "    def explicit(self) -> int32:",
    "        return self.value",
    "    def take(own self) -> int32:",
    "        return self.value",
    "    def bump(mut self):",
    "        self.value += 1",
    "",
    "def main() -> int32:",
    "    mut value = Modes(value=1)",
    "    print(value.read())",
    "    print(value.explicit())",
    "    value.bump()",
    "    print(Modes(value=3).take())",
    "    return 0",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 0);
    for (const signature of [
      "method read(self) -> int32",
      "method explicit(self) -> int32",
      "method take(own self) -> int32",
      "method bump(mut self) -> None"
    ]) {
      assert.ok(
        analysis.occurrences.some((occurrence) => occurrence.hover.includes(signature)),
        `missing hover signature: ${signature}`
      );
    }

    const completionSource = source.replace("    return 0\n", "    value.\n    return 0\n");
    const lineIndex = completionSource.split("\n").findIndex((line) => line.trim() === "value.");
    const character = completionSource.split("\n")[lineIndex].indexOf(".") + 1;
    const completions = await completeWithCompiler(
      mainUri,
      completionSource,
      lineIndex,
      character,
      "."
    );
    const details = new Map(completions.map((item) => [item.name, item.detail]));

    assert.equal(details.get("read"), "read(self) -> int32");
    assert.equal(details.get("explicit"), "explicit(self) -> int32");
    assert.equal(details.get("take"), "take(own self) -> int32");
    assert.equal(details.get("bump"), "bump(mut self) -> None");
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves ordinary parameter ownership in hover and diagnostics", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-param-contracts-"));
  const source = [
    "def inspect(value: str):",
    "    print(value)",
    "def consume(value: own str = \"owned fallback\"):",
    "    print(value)",
    "def explicit(value: str = \"fallback\"):",
    "    print(value)",
    "def mutate(value: mut str):",
    "    pass",
    "def main():",
    "    mut text = \"aura\"",
    "    inspect(text)",
    "    consume()",
    "    consume(text.clone())",
    "    explicit()",
    "    mutate(text)",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 0);
    for (const signature of [
      "function inspect(value: str) -> None",
      "function consume(value: own str = ...) -> None",
      "function explicit(value: str = ...) -> None",
      "function mutate(value: mut str) -> None"
    ]) {
      assert.ok(
        analysis.occurrences.some((occurrence) => occurrence.hover.includes(signature)),
        `missing hover signature: ${signature}`
      );
    }

    const invalid = [
      "def lost(value: mut str = \"fallback\"):",
      "    pass",
      ""
    ].join("\n");
    const invalidAnalysis = await analyzeWithCompiler(mainUri, invalid);
    assert.equal(invalidAnalysis.diagnostics.length, 1);
    assert.equal(
      invalidAnalysis.diagnostics[0].message,
      "`mut` parameter `value` cannot have a default: the default creates a caller-invisible temporary, so mutations through it would be silently lost; require the caller to pass a value, or take the parameter as `own T` and return the result"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes capture-free function values and rejects method values", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-function-values-"));
  const source = [
    "def double(value: int32) -> int32:",
    "    return value * 2",
    "def apply(transform: def(int32) -> int32, value: int32) -> int32:",
    "    return transform(value)",
    "def offset(value: int32 = 10) -> int32:",
    "    return value + 1",
    "def take(value: own str) -> str:",
    "    return value",
    "def apply_owned(callback: def(own str) -> str, value: own str) -> str:",
    "    return callback(value)",
    "def main() -> int32:",
    "    selected = double",
    "    known_offset = offset",
    "    consume: def(own str) -> str = take",
    "    print(apply(selected, 21))",
    "    print(apply_owned(consume, \"owned\"))",
    "    print(known_offset())",
    "    print(known_offset(value=20))",
    "    return 0",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    assert.ok(
      analysis.occurrences.some((occurrence) =>
        occurrence.hover.includes("binding selected: def(int32) -> int32")
      )
    );
    assert.ok(
      analysis.occurrences.some((occurrence) =>
        occurrence.hover.includes("binding known_offset: def(int32) -> int32")
      )
    );
    assert.ok(
      analysis.occurrences.some((occurrence) =>
        occurrence.hover.includes(
          "function apply(transform: def(int32) -> int32, value: int32) -> int32"
        )
      )
    );
    const functionReference = analysis.occurrences.find(
      (occurrence) =>
        occurrence.line === 11 &&
        occurrence.hover.includes("function double(value: int32) -> int32")
    );
    assert.ok(functionReference);
    assert.equal(functionReference.definition?.line, 0);
    assert.ok(
      analysis.occurrences.some((occurrence) =>
        occurrence.hover.includes(
          "function apply_owned(callback: def(own str) -> str, value: own str) -> str"
        )
      )
    );

    const invalidMethodValue = [
      "class Counter:",
      "    value: int32",
      "    def read(self) -> int32:",
      "        return self.value",
      "def main() -> int32:",
      "    counter = Counter(value=1)",
      "    read = counter.read",
      "    return read()",
      ""
    ].join("\n");
    const invalidAnalysis = await analyzeWithCompiler(mainUri, invalidMethodValue);
    assert.equal(invalidAnalysis.diagnostics.length, 1);
    assert.equal(invalidAnalysis.diagnostics[0].code, "AU2005");
    assert.match(
      invalidAnalysis.diagnostics[0].message,
      /method values are not supported/
    );

    const invalidAssociatedMethodValue = [
      "class Math:",
      "    def double(value: int32) -> int32:",
      "        return value * 2",
      "def main() -> int32:",
      "    callback = Math.double",
      "    return callback(21)",
      ""
    ].join("\n");
    const associatedAnalysis = await analyzeWithCompiler(
      mainUri,
      invalidAssociatedMethodValue
    );
    assert.equal(associatedAnalysis.diagnostics.length, 1);
    assert.equal(associatedAnalysis.diagnostics[0].code, "AU2005");
    assert.match(
      associatedAnalysis.diagnostics[0].message,
      /method values are not supported/
    );

    const invalidCapability = [
      "def consume(value: own str) -> int64:",
      "    return value.len()",
      "def main() -> int32:",
      "    shared_only: def(str) -> int64 = consume",
      "    return 0",
      ""
    ].join("\n");
    const capabilityAnalysis = await analyzeWithCompiler(mainUri, invalidCapability);
    assert.equal(capabilityAnalysis.diagnostics.length, 1);
    assert.equal(capabilityAnalysis.diagnostics[0].code, "AU2002");
    assert.match(
      capabilityAnalysis.diagnostics[0].message,
      /has `own` capability.*requires `shared`/
    );

    const dynamicNamedArgument = [
      "def increment(value: int32) -> int32:",
      "    return value + 1",
      "def double(amount: int32) -> int32:",
      "    return amount * 2",
      "def choose(first: bool) -> def(int32) -> int32:",
      "    return increment if first else double",
      "def main() -> int32:",
      "    selected = choose(true)",
      "    return selected(value=20)",
      ""
    ].join("\n");
    const dynamicAnalysis = await analyzeWithCompiler(mainUri, dynamicNamedArgument);
    assert.equal(dynamicAnalysis.diagnostics.length, 1);
    assert.equal(dynamicAnalysis.diagnostics[0].code, "AU2003");
    assert.match(
      dynamicAnalysis.diagnostics[0].message,
      /named argument contract was erased.*possible targets do not all agree/
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge completes to_float for every integer type", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-int-to-float-"));
  const integerTypes = [
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "int128",
    "intsize",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uint128",
    "uintsize"
  ];

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    for (const type of integerTypes) {
      const mainPath = path.join(tempRoot, `${type}.au`);
      const mainUri = `file://${mainPath}`;
      const source = [
        "def main() -> int32:",
        `    value: ${type} = 1`,
        "    value.",
        "    return 0",
        ""
      ].join("\n");
      const line = 2;
      const character = source.split("\n")[line].indexOf(".") + 1;
      const completions = await completeWithCompiler(
        mainUri,
        source,
        line,
        character,
        "."
      );

      assert.ok(completions, `${type} should return compiler completions`);
      const toFloat = completions.find((item) => item.name === "to_float");
      assert.ok(toFloat, `${type} should complete to_float`);
      assert.equal(toFloat.kind, "method", type);
      assert.equal(toFloat.detail, "to_float() -> float64", type);
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes the complete math module function surface", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-math-module-"));
  const source = [
    "import math",
    "from math import pi as circle",
    "",
    "def main() -> int32:",
    "    print(math.floor(3.75))",
    "    print(math.ceil(-3.75))",
    "    print(math.trunc(-3.75))",
    "    print(math.pow(2.0, 10.0))",
    "    print(math.exp(0.0))",
    "    print(math.log(1.0))",
    "    print(math.log2(8.0))",
    "    print(math.log10(1000.0))",
    "    print(math.sin(0.0))",
    "    print(math.cos(0.0))",
    "    print(math.tan(0.0))",
    "    print(math.pi)",
    "    print(math.e)",
    "    print(math.inf)",
    "    print(math.nan)",
    "    print(circle)",
    "    return 0",
    ""
  ].join("\n");
  const signatures = new Map([
    ["floor", "floor(value: float64) -> int64"],
    ["ceil", "ceil(value: float64) -> int64"],
    ["trunc", "trunc(value: float64) -> int64"],
    ["pow", "pow(base: float64, exponent: float64) -> float64"],
    ["exp", "exp(value: float64) -> float64"],
    ["log", "log(value: float64) -> float64"],
    ["log2", "log2(value: float64) -> float64"],
    ["log10", "log10(value: float64) -> float64"],
    ["sin", "sin(value: float64) -> float64"],
    ["cos", "cos(value: float64) -> float64"],
    ["tan", "tan(value: float64) -> float64"]
  ]);
  const constants = new Map([
    ["pi", "float64"],
    ["e", "float64"],
    ["inf", "float64"],
    ["nan", "float64"]
  ]);

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    for (const [name, signature] of signatures) {
      assert.ok(
        analysis.occurrences.some(
          (occurrence) =>
            occurrence.hover.includes(`function ${name}`) &&
            occurrence.hover.includes(signature)
        ),
        `missing math.${name} hover signature: ${signature}`
      );
    }
    for (const name of constants.keys()) {
      assert.ok(
        analysis.occurrences.some(
          (occurrence) =>
            occurrence.hover.includes(`module constant ${name}`) &&
            occurrence.hover.includes("float64")
        ),
        `missing math.${name} constant hover`
      );
    }
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover.includes("module constant circle") &&
          occurrence.hover.includes("float64")
      ),
      "missing directly imported math.pi alias hover"
    );

    const completionSource = source.replace(
      "    return 0\n",
      "    math.\n    return 0\n"
    );
    const line = completionSource
      .split("\n")
      .findIndex((sourceLine) => sourceLine.trim() === "math.");
    const character = completionSource.split("\n")[line].indexOf(".") + 1;
    const completions = await completeWithCompiler(
      mainUri,
      completionSource,
      line,
      character,
      "."
    );

    assert.ok(completions);
    const details = new Map(completions.map((item) => [item.name, item.detail]));
    assert.deepEqual(details, new Map([...signatures, ...constants]));
    assert.ok(
      completions.every((item) =>
        constants.has(item.name)
          ? item.kind === "constant"
          : item.kind === "function"
      )
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes the complete Duration surface and operator precedence", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-duration-"));
  const source = [
    "trait FloorDiv[Rhs, Out]:",
    "    def floor_div(self, rhs: Rhs) -> Out",
    "",
    "class Counter:",
    "    value: int64",
    "",
    "impl FloorDiv[Counter, Counter] for Counter:",
    "    def floor_div(self, rhs: Counter) -> Counter:",
    "        return Counter(value=self.value + rhs.value)",
    "",
    "def inspect(value: int64, left: Duration, right: Duration) -> float64:",
    "    millis: Duration = Duration.ms(value)",
    "    seconds: Duration = Duration.seconds(value=value)",
    "    minutes: Duration = Duration.minutes(value)",
    "    added: Duration = left + right",
    "    subtracted: Duration = left - right",
    "    scaled_right: Duration = left * value",
    "    scaled_left: Duration = value * right",
    "    divided: Duration = left // value",
    "    numeric: int64 = value // 2",
    "    custom: Counter = Counter(value=1) // Counter(value=2)",
    "    equal: bool = left == right",
    "    unequal: bool = left != right",
    "    less: bool = left < right",
    "    less_equal: bool = left <= right",
    "    greater: bool = left > right",
    "    greater_equal: bool = left >= right",
    "    return millis.to_ms() + seconds.to_seconds() + minutes.to_ms()",
    "",
    "def main() -> int32:",
    "    return 0",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    for (const signature of [
      "type Duration",
      "ms(value: int64) -> Duration",
      "seconds(value: int64) -> Duration",
      "minutes(value: int64) -> Duration",
      "to_ms() -> float64",
      "to_seconds() -> float64"
    ]) {
      assert.ok(
        analysis.occurrences.some((occurrence) => occurrence.hover.includes(signature)),
        `missing Duration hover: ${signature}`
      );
    }

    const staticSource = source.replace(
      "    return 0\n",
      "    Duration.\n    return 0\n"
    );
    const staticLine = staticSource
      .split("\n")
      .findIndex((line) => line.trim() === "Duration.");
    const staticCharacter = staticSource.split("\n")[staticLine].indexOf(".") + 1;
    const staticCompletions = await completeWithCompiler(
      mainUri,
      staticSource,
      staticLine,
      staticCharacter,
      "."
    );
    const staticDetails = new Map(
      staticCompletions.map((item) => [item.name, item.detail])
    );
    assert.equal(staticDetails.get("ms"), "ms(value: int64) -> Duration");
    assert.equal(staticDetails.get("seconds"), "seconds(value: int64) -> Duration");
    assert.equal(staticDetails.get("minutes"), "minutes(value: int64) -> Duration");
    assert.equal(staticDetails.has("to_ms"), false);

    const instanceSource = source.replace(
      "    return millis.to_ms() + seconds.to_seconds() + minutes.to_ms()\n",
      "    left.\n    return millis.to_ms() + seconds.to_seconds() + minutes.to_ms()\n"
    );
    const instanceLine = instanceSource
      .split("\n")
      .findIndex((line) => line.trim() === "left.");
    const instanceCharacter = instanceSource.split("\n")[instanceLine].indexOf(".") + 1;
    const instanceCompletions = await completeWithCompiler(
      mainUri,
      instanceSource,
      instanceLine,
      instanceCharacter,
      "."
    );
    const instanceDetails = new Map(
      instanceCompletions.map((item) => [item.name, item.detail])
    );
    assert.equal(instanceDetails.get("to_ms"), "to_ms() -> float64");
    assert.equal(instanceDetails.get("to_seconds"), "to_seconds() -> float64");
    assert.equal(instanceDetails.has("seconds"), false);

    const mixedAnalysis = await analyzeWithCompiler(
      mainUri,
      "def invalid(duration: Duration):\n    value = duration / duration\n"
    );
    assert.equal(mixedAnalysis.diagnostics.length, 1);
    assert.equal(
      mixedAnalysis.diagnostics[0].message,
      "unsupported Duration operands: `Duration` and `Duration`; supported forms are `Duration + Duration`, `Duration - Duration`, `Duration * int64`, `int64 * Duration`, `Duration // int64`, and comparisons between two Duration values"
    );
    assert.equal(mixedAnalysis.diagnostics[0].code, "AU2003");
    const lspDiagnostic = compilerDiagnosticsToLsp(mixedAnalysis, mainUri)[0];
    assert.equal(lspDiagnostic.source, "aura-compiler");
    assert.equal(lspDiagnostic.code, "AU2003");
    assert.equal(lspDiagnostic.message, mixedAnalysis.diagnostics[0].message);

    const constructorAnalysis = await analyzeWithCompiler(
      mainUri,
      "def invalid():\n    value = Duration.seconds(true)\n"
    );
    assert.equal(constructorAnalysis.diagnostics.length, 1);
    assert.equal(
      constructorAnalysis.diagnostics[0].message,
      "`Duration.seconds` expects `int64`, found `bool`"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge understands trait symbols and trait method completions", async () => {
  setWorkspaceRoots([repoRoot]);
  const analysis = await analyzeWithCompiler(traitUri, traitSource);

  assert.ok(analysis);
  assert.equal(analysis.diagnostics.length, 0);
  assert.ok(analysis.symbols.some((symbol) => symbol.kind === "trait" && symbol.name === "Greeter"));

  const lineIndex = traitSource.split("\n").findIndex((line) => line.includes("value.greet()"));
  const lineText = traitSource.split("\n")[lineIndex];
  const character = lineText.indexOf(".") + 1;

  const completions = await completeWithCompiler(
    traitUri,
    traitSource,
    lineIndex,
    character,
    "."
  );
  assert.ok(completions);
  assert.ok(completions.some((item) => item.name === "greet" && item.detail === "greet(self) -> str"));
});

test("compiler bridge resolves local module imports for analysis and completions", async () => {
  setWorkspaceRoots([repoRoot]);
  const analysis = await analyzeWithCompiler(modulesUri, modulesSource);

  assert.ok(analysis);
  assert.equal(analysis.diagnostics.length, 0);
  assert.ok(
    analysis.occurrences.some((occurrence) => occurrence.hover.includes("function double"))
  );

  const lineIndex = modulesSource
    .split("\n")
    .findIndex((line) => line.includes("helpers.math.double"));
  const lineText = modulesSource.split("\n")[lineIndex];
  const character = lineText.indexOf(".double") + 1;

  const completions = await completeWithCompiler(
    modulesUri,
    modulesSource,
    lineIndex,
    character,
    "."
  );

  assert.ok(completions);
  assert.ok(completions.some((item) => item.name === "double"));
});

test("compiler bridge exposes module constants through symbols hover definitions and completions", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-module-constants-"));
  try {
    const settingsPath = path.join(tempRoot, "settings.au");
    fs.writeFileSync(settingsPath, "public service_name: str = \"planner\"\n");
    const canonicalSettingsPath = fs.realpathSync(settingsPath);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source = [
      "import settings",
      "from settings import service_name as configured_name",
      "local_name = configured_name.clone()",
      "",
      "def main():",
      "    print(settings.service_name)",
      "    print(local_name)"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 0);
    assert.ok(
      analysis.symbols.some(
        (symbol) => symbol.name === "local_name" && symbol.kind === "constant"
      )
    );
    const importedUse = analysis.occurrences.find(
      (occurrence) =>
        occurrence.line === 5 && occurrence.hover.includes("module constant service_name")
    );
    assert.ok(importedUse, JSON.stringify(analysis.occurrences, null, 2));
    assert.equal(importedUse.definition?.file_path, canonicalSettingsPath);

    const globalCompletions = await completeWithCompiler(mainUri, source, 4, 0, null);
    assert.ok(
      globalCompletions.some(
        (completion) => completion.name === "local_name" && completion.kind === "constant"
      )
    );
    const memberSource = source.replace("    print(settings.service_name)", "    settings.");
    const memberCompletions = await completeWithCompiler(
      mainUri,
      memberSource,
      5,
      memberSource.split("\n")[5].length,
      "."
    );
    assert.ok(
      memberCompletions.some(
        (completion) => completion.name === "service_name" && completion.kind === "constant"
      )
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge explains module constants that read top-level script locals", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-top-level-local-"));
  const source = [
    "class C:",
    "    v: int64",
    "",
    "    def take(own self) -> int64:",
    "        return self.v",
    "",
    "mut c = C(v=1)",
    "x = c.take()",
    "print(x)",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 1);
    assert.equal(analysis.diagnostics[0].code, "AU2001");
    assert.equal(
      analysis.diagnostics[0].message,
      "module constant `x` cannot read top-level script local `c`"
    );
    assert.deepEqual(analysis.diagnostics[0].help, [
      "declare `x` with `mut` to make it a top-level script local, or move this work into `main`"
    ]);

    const [diagnostic] = compilerDiagnosticsToLsp(analysis, mainUri);
    assert.equal(
      diagnostic.relatedInformation?.[0]?.message,
      "`c` is initialized when top-level entry statements run"
    );
    assert.deepEqual(diagnostic.range, {
      start: { line: 7, character: 4 },
      end: { line: 7, character: 5 }
    });
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves import aliases in hover, definitions, and completions", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-import-aliases-"));
  try {
    fs.mkdirSync(path.join(tempRoot, "pkg"));
    const mathPath = path.join(tempRoot, "pkg/math.au");
    fs.writeFileSync(
      mathPath,
      "public def double(value: int32) -> int32:\n    return value * 2\n"
    );
    const canonicalMathPath = fs.realpathSync(mathPath);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;

    const moduleAliasSource = [
      "import pkg.math as numbers",
      "",
      "def main() -> int32:",
      "    return numbers.double(21)"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const moduleAnalysis = await analyzeWithCompiler(mainUri, moduleAliasSource);
    assert.ok(moduleAnalysis);
    assert.equal(moduleAnalysis.diagnostics.length, 0);
    const moduleAliasDeclaration = moduleAnalysis.occurrences.find(
      (occurrence) =>
        occurrence.line === 0 && occurrence.hover.includes("module numbers = pkg.math")
    );
    assert.ok(moduleAliasDeclaration, JSON.stringify(moduleAnalysis.occurrences, null, 2));
    assert.equal(moduleAliasDeclaration.definition?.file_path, canonicalMathPath);
    const moduleAlias = moduleAnalysis.occurrences.find(
      (occurrence) =>
        occurrence.line === 3 && occurrence.hover.includes("module numbers = pkg.math")
    );
    assert.ok(moduleAlias, JSON.stringify(moduleAnalysis.occurrences, null, 2));
    assert.equal(moduleAlias.definition?.file_path, canonicalMathPath);
    const moduleMember = moduleAnalysis.occurrences.find(
      (occurrence) => occurrence.line === 3 && occurrence.hover.includes("function double")
    );
    assert.ok(moduleMember, JSON.stringify(moduleAnalysis.occurrences, null, 2));
    assert.equal(moduleMember.definition?.file_path, canonicalMathPath);

    const moduleAliasCompletions = await completeWithCompiler(
      mainUri,
      moduleAliasSource,
      2,
      0,
      null
    );
    assert.ok(moduleAliasCompletions);
    assert.ok(
      moduleAliasCompletions.some(
        (completion) => completion.name === "numbers" && completion.kind === "module"
      )
    );
    assert.ok(!moduleAliasCompletions.some((completion) => completion.name === "math"));

    const memberCompletionSource = moduleAliasSource.replace(
      "    return numbers.double(21)",
      "    numbers.\n    return 0"
    );
    const memberLine = memberCompletionSource
      .split("\n")
      .findIndex((line) => line.trim() === "numbers.");
    const memberCompletions = await completeWithCompiler(
      mainUri,
      memberCompletionSource,
      memberLine,
      memberCompletionSource.split("\n")[memberLine].length,
      "."
    );
    assert.ok(memberCompletions);
    assert.ok(memberCompletions.some((completion) => completion.name === "double"));

    const bindingAliasSource = [
      "from pkg.math import double as twice",
      "",
      "def main() -> int32:",
      "    return twice(21)"
    ].join("\n");
    const bindingAnalysis = await analyzeWithCompiler(mainUri, bindingAliasSource);
    assert.ok(bindingAnalysis);
    assert.equal(bindingAnalysis.diagnostics.length, 0);
    const bindingAliasDeclaration = bindingAnalysis.occurrences.find(
      (occurrence) =>
        occurrence.line === 0 && occurrence.hover.includes("Alias `twice` for `pkg.math.double`")
    );
    assert.ok(bindingAliasDeclaration, JSON.stringify(bindingAnalysis.occurrences, null, 2));
    assert.equal(bindingAliasDeclaration.definition?.file_path, canonicalMathPath);
    const bindingAlias = bindingAnalysis.occurrences.find(
      (occurrence) => occurrence.line === 3 && occurrence.hover.includes("function double")
    );
    assert.ok(bindingAlias, JSON.stringify(bindingAnalysis.occurrences, null, 2));
    assert.equal(bindingAlias.definition?.file_path, canonicalMathPath);

    const bindingCompletions = await completeWithCompiler(
      mainUri,
      bindingAliasSource,
      2,
      0,
      null
    );
    assert.ok(bindingCompletions);
    assert.ok(
      bindingCompletions.some(
        (completion) => completion.name === "twice" && completion.kind === "function"
      )
    );
    assert.ok(!bindingCompletions.some((completion) => completion.name === "double"));

    const builtinAliasSource = [
      "import path as paths",
      "",
      "def main() -> int32:",
      "    print(paths.join(\"root\", \"item.au\"))",
      "    return 0"
    ].join("\n");
    const builtinAnalysis = await analyzeWithCompiler(mainUri, builtinAliasSource);
    assert.ok(builtinAnalysis);
    assert.equal(builtinAnalysis.diagnostics.length, 0);
    assert.ok(
      builtinAnalysis.occurrences.some(
        (occurrence) => occurrence.hover.includes("module paths = path")
      )
    );
    const builtinMemberSource = builtinAliasSource.replace(
      "    print(paths.join(\"root\", \"item.au\"))",
      "    paths."
    );
    const builtinMembers = await completeWithCompiler(
      mainUri,
      builtinMemberSource,
      3,
      10,
      "."
    );
    assert.ok(builtinMembers);
    assert.ok(builtinMembers.some((completion) => completion.name === "join"));
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves definitions for namespace-imported symbols", async () => {
  setWorkspaceRoots([repoRoot]);
  const analysis = await analyzeWithCompiler(namespaceTypesUri, namespaceTypesSource);
  const typesPath = path.join(repoRoot, "examples/modules/pkg/types.au");

  assert.ok(analysis);
  assert.equal(analysis.diagnostics.length, 0);
  assert.ok(
    analysis.occurrences.some(
      (occurrence) =>
        occurrence.hover.includes("module pkg.types") &&
        occurrence.definition !== null &&
        occurrence.definition.file_path === typesPath
    )
  );
  assert.ok(
    analysis.occurrences.some(
      (occurrence) =>
        occurrence.hover.includes("class Counter") &&
        occurrence.definition !== null &&
        occurrence.definition.file_path === typesPath
    )
  );
  assert.ok(
    analysis.occurrences.some(
      (occurrence) =>
          occurrence.hover.includes("enum pkg.types.Status") &&
        occurrence.definition !== null &&
        occurrence.definition.file_path === typesPath
    )
  );
});

test("compiler bridge records enum variant occurrences in match patterns", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-match-patterns-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source = [
      "enum Status:",
      "    Ready",
      "    Busy",
      "",
      "def render(status: Status) -> int32:",
      "    match status:",
      "        case Status.Ready:",
      "            return 1",
      "        case Status.Busy:",
      "            return 0"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 0);
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.line === 6 &&
          occurrence.hover.includes("variant Ready") &&
          occurrence.definition !== null
      )
    );
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.line === 8 &&
          occurrence.hover.includes("variant Busy") &&
          occurrence.definition !== null
      )
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge scopes or-pattern bindings through guards and arm bodies", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-match-guards-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source = [
      "enum Reading:",
      "    Exact(int32)",
      "    Approx(int32)",
      "",
      "def inspect(reading: Reading) -> int32:",
      "    match reading:",
      "        case Exact(value) | Approx(value) if value > 0:",
      "            return value",
      "        case Exact(value) | Approx(value):",
      "            return value"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 0, JSON.stringify(analysis.diagnostics));
    for (const line of [6, 7, 9]) {
      assert.ok(
        analysis.occurrences.some(
          (occurrence) =>
            occurrence.line === line &&
            occurrence.hover.includes("local value: int32") &&
            occurrence.definition !== null &&
            occurrence.definition.line === (line === 9 ? 8 : 6)
        ),
        `missing guarded or-pattern binding occurrence on line ${line + 1}`
      );
    }
    assert.equal(
      analysis.occurrences.filter((occurrence) => occurrence.hover.includes("variant Exact")).length,
      2
    );
    assert.equal(
      analysis.occurrences.filter((occurrence) => occurrence.hover.includes("variant Approx")).length,
      2
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves complete owned enum payload signatures", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-enum-payloads-"));
  try {
    fs.mkdirSync(path.join(tempRoot, "pkg"));
    const eventsPath = path.join(tempRoot, "pkg/events.au");
    fs.writeFileSync(
      eventsPath,
      [
        "public enum Event:",
        "    Message(code: int32, body: str)",
        ""
      ].join("\n")
    );
    const canonicalEventsPath = fs.realpathSync(eventsPath);

    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source = [
      "import pkg.events",
      "",
      "def inspect(event: own pkg.events.Event):",
      "    match event:",
      "        case pkg.events.Event.Message(code, body):",
      "            print(code)",
      "            print(body)",
      "",
      "def main():",
      "    event = pkg.events.Event.Message(code=7, body=\"hello\")",
      "    inspect(event)",
      ""
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 0);
    const matchingVariantOccurrences = analysis.occurrences.filter(
      (occurrence) =>
        occurrence.hover.includes(
          "variant Message(code: own int32, body: own str) -> pkg.events.Event"
        ) &&
        occurrence.definition !== null &&
        occurrence.definition.file_path === canonicalEventsPath
    );
    assert.equal(
      matchingVariantOccurrences.length,
      2,
      JSON.stringify(analysis.occurrences, null, 2)
    );

    const completionSource = [
      "import pkg.events",
      "",
      "def main():",
      "    pkg.events.Event.",
      ""
    ].join("\n");
    const lineIndex = completionSource
      .split("\n")
      .findIndex((line) => line.trim() === "pkg.events.Event.");
    const character = completionSource.split("\n")[lineIndex].lastIndexOf(".") + 1;
    const completions = await completeWithCompiler(
      mainUri,
      completionSource,
      lineIndex,
      character,
      "."
    );
    const message = completions.find((completion) => completion.name === "Message");
    assert.ok(message);
    assert.equal(
      message.detail,
      "Message(code: own int32, body: own str) -> pkg.events.Event"
    );

    for (const [enumName, variantName, detail] of [
      ["WaitAny", "Ready", "Ready(own int64, own T) -> WaitAny"],
      ["WaitAll", "Error", "Error(own int64, own str) -> WaitAll"]
    ]) {
      const builtinSource = `def main():\n    ${enumName}.\n`;
      const builtinCompletions = await completeWithCompiler(
        mainUri,
        builtinSource,
        1,
        `    ${enumName}.`.length,
        "."
      );
      assert.equal(
        builtinCompletions.find((completion) => completion.name === variantName)?.detail,
        detail
      );
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge includes imported trait methods in completions", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-imported-trait-"));
  try {
    fs.mkdirSync(path.join(tempRoot, "pkg"));
    fs.writeFileSync(
      path.join(tempRoot, "pkg/named.au"),
      "public trait Named:\n    def name(self) -> str\n"
    );
    fs.writeFileSync(
      path.join(tempRoot, "pkg/user.au"),
      "from pkg.named import Named\n\npublic class User:\n    public label: str\n\nimpl Named for User:\n    def name(self) -> str:\n        return self.label.clone()\n"
    );
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source =
      "from pkg.user import User\n\ndef main() -> int32:\n    user = User(label=\"Ada\")\n    user.\n    return 0\n";

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.ok(Array.isArray(analysis.diagnostics));

    const lineIndex = source.split("\n").findIndex((line) => line.includes("user."));
    const lineText = source.split("\n")[lineIndex];
    const character = lineText.indexOf(".") + 1;
    const completions = await completeWithCompiler(mainUri, source, lineIndex, character, ".");

    assert.ok(completions);
    assert.ok(completions.some((item) => item.name === "label"));
    assert.ok(completions.some((item) => item.name === "name" && item.detail === "name(self) -> str"));
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves cross-file definitions for imported function, field, and trait method uses", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-cross-file-"));
  try {
    fs.mkdirSync(path.join(tempRoot, "pkg"));
    const mathPath = path.join(tempRoot, "pkg/math.au");
    const userPath = path.join(tempRoot, "pkg/user.au");
    const canonicalMathPath = fs.realpathSync.native(path.dirname(mathPath))
      ? path.join(fs.realpathSync.native(path.dirname(mathPath)), path.basename(mathPath))
      : mathPath;
    const canonicalUserPath = fs.realpathSync.native(path.dirname(userPath))
      ? path.join(fs.realpathSync.native(path.dirname(userPath)), path.basename(userPath))
      : userPath;
    fs.writeFileSync(
      mathPath,
      "public def add(left: int32, right: int32) -> int32:\n    return left + right\n"
    );
    fs.writeFileSync(
      path.join(tempRoot, "pkg/named.au"),
      "public trait Named:\n    def name(self) -> str\n"
    );
    fs.writeFileSync(
      userPath,
      "from pkg.named import Named\n\npublic class User:\n    public label: str\n\nimpl Named for User:\n    def name(self) -> str:\n        return self.label.clone()\n"
    );
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source = [
      "from pkg.math import add",
      "from pkg.user import User",
      "",
      "def main() -> int32:",
      "    total = add(left=1, right=2)",
      "    user = User(label=\"Ada\")",
      "    print(user.label)",
      "    print(user.name())",
      "    return total"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 0);
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover.includes("function add") &&
          occurrence.definition?.file_path === canonicalMathPath
      )
    );
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover.includes("class User") &&
          occurrence.definition?.file_path === canonicalUserPath
      )
    );
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover.includes("field label: str") &&
          occurrence.definition?.file_path === canonicalUserPath
      )
    );
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover.includes("method name(self) -> str") &&
          occurrence.definition?.file_path === canonicalUserPath
      )
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge analyzes and completes manifest-rooted path dependencies", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-packages-"));
  try {
    fs.mkdirSync(path.join(tempRoot, "app", "src", "helpers"), { recursive: true });
    fs.mkdirSync(path.join(tempRoot, "util", "src"), { recursive: true });
    fs.writeFileSync(
      path.join(tempRoot, "app", "Aura.toml"),
      [
        "[package]",
        'name = "app"',
        'version = "0.1.0"',
        'edition = "2026"',
        "",
        "[dependencies]",
        'util = { path = "../util" }'
      ].join("\n")
    );
    fs.writeFileSync(
      path.join(tempRoot, "app", "src", "helpers", "math.au"),
      "public def triple(value: int32) -> int32:\n    return value * 3\n"
    );
    fs.writeFileSync(
      path.join(tempRoot, "util", "Aura.toml"),
      ['[package]', 'name = "util"', 'version = "0.1.0"', 'edition = "2026"'].join("\n")
    );
    fs.writeFileSync(
      path.join(tempRoot, "util", "src", "math.au"),
      "public def double(value: int32) -> int32:\n    return value * 2\n"
    );
    const canonicalUtilMathPath = path.join(
      fs.realpathSync.native(path.join(tempRoot, "util", "src")),
      "math.au"
    );

    const mainPath = path.join(tempRoot, "app", "src", "main.au");
    const mainUri = `file://${mainPath}`;
    const validSource = [
      "import util.math",
      "import helpers.math",
      "",
      "def main() -> int32:",
      "    print(util.math.double(value=helpers.math.triple(value=2)))",
      "    return 0"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, validSource);
    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 0);
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover.includes("function double") &&
          occurrence.definition?.file_path === canonicalUtilMathPath
      )
    );

    const completionSource = [
      "import util.math",
      "import helpers.math",
      "",
      "def main() -> int32:",
      "    util.math.",
      "    return helpers.math.triple(value=2)"
    ].join("\n");
    const completions = await completeWithCompiler(mainUri, completionSource, 4, 14, ".");
    assert.ok(completions);
    assert.ok(completions.some((item) => item.name === "double"));
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge analyzes and completes manifest-rooted git dependencies", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-git-packages-"));
  try {
    const appRoot = path.join(tempRoot, "app");
    const repoRootPath = path.join(tempRoot, "util-repo");
    fs.mkdirSync(path.join(appRoot, "src"), { recursive: true });
    fs.mkdirSync(path.join(repoRootPath, "src"), { recursive: true });
    fs.writeFileSync(
      path.join(appRoot, "Aura.toml"),
      [
        "[package]",
        'name = "app"',
        'version = "0.1.0"',
        'edition = "2026"',
        "",
        "[dependencies]",
        'util = { git = "../util-repo" }'
      ].join("\n")
    );
    fs.writeFileSync(
      path.join(repoRootPath, "Aura.toml"),
      ['[package]', 'name = "util"', 'version = "0.1.0"', 'edition = "2026"'].join("\n")
    );
    fs.writeFileSync(
      path.join(repoRootPath, "src", "math.au"),
      "public def double(value: int32) -> int32:\n    return value * 2\n"
    );
    childProcess.execFileSync("git", ["init", "-b", "main"], { cwd: repoRootPath });
    childProcess.execFileSync("git", ["config", "user.name", "Aura Tests"], { cwd: repoRootPath });
    childProcess.execFileSync("git", ["config", "user.email", "aura-tests@example.com"], {
      cwd: repoRootPath
    });
    childProcess.execFileSync("git", ["add", "."], { cwd: repoRootPath });
    childProcess.execFileSync("git", ["commit", "-m", "initial"], { cwd: repoRootPath });
    const mainPath = path.join(appRoot, "src", "main.au");
    const mainUri = `file://${mainPath}`;
    const validSource = [
      "import util.math",
      "",
      "def main() -> int32:",
      "    print(util.math.double(value=2))",
      "    return 0"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, validSource);
    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 0);
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover.includes("function double") &&
          occurrence.definition?.file_path &&
          occurrence.definition.file_path.endsWith(`${path.sep}src${path.sep}math.au`)
      )
    );

    const completionSource = [
      "import util.math",
      "",
      "def main() -> int32:",
      "    util.math.",
      "    return 0"
    ].join("\n");
    const completions = await completeWithCompiler(mainUri, completionSource, 3, 14, ".");
    assert.ok(completions);
    assert.ok(completions.some((item) => item.name === "double"));
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge maps cross-file definitions to file URIs", () => {
  const location = compilerDefinitionToLspLocation("file:///workspace/main.au", {
    file_path: path.join(repoRoot, "examples/modules/pkg/types.au"),
    line: 1,
    start_character: 4,
    end_character: 11
  });

  assert.equal(location.uri, `file://${path.join(repoRoot, "examples/modules/pkg/types.au")}`);
  assert.deepEqual(location.range, {
    start: { line: 1, character: 4 },
    end: { line: 1, character: 11 }
  });
});

test("compiler bridge includes list collection members in completions", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-vec-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source =
      "def main() -> int32:\n    mut values = [1, 2, 3]\n    values.\n    return 0\n";

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.ok(Array.isArray(analysis.diagnostics));

    const lineIndex = source.split("\n").findIndex((line) => line.includes("values."));
    const lineText = source.split("\n")[lineIndex];
    const character = lineText.indexOf(".") + 1;
    const completions = await completeWithCompiler(mainUri, source, lineIndex, character, ".");

    assert.ok(completions);
    const names = new Set(completions.map((item) => item.name));
    assert.deepEqual(
      [...names].sort(),
      [
        "append",
        "clear",
        "copy",
        "count",
        "extend",
        "filter",
        "get",
        "index",
        "insert",
        "is_empty",
        "len",
        "map",
        "pop",
        "remove",
        "reserve",
        "reverse",
        "set",
        "sort",
        "swap"
      ]
    );
    const details = new Map(completions.map((item) => [item.name, item.detail]));
    assert.equal(details.get("len"), "len() -> int64");
    assert.equal(details.get("append"), "append(value: own T) -> None");
    assert.equal(details.get("pop"), "pop(index: int64 = -1) -> T");
    assert.equal(details.get("remove"), "remove(value: T) -> None");
    assert.equal(details.get("index"), "index(value: T) -> int64");
    assert.equal(details.get("count"), "count(value: T) -> int64");
    assert.equal(details.get("set"), "set(index: int64, value: own T) -> T");
    assert.equal(details.get("swap"), "swap(first: int64, second: int64) -> None");
    assert.equal(details.get("extend"), "extend(other: own list[T]) -> None");
    assert.equal(details.get("insert"), "insert(index: int64, value: own T) -> None");
    assert.match(details.get("sort"), /^sort\(/);
    assert.match(details.get("sort"), /reverse: bool = false/);
    assert.equal(details.get("copy"), "copy() -> list[T]");
    assert.equal(details.get("reserve"), "reserve(additional: int64) -> None");
    assert.equal(details.get("map"), "map(f: def(T) -> U) -> list[U]");
    assert.equal(details.get("filter"), "filter(f: def(T) -> bool) -> list[T]");
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes list algorithm hover contracts", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-vec-algorithms-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source = [
      "def key(value: int64) -> int64:",
      "    return value",
      "def render(value: int64) -> str:",
      "    return str(value)",
      "def keep(value: int64) -> bool:",
      "    return value > 0",
      "def main():",
      "    mut values = [3, 1, 2]",
      "    values.sort()",
      "    values.sort(key=key)",
      "    mapped = values.map(render)",
      "    filtered = values.filter(keep)",
      "    print(mapped)",
      "    print(filtered)"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);

    for (const [line, signature] of [
      [8, "sort(key: def(T) -> K = ..., reverse: bool = false) -> None"],
      [9, "sort(key: def(T) -> K = ..., reverse: bool = false) -> None"],
      [10, "map(f: def(T) -> U) -> list[U]"],
      [11, "filter(f: def(T) -> bool) -> list[T]"]
    ]) {
      const methodStart = source.split("\n")[line].indexOf(".") + 1;
      const hover = compilerHoverAtPosition(analysis, line, methodStart);
      assert.ok(hover, `list algorithm on line ${line + 1} should expose hover`);
      assert.ok(
        hover.value.includes(signature),
        `list algorithm hover should contain \`${signature}\`, found ${hover.value}`
      );
    }
    const mappedUse = source.split("\n")[12].indexOf("mapped");
    assert.equal(
      compilerHoverAtPosition(analysis, 12, mappedUse)?.value,
      "```aura\nbinding mapped: list[str]\n```"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge includes str and dict builtin members in completions", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-string-map-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source =
      "def main() -> int32:\n    text = '  aura repo  '\n    mut counts = dict[str, int32]()\n    text.\n    counts.\n    return 0\n";

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.ok(Array.isArray(analysis.diagnostics));

    const lines = source.split("\n");
    const textLineIndex = lines.findIndex((line) => line.includes("text."));
    const textCharacter = lines[textLineIndex].indexOf(".") + 1;
    const textCompletions = await completeWithCompiler(
      mainUri,
      source,
      textLineIndex,
      textCharacter,
      "."
    );

    assert.ok(textCompletions);
    const textNames = new Set(textCompletions.map((item) => item.name));
    assert.ok(textNames.has("len"));
    assert.ok(textNames.has("byte_len"));
    assert.ok(textNames.has("contains"));
    assert.ok(textNames.has("starts_with"));
    assert.ok(textNames.has("ends_with"));
    assert.ok(textNames.has("trim"));
    assert.ok(textNames.has("split"));
    assert.ok(textNames.has("replace"));
    assert.ok(textNames.has("to_lower"));
    assert.ok(textNames.has("to_upper"));
    assert.ok(textNames.has("strip_prefix"));
    assert.ok(textNames.has("strip_suffix"));
    assert.ok(textNames.has("clone"));
    assert.ok(textNames.has("join"));
    assert.equal(
      textCompletions.find((item) => item.name === "len")?.detail,
      "len() -> int64"
    );
    assert.equal(
      textCompletions.find((item) => item.name === "byte_len")?.detail,
      "byte_len() -> int64"
    );

    const mapLineIndex = lines.findIndex((line) => line.includes("counts."));
    const mapCharacter = lines[mapLineIndex].indexOf(".") + 1;
    const mapCompletions = await completeWithCompiler(
      mainUri,
      source,
      mapLineIndex,
      mapCharacter,
      "."
    );

    assert.ok(mapCompletions);
    const mapNames = new Set(mapCompletions.map((item) => item.name));
    assert.deepEqual(
      [...mapNames].sort(),
      [
        "clear",
        "copy",
        "get",
        "is_empty",
        "items",
        "keys",
        "len",
        "remove",
        "reserve",
        "update",
        "values"
      ]
    );
    assert.equal(
      mapCompletions.find((item) => item.name === "len")?.detail,
      "len() -> int64"
    );
    assert.equal(
      mapCompletions.find((item) => item.name === "items")?.detail,
      "items() -> list[(K, V)]"
    );
    assert.equal(
      mapCompletions.find((item) => item.name === "update")?.detail,
      "update(other: own dict[K, V]) -> None"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes typed select inference, hover, and outcome completions", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-typed-select-"));
  const source = [
    "def inspect(queue: Queue[str], task: Task[int32]):",
    "    result = select(queue, task, 1ms)",
    "    match result:",
    "        case SelectOutcome.Queue(index, outcome):",
    "            print(index)",
    "            print(outcome)",
    "        case SelectOutcome.Task(index, outcome):",
    "            print(index)",
    "            print(outcome)",
    "        case SelectOutcome.Deadline(index):",
    "            print(index)",
    "        case SelectOutcome.Cancelled:",
    "            pass",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);

    const selectHover = compilerHoverAtPosition(
      analysis,
      1,
      source.split("\n")[1].indexOf("select") + 1
    );
    assert.ok(selectHover, "select call should expose builtin hover");
    assert.ok(
      selectHover.value.includes(
        "select(source, ...) -> SelectOutcome[Q, T] [Queue[Q], Task[T], or Duration sources]"
      ),
      `unexpected select hover: ${selectHover.value}`
    );
    assert.ok(
      selectHover.value.includes(
        "non-repeatable Task sources are consumed at call entry"
      ),
      `select hover must teach its observation-right contract: ${selectHover.value}`
    );

    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover ===
          "```aura\nbinding result: SelectOutcome[str, int32]\n```"
      ),
      "select result hover should preserve independently inferred Queue and Task types"
    );

    const globalCompletions = await completeWithCompiler(
      mainUri,
      source,
      1,
      4,
      null
    );
    assert.deepEqual(
      globalCompletions.find((completion) => completion.name === "select"),
      {
        name: "select",
        kind: "function",
        detail:
          "select(source, ...) -> SelectOutcome[Q, T] [Queue[Q], Task[T], or Duration sources]"
      }
    );
    assert.deepEqual(
      globalCompletions.find((completion) => completion.name === "SelectOutcome"),
      {
        name: "SelectOutcome",
        kind: "enum",
        detail: "enum SelectOutcome[Q, T]"
      }
    );

    const outcomeSource = "def main():\n    SelectOutcome.\n";
    const outcomeCompletions = await completeWithCompiler(
      mainUri,
      outcomeSource,
      1,
      "    SelectOutcome.".length,
      "."
    );
    assert.deepEqual(
      outcomeCompletions.filter((completion) =>
        ["Queue", "Task", "Deadline", "Cancelled"].includes(completion.name)
      ),
      [
        {
          name: "Queue",
          kind: "variant",
          detail: "Queue(own int64, own QueueReceive[Q]) -> SelectOutcome"
        },
        {
          name: "Task",
          kind: "variant",
          detail: "Task(own int64, own TaskResult[T]) -> SelectOutcome"
        },
        {
          name: "Deadline",
          kind: "variant",
          detail: "Deadline(own int64) -> SelectOutcome"
        },
        {
          name: "Cancelled",
          kind: "variant",
          detail: "Cancelled -> SelectOutcome"
        }
      ]
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves typed select diagnostic codes and guidance", async () => {
  const tempRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), "aura-lsp-typed-select-diagnostics-")
  );

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    for (const [source, code, message, help] of [
      [
        "def main():\n    print(select())\n",
        "AU2004",
        "`select` expects at least one positional source",
        null
      ],
      [
        "def main():\n    jobs = Queue[int32]()\n    print(select(source=jobs))\n",
        "AU2004",
        "`select` does not take keyword arguments",
        null
      ],
      [
        "def main():\n    print(select(1))\n",
        "AU2002",
        "`select` sources must be `Queue[Q]`, `Task[T]`, or `Duration`",
        "pass one or more queue handles, task handles, or relative Duration values as positional sources"
      ],
      [
        "def main():\n    left = Queue[int32]()\n    right = Queue[str]()\n    print(select(left, right))\n",
        "AU2002",
        "all Queue sources in one `select` call must have the same payload type",
        "wrap heterogeneous queue payloads in one explicit enum before selecting"
      ]
    ]) {
      const analysis = await analyzeWithCompiler(mainUri, source);
      assert.ok(analysis);
      assert.equal(analysis.diagnostics.length, 1, JSON.stringify(analysis.diagnostics));
      const [diagnostic] = analysis.diagnostics;
      assert.equal(diagnostic.code, code);
      assert.ok(
        diagnostic.message.includes(message),
        `${source}: expected ${message}, found ${diagnostic.message}`
      );
      if (help !== null) {
        assert.ok(
          diagnostic.help.includes(help),
          `${source}: expected help ${help}, found ${JSON.stringify(diagnostic.help)}`
        );
      }
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge withholds typed select inference for rejected keyword arguments", async () => {
  const tempRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), "aura-lsp-typed-select-keyword-")
  );
  const source = [
    "def inspect(queue: Queue[str]):",
    "    result = select(source=queue)",
    "    print(result)",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 1);
    assert.equal(analysis.diagnostics[0].code, "AU2004");
    assert.ok(
      analysis.diagnostics[0].message.includes(
        "`select` does not take keyword arguments"
      )
    );
    assert.equal(
      analysis.occurrences.some((occurrence) =>
        occurrence.hover?.includes("SelectOutcome")
      ),
      false,
      "a rejected keyword-form select call must not advertise SelectOutcome inference or hover"
    );
    assert.equal(
      compilerHoverAtPosition(
        analysis,
        2,
        source.split("\n")[2].indexOf("result") + 1
      ),
      null,
      "a use of the rejected select result must not expose inferred SelectOutcome hover"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves typed select builtin redefinition diagnostics", async () => {
  const tempRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), "aura-lsp-typed-select-redefinition-")
  );

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    for (const [source, code, message] of [
      [
        "def select(value: int32):\n    pass\n",
        "AU2007",
        "`select` is a builtin function name and cannot be redefined"
      ],
      [
        "enum SelectOutcome:\n    Cancelled\n",
        "AU2002",
        "`SelectOutcome` is a reserved built-in type name"
      ]
    ]) {
      const analysis = await analyzeWithCompiler(mainUri, source);
      assert.ok(analysis);
      assert.equal(analysis.diagnostics.length, 1, JSON.stringify(analysis.diagnostics));
      assert.equal(analysis.diagnostics[0].code, code);
      assert.equal(analysis.diagnostics[0].message, message);
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes canonical set members and tuple-shaped dict items", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-set-dict-items-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source = [
      "def main() -> int32:",
      "    mut seen = {1, 2, 3}",
      "    counts: dict[str, int32] = {\"a\": 1, \"b\": 2}",
      "    entries: list[(str, int32)] = counts.items()",
      "    match entries.get(index=0):",
      "        case Some(found):",
      "            entry = found",
      "            seen.",
      "            print(entry[0])",
      "        case None:",
      "            pass",
      "    return 0"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.ok(Array.isArray(analysis.diagnostics));

    const lines = source.split("\n");
    const seenLineIndex = lines.findIndex((line) => line.includes("seen."));
    const seenCharacter = lines[seenLineIndex].indexOf(".") + 1;
    const setCompletions = await completeWithCompiler(
      mainUri,
      source,
      seenLineIndex,
      seenCharacter,
      "."
    );

    assert.ok(setCompletions);
    const setNames = new Set(setCompletions.map((item) => item.name));
    assert.equal(
      setCompletions.length,
      setNames.size,
      "set member completions must not contain duplicate rows"
    );
    assert.deepEqual(
      [...setNames].sort(),
      ["add", "clear", "copy", "discard", "is_empty", "len", "remove", "reserve"]
    );
    const setLengthCompletions = setCompletions.filter((item) => item.name === "len");
    assert.equal(
      setLengthCompletions.length,
      1,
      "set.len must be emitted exactly once"
    );
    assert.equal(
      setLengthCompletions[0]?.detail,
      "len() -> int64"
    );
    const setDetails = new Map(
      setCompletions.map((item) => [item.name, item.detail])
    );
    assert.equal(setDetails.get("add"), "add(value: own T) -> None");
    assert.equal(setDetails.get("remove"), "remove(value: T) -> None");
    assert.equal(setDetails.get("discard"), "discard(value: T) -> None");
    assert.equal(setDetails.get("copy"), "copy() -> set[T]");
    assert.equal(setDetails.get("reserve"), "reserve(additional: int64) -> None");

    const entriesOccurrence = analysis.occurrences.find(
      (occurrence) =>
        occurrence.hover === "```aura\nbinding entries: list[(str, int32)]\n```"
    );
    assert.ok(entriesOccurrence, "dict.items() must preserve its tuple-shaped list type");
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes collection with_capacity constructors", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-capacity-"));
  try {
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    setWorkspaceRoots([repoRoot, tempRoot]);

    for (const [typeExpression, detail] of [
      ["list[int64]", "with_capacity(minimum: int64) -> list[int64]"],
      ["dict[str, int64]", "with_capacity(minimum: int64) -> dict[str, int64]"],
      ["set[int64]", "with_capacity(minimum: int64) -> set[int64]"]
    ]) {
      const source = `def main():\n    ${typeExpression}.\n`;
      const completions = await completeWithCompiler(
        mainUri,
        source,
        1,
        source.split("\n")[1].length,
        "."
      );
      assert.ok(completions, typeExpression);
      assert.equal(
        completions.find((item) => item.name === "with_capacity")?.detail,
        detail,
        typeExpression
      );
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge includes builtin io/fs/net module and resource members", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-io-net-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const prelude = [
      "import io",
      "import fs",
      "import net",
      "",
      "def inspect(file: fs.File, listener: net.TcpListener, stream: net.TcpStream, udp: net.UdpSocket, packet: net.UdpDatagram, http_listener: net.HttpListener, exchange: net.HttpExchange, response: net.HttpResponse, ws_listener: net.WebSocketListener, socket: net.WebSocket, unix_listener: net.UnixListener, unix_stream: net.UnixStream, tls_listener: net.TlsListener, tls_stream: net.TlsStream) -> int32:"
    ];
    const sourceForLine = (line) => [...prelude, line, "    return 0"].join("\n");
    const completionsForLine = async (line) => {
      const source = sourceForLine(line);
      const lines = source.split("\n");
      const lineIndex = lines.findIndex((candidate) => candidate === line);
      const character = lines[lineIndex].indexOf(".") + 1;
      const items = await completeWithCompiler(mainUri, source, lineIndex, character, ".");
      assert.ok(items);
      return new Set(items.map((item) => item.name));
    };

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, sourceForLine("    return 0"));
    assert.ok(analysis);
    assert.ok(Array.isArray(analysis.diagnostics));
    assert.equal(analysis.diagnostics.length, 0);

    const ioNames = await completionsForLine("    io.");
    assert.ok(ioNames.has("write"));
    assert.ok(ioNames.has("flush"));
    assert.ok(ioNames.has("read_line"));
    assert.ok(ioNames.has("Error"));

    const fsNames = await completionsForLine("    fs.");
    assert.ok(fsNames.has("open"));
    assert.ok(fsNames.has("create"));
    assert.ok(fsNames.has("append"));
    assert.ok(fsNames.has("read_to_string"));
    assert.ok(fsNames.has("read_bytes"));
    assert.ok(fsNames.has("write_string"));
    assert.ok(fsNames.has("write_bytes"));
    assert.ok(fsNames.has("append_bytes"));
    assert.ok(fsNames.has("File"));

    const fileNames = await completionsForLine("    file.");
    assert.ok(fileNames.has("read_all"));
    assert.ok(fileNames.has("read_bytes"));
    assert.ok(fileNames.has("write_all"));
    assert.ok(fileNames.has("write_bytes"));
    assert.ok(fileNames.has("flush"));
    assert.ok(fileNames.has("close"));

    const netNames = await completionsForLine("    net.");
    assert.ok(netNames.has("connect_timeout"));
    assert.ok(netNames.has("udp_bind"));
    assert.ok(netNames.has("http_listen"));
    assert.ok(netNames.has("http_request_text"));
    assert.ok(netNames.has("http_request_text_timeout"));
    assert.ok(netNames.has("http_request_bytes"));
    assert.ok(netNames.has("http_request_bytes_timeout"));
    assert.ok(netNames.has("websocket_listen"));
    assert.ok(netNames.has("websocket_connect"));
    assert.ok(netNames.has("websocket_connect_timeout"));
    assert.ok(netNames.has("unix_listen"));
    assert.ok(netNames.has("unix_connect"));
    assert.ok(netNames.has("unix_connect_timeout"));
    assert.ok(netNames.has("tls_listen"));
    assert.ok(netNames.has("tls_connect"));
    assert.ok(netNames.has("tls_connect_timeout"));
    assert.ok(netNames.has("UdpSocket"));
    assert.ok(netNames.has("HttpResponse"));
    assert.ok(netNames.has("TlsStream"));

    const streamNames = await completionsForLine("    stream.");
    assert.ok(streamNames.has("read_all"));
    assert.ok(streamNames.has("read_line"));
    assert.ok(streamNames.has("read_bytes"));
    assert.ok(streamNames.has("read_exact"));
    assert.ok(streamNames.has("write_all"));
    assert.ok(streamNames.has("write_bytes"));
    assert.ok(streamNames.has("flush"));
    assert.ok(streamNames.has("local_addr"));
    assert.ok(streamNames.has("peer_addr"));
    assert.ok(streamNames.has("shutdown_read"));
    assert.ok(streamNames.has("shutdown_write"));
    assert.ok(streamNames.has("shutdown_both"));
    assert.ok(streamNames.has("close"));

    const udpNames = await completionsForLine("    udp.");
    assert.ok(udpNames.has("send_text"));
    assert.ok(udpNames.has("send_bytes"));
    assert.ok(udpNames.has("recv"));
    assert.ok(udpNames.has("recv_from"));
    assert.ok(udpNames.has("local_addr"));
    assert.ok(udpNames.has("peer_addr"));

    const exchangeNames = await completionsForLine("    exchange.");
    assert.ok(exchangeNames.has("method"));
    assert.ok(exchangeNames.has("path"));
    assert.ok(exchangeNames.has("headers"));
    assert.ok(exchangeNames.has("body_text"));
    assert.ok(exchangeNames.has("body_bytes"));
    assert.ok(exchangeNames.has("respond_text"));
    assert.ok(exchangeNames.has("respond_bytes"));

    const responseNames = await completionsForLine("    response.");
    assert.ok(responseNames.has("status"));
    assert.ok(responseNames.has("reason"));
    assert.ok(responseNames.has("headers"));
    assert.ok(responseNames.has("text"));
    assert.ok(responseNames.has("bytes"));

    const socketNames = await completionsForLine("    socket.");
    assert.ok(socketNames.has("send_text"));
    assert.ok(socketNames.has("send_bytes"));
    assert.ok(socketNames.has("recv_text"));
    assert.ok(socketNames.has("recv_bytes"));

    const unixStreamNames = await completionsForLine("    unix_stream.");
    assert.ok(unixStreamNames.has("read_line"));
    assert.ok(unixStreamNames.has("read_exact"));
    assert.ok(unixStreamNames.has("write_all"));

    const tlsStreamNames = await completionsForLine("    tls_stream.");
    assert.ok(tlsStreamNames.has("read_line"));
    assert.ok(tlsStreamNames.has("read_exact"));
    assert.ok(tlsStreamNames.has("write_all"));
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge includes builtin process module and resource members", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-process-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const prelude = [
      "import process",
      "",
      "def inspect(child: process.Child, pipe: process.Pipe, completed: process.Completed, status: process.ExitStatus, wait: process.Wait, stdio: process.Stdio, error: process.Error) -> int32:"
    ];
    const sourceForLine = (line) => [...prelude, line, "    return 0"].join("\n");
    const completionsForLine = async (line) => {
      const source = sourceForLine(line);
      const lines = source.split("\n");
      const lineIndex = lines.findIndex((candidate) => candidate === line);
      const character = lines[lineIndex].indexOf(".") + 1;
      const items = await completeWithCompiler(mainUri, source, lineIndex, character, ".");
      assert.ok(items);
      return new Set(items.map((item) => item.name));
    };

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, sourceForLine("    return 0"));
    assert.ok(analysis);
    assert.ok(Array.isArray(analysis.diagnostics));
    assert.equal(analysis.diagnostics.length, 0);

    const processNames = await completionsForLine("    process.");
    assert.ok(processNames.has("start"));
    assert.ok(processNames.has("run"));
    assert.ok(processNames.has("inherit"));
    assert.ok(processNames.has("null"));
    assert.ok(processNames.has("pipe"));
    assert.ok(processNames.has("Child"));
    assert.ok(processNames.has("Pipe"));
    assert.ok(processNames.has("Completed"));
    assert.ok(processNames.has("ExitStatus"));
    assert.ok(processNames.has("Wait"));
    assert.ok(processNames.has("Stdio"));
    assert.ok(processNames.has("Error"));

    const childNames = await completionsForLine("    child.");
    assert.ok(childNames.has("stdin"));
    assert.ok(childNames.has("stdout"));
    assert.ok(childNames.has("stderr"));
    assert.ok(childNames.has("wait"));
    assert.ok(childNames.has("kill"));
    assert.ok(childNames.has("terminate"));
    assert.ok(childNames.has("close"));

    const pipeNames = await completionsForLine("    pipe.");
    assert.ok(pipeNames.has("read_all"));
    assert.ok(pipeNames.has("read_line"));
    assert.ok(pipeNames.has("read_bytes"));
    assert.ok(pipeNames.has("write_all"));
    assert.ok(pipeNames.has("write_bytes"));
    assert.ok(pipeNames.has("flush"));
    assert.ok(pipeNames.has("close"));

    const completedNames = await completionsForLine("    completed.");
    assert.ok(completedNames.has("status"));
    assert.ok(completedNames.has("success"));
    assert.ok(completedNames.has("stdout"));
    assert.ok(completedNames.has("stderr"));
    assert.ok(completedNames.has("stdout_bytes"));
    assert.ok(completedNames.has("stderr_bytes"));
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes control-plane module completions", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-control-plane-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const modules = ["sys", "path", "json", "toml", "log", "metrics", "trace", "control"];
    const prelude = [...modules.map((name) => `import ${name}`), "", "def main() -> int32:"];
    const completions = async (moduleName) => {
      const line = `    ${moduleName}.`;
      const source = [...prelude, line, "    return 0"].join("\n");
      const lineIndex = prelude.length;
      const items = await completeWithCompiler(
        mainUri,
        source,
        lineIndex,
        line.length,
        "."
      );
      assert.ok(items);
      return items;
    };
    const completionNames = async (moduleName) =>
      new Set((await completions(moduleName)).map((item) => item.name));
    setWorkspaceRoots([repoRoot, tempRoot]);
    assert.ok((await completionNames("sys")).has("args"));
    assert.ok((await completionNames("path")).has("join"));
    assert.ok((await completionNames("json")).has("parse_string_map"));
    assert.ok((await completionNames("toml")).has("stringify_map"));
    assert.ok((await completionNames("log")).has("info"));
    assert.ok((await completionNames("metrics")).has("increment"));
    assert.ok((await completionNames("trace")).has("event"));

    const retryCompletion = (await completions("control")).find(
      (completion) => completion.name === "retry"
    );
    assert.deepEqual(retryCompletion, {
      name: "retry",
      kind: "function",
      detail:
        "retry(worker: def() -> Result[T, E], max_attempts: int32 = ..., initial_backoff: Duration = ...) -> Result[T, E]"
    });

    const analysisSource = [
      "import control",
      "",
      "def worker() -> Result[int32, str]:",
      "    return Result.Ok(7)",
      "",
      "def main():",
      "    result = control.retry[int32, str](worker)",
      "    print(result)"
    ].join("\n");
    const analysis = await analyzeWithCompiler(mainUri, analysisSource);
    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    const retryStart = analysisSource.split("\n")[6].indexOf("retry");
    const retryHover = compilerHoverAtPosition(analysis, 6, retryStart);
    assert.ok(retryHover, "control.retry should expose hover");
    assert.ok(
      retryHover.value.includes(
        "function retry(worker: def() -> Result[T, E], max_attempts: int32 = ..., initial_backoff: Duration = ...) -> Result[T, E]"
      ),
      `control.retry hover should expose its generic callable contract, found ${retryHover.value}`
    );
    const resultUse = analysisSource.split("\n")[7].indexOf("result");
    assert.equal(
      compilerHoverAtPosition(analysis, 7, resultUse)?.value,
      "```aura\nbinding result: Result[int32, str]\n```"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes the recursive json.Value contract", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-json-value-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const prelude = ["import json", "", "def main() -> int32:"];
    const completionsForLine = async (line) => {
      const source = [...prelude, line, "    return 0"].join("\n");
      const lineIndex = prelude.length;
      const items = await completeWithCompiler(mainUri, source, lineIndex, line.length, ".");
      assert.ok(items);
      return items;
    };

    setWorkspaceRoots([repoRoot, tempRoot]);
    const validSource = [
      "import json",
      "",
      "def decode(text: str) -> Result[json.Value, json.Error]:",
      "    return json.parse(text)",
      "",
      "def main() -> int32:",
      "    value = json.Value.Int(7)",
      "    print(json.dumps(value, indent=Option.Some(2)))",
      "    return 0"
    ].join("\n");
    const analysis = await analyzeWithCompiler(mainUri, validSource);
    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover.includes("parse") &&
          occurrence.hover.includes("Result[json.Value, json.Error]")
      )
    );

    const moduleItems = await completionsForLine("    json.");
    const moduleNames = new Set(moduleItems.map((item) => item.name));
    for (const expected of [
      "Value",
      "Error",
      "parse",
      "dumps",
      "is_null",
      "as_bool",
      "as_int",
      "as_float",
      "into_string",
      "into_array",
      "into_object"
    ]) {
      assert.ok(moduleNames.has(expected), `json completion should include ${expected}`);
    }
    assert.equal(
      moduleItems.find((item) => item.name === "parse")?.detail,
      "parse(text: str) -> Result[json.Value, json.Error]"
    );
    assert.equal(
      moduleItems.find((item) => item.name === "dumps")?.detail,
      "dumps(value: json.Value, indent: Option[int64] = ...) -> str"
    );
    const accessorDetails = {
      as_bool: "as_bool(value: json.Value) -> Option[bool]",
      as_float: "as_float(value: json.Value) -> Option[float64]",
      as_int: "as_int(value: json.Value) -> Option[int64]",
      into_array:
        "into_array(value: own json.Value) -> Option[list[json.Value]]",
      into_object:
        "into_object(value: own json.Value) -> Option[dict[str, json.Value]]",
      into_string: "into_string(value: own json.Value) -> Option[str]",
      is_null: "is_null(value: json.Value) -> bool"
    };
    for (const [name, detail] of Object.entries(accessorDetails)) {
      assert.equal(moduleItems.find((item) => item.name === name)?.detail, detail);
    }

    const valueItems = await completionsForLine("    json.Value.");
    assert.deepEqual(
      new Set(valueItems.map((item) => item.name)),
      new Set(["Null", "Bool", "Int", "Float", "String", "Array", "Object"])
    );
    assert.deepEqual(
      Object.fromEntries(valueItems.map((item) => [item.name, item.detail])),
      {
        Array: "Array(own list[json.Value]) -> json.Value",
        Bool: "Bool(own bool) -> json.Value",
        Float: "Float(own float64) -> json.Value",
        Int: "Int(own int64) -> json.Value",
        Null: "Null -> json.Value",
        Object: "Object(own dict[str, json.Value]) -> json.Value",
        String: "String(own str) -> json.Value"
      }
    );
    const errorItems = await completionsForLine("    json.Error.");
    assert.deepEqual(
      Object.fromEntries(errorItems.map((item) => [item.name, item.detail])),
      {
        InputTooLarge:
          "InputTooLarge(actual_bytes: own int64, limit_bytes: own int64) -> json.Error",
        NestingTooDeep:
          "NestingTooDeep(limit: own int32, line: own int32, column: own int32) -> json.Error",
        NumberOutOfRange:
          "NumberOutOfRange(line: own int32, column: own int32) -> json.Error",
        Syntax:
          "Syntax(message: own str, line: own int32, column: own int32) -> json.Error"
      }
    );
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover === "```aura\nvariant Int(own int64) -> json.Value\n```"
      )
    );

    const fromImportSource = [
      "from json import Value, Error, parse, dumps",
      "",
      "def decode(text: str) -> Result[Value, Error]:",
      "    return parse(text)",
      "",
      "def main() -> int32:",
      "    value = Value.Int(7)",
      "    print(dumps(value))",
      "    return 0"
    ].join("\n");
    const fromImportAnalysis = await analyzeWithCompiler(mainUri, fromImportSource);
    assert.ok(fromImportAnalysis);
    assert.deepEqual(fromImportAnalysis.diagnostics, []);
    assert.ok(
      fromImportAnalysis.occurrences.some(
        (occurrence) =>
          occurrence.hover === "```aura\nvariant Int(own int64) -> json.Value\n```"
      )
    );

    const fromImportCompletionSource = [
      "from json import Value",
      "",
      "def main() -> int32:",
      "    Value.",
      "    return 0"
    ].join("\n");
    const fromImportItems = await completeWithCompiler(
      mainUri,
      fromImportCompletionSource,
      3,
      "    Value.".length,
      "."
    );
    assert.ok(fromImportItems);
    assert.equal(
      fromImportItems.find((item) => item.name === "Int")?.detail,
      "Int(own int64) -> json.Value"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge analyzes indexed member chains and f-string indexed lookups", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-index-chain-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source = [
      "def main() -> int32:",
      "    keys = [\"a\", \"b\"]",
      "    idx: int32 = 1",
      "    mut counts = {\"key\": 7}",
      "    match keys.get(index=idx):",
      "        case Some(key):",
      "            print(key)",
      "        case None:",
      "            return 1",
      "    print(f\"val: {counts[\"key\"]}\")",
      "    return 0"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.deepStrictEqual(analysis.diagnostics, []);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge analyzes single-quoted strings nested in f-string interpolations", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-single-strings-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source = [
      "def main() -> int32:",
      "    print('single # quote')",
      "    print(f\"{'{left} and }'}\")",
      "    return 0"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.deepStrictEqual(analysis.diagnostics, []);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge analyzes exact string forms and typed format specifications", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-format-strings-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source = [
      "def main():",
      "    prompt = \"\"\"first",
      "second\"\"\"",
      "    path = r\"C:\\\\agents\\\\run\"",
      "    count: int64 = 1234567",
      "    print(prompt)",
      "    print(path)",
      "    print(f\"{count:*>14,d}\")"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.deepStrictEqual(analysis.diagnostics, []);
    const count = analysis.occurrences.find(
      (occurrence) => occurrence.line === 7 && occurrence.hover?.includes("count: int64")
    );
    assert.ok(count, "formatted interpolation should retain compiler-backed hover");
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge recovers completions and symbols for dangling-dot EOF buffers", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-dangling-dot-"));
  try {
    const mainPath = path.join(tempRoot, "counter.au");
    const mainUri = `file://${mainPath}`;
    const source =
      "class Counter:\n    value: int32\n\ndef main() -> int32:\n    counter = Counter(value=1)\n    counter.";

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.ok(Array.isArray(analysis.symbols));
    assert.ok(analysis.symbols.some((symbol) => symbol.name === "Counter"));

    const lineIndex = source.split("\n").findIndex((line) => line.includes("counter."));
    const lineText = source.split("\n")[lineIndex];
    const character = lineText.indexOf(".") + 1;
    const completions = await completeWithCompiler(mainUri, source, lineIndex, character, ".");

    assert.ok(completions);
    assert.ok(completions.some((item) => item.name === "value"));
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves member completion inside a call with an unrelated diagnostic", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-member-diagnostic-"));
  try {
    const mainPath = path.join(tempRoot, "tags.au");
    const mainUri = `file://${mainPath}`;
    const lines = [
      "def inspect(tags: list[str]):",
      "    first = tags[0].clone()",
      "    print(range(tags.))"
    ];
    const source = lines.join("\n");
    const character = lines[2].indexOf("tags.") + "tags.".length;

    setWorkspaceRoots([repoRoot, tempRoot]);
    const completions = await completeWithCompiler(mainUri, source, 2, character, ".");

    assert.ok(completions);
    const names = new Set(completions.map((item) => item.name));
    for (const name of ["append", "get", "len"]) {
      assert.ok(names.has(name), `list completion should include ${name}`);
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes one random.Rng constructor and its stateful members", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-random-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const prelude = [
      "import random",
      "",
      "def inspect(rng: mut random.Rng) -> int32:"
    ];
    const sourceForLine = (line) => [...prelude, line, "    return 0"].join("\n");
    const completionsForLine = async (line) => {
      const source = sourceForLine(line);
      const lines = source.split("\n");
      const lineIndex = lines.findIndex((candidate) => candidate === line);
      const character = lines[lineIndex].indexOf(".") + 1;
      const items = await completeWithCompiler(mainUri, source, lineIndex, character, ".");
      assert.ok(items);
      return items;
    };

    setWorkspaceRoots([repoRoot, tempRoot]);
    const validSource = sourceForLine("    print(rng.next_int(lo=0, hi=10))");
    const analysis = await analyzeWithCompiler(mainUri, validSource);
    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);

    const unavailableSecureFloat = await analyzeWithCompiler(
      mainUri,
      sourceForLine("    print(random.secure_float())")
    );
    assert.ok(unavailableSecureFloat);
    assert.equal(unavailableSecureFloat.diagnostics.length, 1);
    assert.equal(unavailableSecureFloat.diagnostics[0].code, "AU2001");
    assert.match(
      unavailableSecureFloat.diagnostics[0].message,
      /module `random` has no callable member `secure_float`/
    );

    const moduleItems = await completionsForLine("    random.");
    const rngItems = moduleItems.filter((item) => item.name === "Rng");
    assert.equal(rngItems.length, 1);
    assert.equal(rngItems[0].kind, "class");
    assert.equal(rngItems[0].detail, "Rng(seed: int64)");
    assert.equal(
      moduleItems.find((item) => item.name === "secure_int")?.detail,
      "secure_int(lo: int64, hi: int64) -> int64"
    );
    assert.equal(
      moduleItems.find((item) => item.name === "secure_bytes")?.detail,
      "secure_bytes(n: int64) -> list[uint8]"
    );
    assert.equal(moduleItems.some((item) => item.name === "secure_float"), false);

    const memberItems = await completionsForLine("    rng.");
    for (const [name, detail] of [
      ["next_int", "next_int(lo: int64, hi: int64) -> int64"],
      ["next_float", "next_float() -> float64"],
      ["shuffle", "shuffle(values: mut list[T]) -> None"]
    ]) {
      const matching = memberItems.filter((item) => item.name === name);
      assert.equal(matching.length, 1);
      assert.equal(matching[0].kind, "method");
      assert.equal(matching[0].detail, detail);
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes the canonical bytes module, errors, and str conversions", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-bytes-"));
  try {
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const prelude = ["import bytes", "", "def main() -> int32:"];
    const completionsForLine = async (line) => {
      const source = [...prelude, line, "    return 0"].join("\n");
      const lineIndex = prelude.length;
      const items = await completeWithCompiler(
        mainUri,
        source,
        lineIndex,
        line.length,
        "."
      );
      assert.ok(items);
      return items;
    };

    setWorkspaceRoots([repoRoot, tempRoot]);
    const validSource = [
      "import bytes",
      "",
      "def decode(value: list[uint8]) -> Result[str, bytes.Error]:",
      "    return str.from_bytes(bytes=value)",
      "",
      "def main() -> int32:",
      "    text = \"abc\"",
      "    payload = text.to_bytes()",
      "    print(bytes.hex_encode(value=payload))",
      "    print(bytes.base64_encode(value=payload))",
      "    print(bytes.sha256(value=payload))",
      "    print(bytes.sha256_string(text=text))",
      "    return 0"
    ].join("\n");
    const analysis = await analyzeWithCompiler(mainUri, validSource);
    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    for (const signature of [
      "from_bytes(bytes: list[uint8]) -> Result[str, bytes.Error]",
      "to_bytes() -> list[uint8]",
      "hex_encode(value: list[uint8]) -> str",
      "base64_encode(value: list[uint8]) -> str",
      "sha256(value: list[uint8]) -> list[uint8]",
      "sha256_string(text: str) -> list[uint8]"
    ]) {
      assert.ok(
        analysis.occurrences.some((occurrence) =>
          occurrence.hover.includes(signature)
        ),
        `missing Bytes hover: ${signature}`
      );
    }

    const moduleItems = await completionsForLine("    bytes.");
    const moduleNames = new Set(moduleItems.map((item) => item.name));
    for (const expected of [
      "Error",
      "hex_encode",
      "hex_decode",
      "base64_encode",
      "base64_decode",
      "sha256",
      "sha256_string"
    ]) {
      assert.ok(moduleNames.has(expected), `bytes completion should include ${expected}`);
    }
    const moduleDetails = {
      hex_encode: "hex_encode(value: list[uint8]) -> str",
      hex_decode: "hex_decode(text: str) -> Result[list[uint8], bytes.Error]",
      base64_encode: "base64_encode(value: list[uint8]) -> str",
      base64_decode:
        "base64_decode(text: str) -> Result[list[uint8], bytes.Error]",
      sha256: "sha256(value: list[uint8]) -> list[uint8]",
      sha256_string: "sha256_string(text: str) -> list[uint8]"
    };
    for (const [name, detail] of Object.entries(moduleDetails)) {
      assert.equal(moduleItems.find((item) => item.name === name)?.detail, detail);
    }

    const errorItems = await completionsForLine("    bytes.Error.");
    assert.deepEqual(
      Object.fromEntries(errorItems.map((item) => [item.name, item.detail])),
      {
        InvalidBase64:
          "InvalidBase64(index: own int32) -> bytes.Error",
        InvalidHexDigit:
          "InvalidHexDigit(index: own int32, byte: own uint8) -> bytes.Error",
        InvalidHexLength:
          "InvalidHexLength(length: own int32) -> bytes.Error",
        InvalidUtf8:
          "InvalidUtf8(index: own int32) -> bytes.Error"
      }
    );

    const staticItems = await completionsForLine("    str.");
    assert.equal(
      staticItems.find((item) => item.name === "from_bytes")?.detail,
      "from_bytes(bytes: list[uint8]) -> Result[str, bytes.Error]"
    );
    assert.equal(staticItems.some((item) => item.name === "to_bytes"), false);

    const instanceLine = "    text.";
    const instanceSource = [
      ...prelude,
      "    text = \"abc\"",
      instanceLine,
      "    return 0"
    ].join("\n");
    const instanceItems = await completeWithCompiler(
      mainUri,
      instanceSource,
      prelude.length + 1,
      instanceLine.length,
      "."
    );
    assert.ok(instanceItems);
    assert.equal(
      instanceItems.find((item) => item.name === "to_bytes")?.detail,
      "to_bytes() -> list[uint8]"
    );
    assert.equal(instanceItems.some((item) => item.name === "from_bytes"), false);

    const fromImportSource = [
      "from bytes import Error, hex_decode",
      "",
      "def decode(text: str) -> Result[list[uint8], Error]:",
      "    return hex_decode(text)",
      "",
      "def main() -> int32:",
      "    return 0"
    ].join("\n");
    const fromImportAnalysis = await analyzeWithCompiler(mainUri, fromImportSource);
    assert.ok(fromImportAnalysis);
    assert.deepEqual(fromImportAnalysis.diagnostics, []);
    assert.ok(
      fromImportAnalysis.occurrences.some(
        (occurrence) =>
          occurrence.hover.includes("hex_decode") &&
          occurrence.hover.includes("bytes.Error")
      )
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge recovers imported completions and symbols when a buffer contains multiple dangling dots", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-multi-dangling-"));
  try {
    fs.mkdirSync(path.join(tempRoot, "helpers"));
    fs.writeFileSync(
      path.join(tempRoot, "helpers/math.au"),
      "public def double(value: int32) -> int32:\n    return value * 2\n"
    );
    fs.writeFileSync(
      path.join(tempRoot, "helpers/counter.au"),
      "public class Counter:\n    public value: int32\n"
    );
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const source = [
      "import helpers.math",
      "from helpers.counter import Counter",
      "",
      "def main() -> int32:",
      "    counter = Counter(value=1)",
      "    print(helpers.math.",
      "    print(counter.",
      "    return 0"
    ].join("\n");

    setWorkspaceRoots([repoRoot, tempRoot]);
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.ok(Array.isArray(analysis.symbols));
    assert.ok(analysis.symbols.length > 0);
    assert.ok(Array.isArray(analysis.occurrences));
    assert.ok(analysis.occurrences.length > 0);

    const lineIndex = source.split("\n").findIndex((line) => line.includes("helpers.math."));
    const lineText = source.split("\n")[lineIndex];
    const character = lineText.lastIndexOf(".") + 1;
    const completions = await completeWithCompiler(mainUri, source, lineIndex, character, ".");

    assert.ok(completions);
    assert.ok(completions.some((item) => item.name === "double"));
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes tuple return, index, destructuring, loop, and pattern analysis", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-tuples-"));
  const source = [
    "def make() -> (int64, str):",
    "    return (1, \"one\")",
    "",
    "def main():",
    "    pair = make()",
    "    first = pair[0]",
    "    number, label = pair",
    "    rows = [(2, 3)]",
    "    for left, right in rows:",
    "        print(left + right)",
    "    match (4, 5):",
    "        case (matched_left, matched_right):",
    "            print(matched_left + matched_right)",
    "    print(first)",
    "    print(number)",
    "    print(label)",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    assert.ok(
      analysis.symbols.some(
        (symbol) =>
          symbol.name === "make" &&
          symbol.kind === "function" &&
          symbol.detail === "(int64, str)"
      )
    );
    assert.deepEqual(compilerHoverAtPosition(analysis, 5, 13), {
      value: "```aura\nbinding pair: (int64, str)\n```",
      range: {
        start: { line: 5, character: 12 },
        end: { line: 5, character: 16 }
      }
    });
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover === "```aura\nbinding number: int64\n```"
      )
    );
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover === "```aura\nbinding label: str\n```"
      )
    );
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover === "```aura\nlocal left: int64\n```"
      )
    );
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover === "```aura\nlocal right: int64\n```"
      )
    );
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover === "```aura\nlocal matched_left: int64\n```"
      )
    );
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover === "```aura\nlocal matched_right: int64\n```"
      )
    );
    assert.deepEqual(
      compilerDefinitionAtPosition(mainUri, analysis, 14, 12)?.range,
      {
        start: { line: 6, character: 4 },
        end: { line: 6, character: 10 }
      }
    );
    assert.deepEqual(
      compilerDefinitionAtPosition(mainUri, analysis, 12, 20)?.range,
      {
        start: { line: 11, character: 14 },
        end: { line: 11, character: 26 }
      }
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge maps non-copy tuple index diagnostics", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-tuple-index-"));
  const source = [
    "def main():",
    "    pair = (\"left\", 1)",
    "    print(pair[0])",
    ""
  ].join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.equal(analysis.diagnostics.length, 1);
    assert.deepEqual(analysis.diagnostics[0], {
      code: "AU3005",
      edits: [],
      end_character: 11,
      help: [],
      line: 2,
      message:
        "cannot consume non-copy tuple element `str` by indexing; unpack the tuple to move its elements",
      notes: [],
      secondary_spans: [],
      severity: 1,
      start_character: 10,
      call_frames: [],
      task_ancestry: []
    });

    const [diagnostic] = compilerDiagnosticsToLsp(analysis, mainUri);
    assert.equal(diagnostic.code, "AU3005");
    assert.equal(diagnostic.source, "aura-compiler");
    assert.equal(
      diagnostic.message,
      "cannot consume non-copy tuple element `str` by indexing; unpack the tuple to move its elements"
    );
    assert.deepEqual(diagnostic.range, {
      start: { line: 2, character: 10 },
      end: { line: 2, character: 11 }
    });
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes structural tuple equality and ordering diagnostics", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-tuple-equality-"));
  const sourceLines = [
    "def inspect():",
    "    left: (int32, str) = (1, \"left\")",
    "    right: (int32, str) = (2, \"right\")",
    "    equal = left == right",
    "    not_equal = left != right",
    "    literal_on_right = left == (1, \"left\")",
    "    literal_on_left = (2, \"right\") != right",
    "    print(left[0])",
    "    print(right[0])",
    "    print(equal)",
    "    print(not_equal)",
    "    print(literal_on_right)",
    "    print(literal_on_left)",
    ""
  ];
  const source = sourceLines.join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);

    for (const [name, line, start, end] of [
      ["equal", 9, 10, 15],
      ["not_equal", 10, 10, 19],
      ["literal_on_right", 11, 10, 26],
      ["literal_on_left", 12, 10, 25]
    ]) {
      assert.deepEqual(compilerHoverAtPosition(analysis, line, start + 1), {
        value: `\`\`\`aura\nbinding ${name}: bool\n\`\`\``,
        range: {
          start: { line, character: start },
          end: { line, character: end }
        }
      });
    }

    const definitionRanges = {
      left: {
        start: { line: 1, character: 4 },
        end: { line: 1, character: 8 }
      },
      right: {
        start: { line: 2, character: 4 },
        end: { line: 2, character: 9 }
      }
    };
    for (const [name, line, useLastOccurrence] of [
      ["left", 3, false],
      ["right", 3, false],
      ["left", 4, false],
      ["right", 4, false],
      ["left", 5, false],
      ["right", 6, true],
      ["left", 7, false],
      ["right", 8, false]
    ]) {
      const start = useLastOccurrence
        ? sourceLines[line].lastIndexOf(name)
        : sourceLines[line].indexOf(name);
      const occurrence = analysis.occurrences.find(
        (candidate) =>
          candidate.line === line &&
          candidate.start_character === start &&
          candidate.end_character === start + name.length
      );
      assert.ok(occurrence, `missing tuple operand occurrence for ${name} on line ${line}`);
      assert.equal(
        occurrence.hover,
        `\`\`\`aura\nbinding ${name}: (int32, str)\n\`\`\``
      );
      assert.deepEqual(
        compilerDefinitionAtPosition(mainUri, analysis, line, start + 1)?.range,
        definitionRanges[name]
      );
    }

    const orderingSource = [
      "def compare(left: (int32, str), right: (int32, str)):",
      "    ordered = left < right",
      ""
    ].join("\n");
    const orderingAnalysis = await analyzeWithCompiler(mainUri, orderingSource);

    assert.ok(orderingAnalysis);
    assert.equal(orderingAnalysis.diagnostics.length, 1);
    const [rawDiagnostic] = orderingAnalysis.diagnostics;
    assert.equal(rawDiagnostic.code, "AU2003");
    assert.equal(
      rawDiagnostic.message,
      "tuple ordering is not supported; use `==` or `!=`, or compare tuple elements explicitly"
    );
    assert.deepEqual(
      {
        line: rawDiagnostic.line,
        start_character: rawDiagnostic.start_character,
        end_character: rawDiagnostic.end_character
      },
      { line: 1, start_character: 14, end_character: 15 }
    );

    const [diagnostic] = compilerDiagnosticsToLsp(orderingAnalysis, mainUri);
    assert.equal(diagnostic.source, "aura-compiler");
    assert.equal(diagnostic.code, "AU2003");
    assert.equal(diagnostic.message, rawDiagnostic.message);
    assert.deepEqual(diagnostic.range, {
      start: { line: 1, character: 14 },
      end: { line: 1, character: 15 }
    });
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes contextual lambda scope, hover, definitions, and completions", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-lambda-scope-"));
  const sourceLines = [
    "def main():",
    "    offset: int32 = 40",
    "    add: def(int32) -> int32 = lambda value: value + offset",
    "    print(add(2))",
    ""
  ];
  const source = sourceLines.join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);

    const lambdaParameterDeclaration = sourceLines[2].indexOf("value");
    const lambdaParameterUse = sourceLines[2].lastIndexOf("value");
    const parameterOccurrence = analysis.occurrences.find(
      (occurrence) =>
        occurrence.line === 2 &&
        occurrence.start_character === lambdaParameterUse &&
        occurrence.end_character === lambdaParameterUse + "value".length
    );
    assert.ok(parameterOccurrence, "the lambda parameter use should be in semantic analysis");
    assert.match(parameterOccurrence.hover, /value: int32/);
    assert.deepEqual(parameterOccurrence.definition, {
      file_path: null,
      line: 2,
      start_character: lambdaParameterDeclaration,
      end_character: lambdaParameterDeclaration + "value".length
    });

    const captureUse = sourceLines[2].lastIndexOf("offset");
    const captureOccurrence = analysis.occurrences.find(
      (occurrence) =>
        occurrence.line === 2 &&
        occurrence.start_character === captureUse &&
        occurrence.end_character === captureUse + "offset".length
    );
    assert.ok(captureOccurrence, "a captured local should retain hover and navigation");
    assert.match(captureOccurrence.hover, /offset: int32/);
    assert.ok(
      captureOccurrence.definition.file_path.endsWith("/main.au"),
      "captured locals should navigate within the analyzed file"
    );
    assert.deepEqual({
      ...captureOccurrence.definition,
      file_path: null
    }, {
      file_path: null,
      line: 1,
      start_character: sourceLines[1].indexOf("offset"),
      end_character: sourceLines[1].indexOf("offset") + "offset".length
    });

    const lambdaBinding = analysis.occurrences.find(
      (occurrence) =>
        occurrence.line === 3 &&
        occurrence.hover.includes("add") &&
        occurrence.hover.includes("def(int32) -> int32")
    );
    assert.ok(lambdaBinding, "the closure binding should expose its callable contract");

    const completionLines = [
      "def main():",
      "    offset: int32 = 40",
      "    add: def(int32) -> int32 = lambda value: value + offset",
      ""
    ];
    const completionSource = completionLines.join("\n");
    const completions = await completeWithCompiler(
      mainUri,
      completionSource,
      2,
      completionLines[2].length,
      null
    );
    assert.ok(completions);
    const completionNames = new Set(completions.map((item) => item.name));
    assert.ok(completionNames.has("value"), "lambda parameters belong to the body scope");
    assert.ok(completionNames.has("offset"), "outer locals remain visible for capture");
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes ADR-0038 view provenance and returned-view contracts", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-place-views-"));
  const sourceLines = [
    "class User:",
    "    name: str",
    "",
    "def name(user: User) -> view str from user:",
    "    return view user.name",
    "",
    "def main():",
    "    user = User(name=\"Ada\")",
    "    view display = name(user)",
    "    print(display)",
    ""
  ];
  const source = sourceLines.join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);
    assert.ok(
      analysis.occurrences.some((occurrence) =>
        occurrence.hover.includes("function name(user: User) -> view str from user")
      ),
      "returned-view function hover should retain its origin contract"
    );
    const display = analysis.occurrences.find((occurrence) =>
      occurrence.hover.includes("view display: str from <place>")
    );
    assert.ok(display, "local view hover should expose kind, pointee type, and provenance");
    assert.ok(display.definition, "a view use should navigate to its source place");
    assert.ok(
      analysis.symbols.some(
        (symbol) => symbol.name === "name" && symbol.detail === "view str from user"
      ),
      "document symbols should expose returned-view metadata"
    );
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes progressively scoped comprehension intelligence", async () => {
  const tempRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), "aura-lsp-comprehension-scope-")
  );
  const sourceLines = [
    "def collect_lengths(groups: list[list[str]]) -> list[int64]:",
    "    lengths = [",
    "        entry.len()",
    "        for group in groups",
    "        if group.len() > 0",
    "        for entry in group",
    "        if entry.contains(\"a\")",
    "    ]",
    "    print(lengths)",
    "    return lengths",
    ""
  ];
  const source = sourceLines.join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;
    const analysis = await analyzeWithCompiler(mainUri, source);

    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);

    const outputEntryStart = sourceLines[2].indexOf("entry");
    const entryTargetStart = sourceLines[5].indexOf("entry");
    const entryHover = compilerHoverAtPosition(analysis, 2, outputEntryStart + 1);
    assert.deepEqual(entryHover, {
      value: "```aura\nlocal entry: str\n```",
      range: {
        start: { line: 2, character: outputEntryStart },
        end: { line: 2, character: outputEntryStart + "entry".length }
      }
    });
    assert.deepEqual(
      compilerDefinitionAtPosition(mainUri, analysis, 2, outputEntryStart + 1)?.range,
      {
        start: { line: 5, character: entryTargetStart },
        end: { line: 5, character: entryTargetStart + "entry".length }
      },
      "the output occurrence must navigate to the target after `for`, not to itself"
    );

    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover === "```aura\nbinding lengths: list[int64]\n```"
      ),
      "the eager owned comprehension result should retain its checked collection type"
    );

    const completionNamesAt = async (line, character, trigger = null) => {
      const completions = await completeWithCompiler(
        mainUri,
        source,
        line,
        character,
        trigger
      );
      assert.ok(completions);
      return new Set(completions.map((item) => item.name));
    };

    const outputMembers = await completionNamesAt(
      2,
      sourceLines[2].indexOf(".") + 1,
      "."
    );
    assert.ok(outputMembers.has("len"));
    assert.ok(outputMembers.has("contains"));

    const outerFilter = await completionNamesAt(
      4,
      sourceLines[4].indexOf("group") + 2
    );
    assert.ok(outerFilter.has("group"));
    assert.equal(outerFilter.has("entry"), false);

    const innerIterable = await completionNamesAt(
      5,
      sourceLines[5].lastIndexOf("group") + 2
    );
    assert.ok(innerIterable.has("group"));
    assert.equal(innerIterable.has("entry"), false);

    const innerFilter = await completionNamesAt(
      6,
      sourceLines[6].indexOf("entry") + 2
    );
    assert.ok(innerFilter.has("group"));
    assert.ok(innerFilter.has("entry"));

    const afterComprehension = await completionNamesAt(8, sourceLines[8].length);
    assert.equal(afterComprehension.has("group"), false);
    assert.equal(afterComprehension.has("entry"), false);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves owned slice result types, endpoint intelligence, and diagnostics", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-owned-slices-"));
  const sourceLines = [
    "def take_slice(values: list[str], start: int64, end: int64) -> list[str]:",
    "    selected = values[start:end]",
    "    return selected",
    ""
  ];
  const source = sourceLines.join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);

    for (const [name, hover] of [
      ["values", "param values: list[str]"],
      ["start", "param start: int64"],
      ["end", "param end: int64"]
    ]) {
      const startCharacter = sourceLines[1].indexOf(name);
      assert.equal(
        compilerHoverAtPosition(analysis, 1, startCharacter + 1)?.value,
        `\`\`\`aura\n${hover}\n\`\`\``
      );
    }
    assert.ok(
      analysis.occurrences.some(
        (occurrence) =>
          occurrence.hover === "```aura\nbinding selected: list[str]\n```"
      ),
      "an owned list slice should preserve the ordinary list result type"
    );

    const completionLine = "    values[start:end].";
    const completionSource = [
      sourceLines[0],
      completionLine,
      "    return values[:]",
      ""
    ].join("\n");
    const completions = await completeWithCompiler(
      mainUri,
      completionSource,
      1,
      completionLine.length,
      "."
    );
    const names = new Set(completions.map((item) => item.name));
    assert.ok(names.has("append"));
    assert.ok(names.has("len"));

    for (const receiver of [
      "make_values()[1:]",
      "values[endpoint(\"]\"):]"
    ]) {
      const receiverLine = `    ${receiver}.`;
      const receiverSource = [
        "def make_values() -> list[str]:",
        "    return [\"Ada\", \"Grace\"]",
        "",
        "def endpoint(text: str) -> int64:",
        "    return 0",
        "",
        "def inspect(values: list[str]):",
        receiverLine,
        ""
      ].join("\n");
      const receiverCompletions = await completeWithCompiler(
        mainUri,
        receiverSource,
        7,
        receiverLine.length,
        "."
      );
      assert.ok(
        receiverCompletions,
        `completion should recover the slice receiver ${receiver}`
      );
      const receiverNames = new Set(receiverCompletions.map((item) => item.name));
      assert.ok(receiverNames.has("append"), receiver);
      assert.ok(receiverNames.has("len"), receiver);
    }

    const stepped = await analyzeWithCompiler(
      `file://${path.join(tempRoot, "stepped.au")}`,
      [
        "def reject(values: list[int32]):",
        "    print(values[::2])",
        ""
      ].join("\n")
    );
    assert.equal(stepped.diagnostics.length, 1);
    assert.equal(stepped.diagnostics[0].code, "AU2005");
    assert.equal(
      stepped.diagnostics[0].message,
      "slice steps are unavailable; use an explicit loop to select a stride"
    );

    for (const diagnosticCase of [
      {
        name: "endpoint-type",
        source: [
          "def reject(values: list[int32], endpoint: uint64):",
          "    print(values[endpoint:])",
          ""
        ].join("\n"),
        expected: {
          code: "AU2002",
          message: "slice endpoints must have type `int64` or a losslessly narrower integer type, found `uint64`",
          line: 1,
          startCharacter: 17,
          endCharacter: 18
        }
      },
      {
        name: "assignment",
        source: [
          "def replace(values: list[int32]):",
          "    values[1:3] = values",
          ""
        ].join("\n"),
        expected: {
          code: "AU2005",
          message:
            "slice assignment is unavailable because slices are owned copies; mutate the source by index or build a new value",
          line: 1,
          startCharacter: 12,
          endCharacter: 13
        }
      }
    ]) {
      const rejected = await analyzeWithCompiler(
        `file://${path.join(tempRoot, `${diagnosticCase.name}.au`)}`,
        diagnosticCase.source
      );
      assert.equal(rejected.diagnostics.length, 1);
      assert.deepEqual(
        {
          code: rejected.diagnostics[0].code,
          message: rejected.diagnostics[0].message,
          line: rejected.diagnostics[0].line,
          startCharacter: rejected.diagnostics[0].start_character,
          endCharacter: rejected.diagnostics[0].end_character
        },
        diagnosticCase.expected
      );
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves incomplete comprehension diagnostics without stale intelligence", async () => {
  const tempRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), "aura-lsp-incomplete-comprehension-")
  );
  const cases = [
    {
      name: "iterable",
      line: "    result = [value for value in ]",
      message: "expected an iterable expression after `in` in comprehension",
      startCharacter: 33
    },
    {
      name: "filter",
      line: "    result = [value for value in values if ]",
      message: "expected a filter expression after `if` in comprehension",
      startCharacter: 43
    }
  ];

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    for (const edit of cases) {
      const sourceLines = [
        "def collect(values: list[int64]) -> list[int64]:",
        edit.line,
        "    return result",
        ""
      ];
      const source = sourceLines.join("\n");
      const uri = `file://${path.join(tempRoot, `${edit.name}.au`)}`;
      const analysis = await analyzeWithCompiler(uri, source);

      assert.ok(analysis);
      assert.equal(analysis.diagnostics.length, 1);
      assert.deepEqual(
        {
          code: analysis.diagnostics[0].code,
          message: analysis.diagnostics[0].message,
          line: analysis.diagnostics[0].line,
          startCharacter: analysis.diagnostics[0].start_character,
          endCharacter: analysis.diagnostics[0].end_character
        },
        {
          code: "AU1101",
          message: edit.message,
          line: 1,
          startCharacter: edit.startCharacter,
          endCharacter: edit.startCharacter + 1
        }
      );
      assert.deepEqual(analysis.occurrences, []);
      assert.equal(
        compilerHoverAtPosition(analysis, 0, sourceLines[0].indexOf("values") + 1),
        null,
        "a parse-incomplete expression must not leak stale checked hover metadata"
      );

      const completions = await completeWithCompiler(
        uri,
        source,
        1,
        sourceLines[1].indexOf("]"),
        null
      );
      assert.equal(
        completions,
        null,
        "a rejected compiler completion must select the server's lexical recovery path"
      );
    }
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge preserves closure capture ownership diagnostics and guidance", async () => {
  const tempRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), "aura-lsp-lambda-diagnostics-")
  );

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainPath = path.join(tempRoot, "main.au");
    const mainUri = `file://${mainPath}`;

    const movedCapture = [
      "def main():",
      "    text = \"captured\"",
      "    length: def() -> int64 = lambda: text.len()",
      "    print(text)",
      "    print(length())",
      ""
    ].join("\n");
    const movedAnalysis = await analyzeWithCompiler(mainUri, movedCapture);
    assert.ok(movedAnalysis);
    assert.equal(movedAnalysis.diagnostics.length, 1);
    assert.equal(movedAnalysis.diagnostics[0].code, "AU3001");
    assert.match(movedAnalysis.diagnostics[0].message, /captur|mov/i);

    const sharedCapture = [
      "def make_length(text: str) -> def() -> int64:",
      "    return lambda: text.len()",
      ""
    ].join("\n");
    const sharedAnalysis = await analyzeWithCompiler(mainUri, sharedCapture);
    assert.ok(sharedAnalysis);
    assert.equal(sharedAnalysis.diagnostics.length, 1);
    assert.match(sharedAnalysis.diagnostics[0].message, /captur/i);
    assert.ok(
      sharedAnalysis.diagnostics[0].help.some(
        (help) => /clone/i.test(help) || /\bown\b/.test(help)
      ),
      `shared-capability capture should suggest cloning or ownership: ${JSON.stringify(
        sharedAnalysis.diagnostics[0]
      )}`
    );

    const mutableCapture = [
      "def main():",
      "    mut values = list[int32]()",
      "    push: def(int32) -> None = lambda value: values.append(value)",
      "    push(1)",
      ""
    ].join("\n");
    const mutableAnalysis = await analyzeWithCompiler(mainUri, mutableCapture);
    assert.ok(mutableAnalysis);
    assert.equal(mutableAnalysis.diagnostics.length, 1);
    assert.match(mutableAnalysis.diagnostics[0].message, /captur|mutable/i);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("compiler bridge exposes the global numeric Array surface and result types", async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aura-lsp-numeric-arrays-"));
  const sourceLines = [
    "def widen(value: int32) -> float64:",
    "    return value.to_float()",
    "",
    "def transform() -> float64:",
    "    left = Array[int32].from_list([1, 2, 3, 4], [2, 2])",
    "    right = Array[int32].full([2, 2], 5)",
    "    combined = left.wrapping_add(right)",
    "    scaled = combined * 2",
    "    item = scaled[0, 1]",
    "    maybe_item = scaled.get([0, 1])",
    "    dimensions = scaled.shape()",
    "    length = scaled.len()",
    "    first_row = scaled[0:1]",
    "    mapped = scaled.map[float64](widen)",
    "    mut writable = scaled.clone()",
    "    replaced = writable.set([0, 1], 9)",
    "    total = scaled.sum()",
    "    match replaced:",
    "        case Option.Some(previous):",
    "            print(previous)",
    "        case Option.None:",
    "            print(-1)",
    "    wrapped_scalar = scaled.wrapping_add(2147483647)",
    "    saturated_scalar = scaled.saturating_add(2147483647)",
    "    float_values = Array[float64].full([2], 4.0) / 2.0",
    "    average = mapped.mean()",
    "    return average",
    ""
  ];
  const source = sourceLines.join("\n");

  try {
    setWorkspaceRoots([repoRoot, tempRoot]);
    const mainUri = `file://${path.join(tempRoot, "main.au")}`;
    const analysis = await analyzeWithCompiler(mainUri, source);
    assert.ok(analysis);
    assert.deepEqual(analysis.diagnostics, []);

    for (const expected of [
      "binding left: Array[int32]",
      "binding combined: Array[int32]",
      "binding scaled: Array[int32]",
      "binding item: int32",
      "binding maybe_item: Option[int32]",
      "binding dimensions: list[int64]",
      "binding length: int64",
      "binding first_row: Array[int32]",
      "binding mapped: Array[float64]",
      "binding writable: Array[int32]",
      "binding replaced: Option[int32]",
      "binding total: int32",
      "binding float_values: Array[float64]",
      "binding average: float64"
    ]) {
      assert.ok(
        analysis.occurrences.some(
          (occurrence) => occurrence.hover === `\`\`\`aura\n${expected}\n\`\`\``
        ),
        `missing checked Array hover: ${expected}`
      );
    }

    const memberLine = "    scaled.";
    const memberSource = [
      "def inspect():",
      "    scaled = Array[int32].zeros([2, 2])",
      memberLine,
      ""
    ].join("\n");
    const memberItems = await completeWithCompiler(
      mainUri,
      memberSource,
      2,
      memberLine.length,
      "."
    );
    const memberNames = new Set(memberItems.map((item) => item.name));
    for (const expected of [
      "shape",
      "len",
      "clone",
      "get",
      "set",
      "fill",
      "map",
      "sum",
      "min",
      "max",
      "mean",
      "wrapping_add",
      "wrapping_sub",
      "wrapping_mul",
      "saturating_add",
      "saturating_sub",
      "saturating_mul"
    ]) {
      assert.ok(memberNames.has(expected), `Array completion should include ${expected}`);
    }

    const staticLine = "    Array[float64].";
    const staticSource = ["def construct():", staticLine, ""].join("\n");
    const staticItems = await completeWithCompiler(
      mainUri,
      staticSource,
      1,
      staticLine.length,
      "."
    );
    const staticNames = new Set(staticItems.map((item) => item.name));
    assert.deepEqual(
      [...staticNames].filter((name) => ["zeros", "full", "from_list"].includes(name)).sort(),
      ["from_list", "full", "zeros"]
    );

    const rejected = await analyzeWithCompiler(
      `file://${path.join(tempRoot, "integer-division.au")}`,
      [
        "def reject():",
        "    values = Array[int64].full([2], 4)",
        "    print(values / 2)",
        ""
      ].join("\n")
    );
    assert.equal(rejected.diagnostics.length, 1);
    assert.equal(rejected.diagnostics[0].code, "AU2003");
    assert.equal(rejected.diagnostics[0].message, "integer Array `/` is not supported");
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});
