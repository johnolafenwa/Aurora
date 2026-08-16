"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { computeAuraNewlineIndent } = require("../src/indentation");

test("extension bundle contains built extension and language server files", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const distFiles = ["extension.js", "server.js"];

  for (const filename of distFiles) {
    const fullPath = path.join(extensionRoot, "dist", filename);
    assert.equal(fs.existsSync(fullPath), true, `${filename} should exist in extension/dist`);
  }
});

test("extension manifest and listing are ready for both public marketplaces", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const manifest = JSON.parse(
    fs.readFileSync(path.join(extensionRoot, "package.json"), "utf8")
  );
  const readme = fs.readFileSync(path.join(extensionRoot, "README.md"), "utf8");

  assert.equal(manifest.name, "vscode-aura-lang");
  assert.equal(manifest.publisher, "JohnOlafenwa");
  assert.equal(manifest.displayName, "Aura Programming Language");
  assert.equal(manifest.version, "0.3.3");
  assert.equal(manifest.preview, true);
  assert.equal(manifest.private, undefined);
  assert.equal(manifest.icon, "images/aura.png");
  assert.deepEqual(manifest.categories, ["Programming Languages"]);
  assert.deepEqual(manifest.keywords, [
    "aura",
    "compiled language",
    "python-like",
    "structured concurrency",
    "lsp"
  ]);
  assert.match(manifest.description, /compiled, statically typed programming language/);
  assert.doesNotMatch(manifest.description, /systems programming language/);
  assert.deepEqual(manifest.repository, {
    type: "git",
    url: "https://github.com/johnolafenwa/Aura.git",
    directory: "tools/vscode-aura"
  });
  assert.deepEqual(manifest.bugs, {
    url: "https://github.com/johnolafenwa/Aura/issues"
  });
  assert.equal(manifest.homepage, "https://github.com/johnolafenwa/Aura#readme");
  assert.ok(manifest.files.includes("LICENSE"));
  assert.ok(manifest.files.includes("images/aura.png"));
  assert.equal(fs.existsSync(path.join(extensionRoot, "LICENSE")), true);
  assert.equal(fs.existsSync(path.join(extensionRoot, manifest.icon)), true);
  const auraLanguage = manifest.contributes.languages.find(
    (language) => language.id === "aura"
  );
  assert.deepEqual(auraLanguage?.icon, {
    light: "./images/aura.png",
    dark: "./images/aura.png"
  });
  for (const iconPath of Object.values(auraLanguage.icon)) {
    assert.equal(
      fs.existsSync(path.resolve(extensionRoot, iconPath)),
      true,
      `${iconPath} should exist for Aura files in the Explorer`
    );
  }
  assert.match(
    readme.split("\n").slice(0, 12).join("\n"),
    /Aura is a compiled, statically typed programming language[\s\S]*https:\/\/github\.com\/johnolafenwa\/Aura/i
  );
  assert.doesNotMatch(manifest.scripts["package:vsix"], /allow-missing-repository|skip-license/);
});

test("extension package includes the assertion-aware Aura grammar", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const manifest = JSON.parse(
    fs.readFileSync(path.join(extensionRoot, "package.json"), "utf8")
  );
  const grammarContribution = manifest.contributes.grammars.find(
    (grammar) => grammar.language === "aura"
  );

  assert.ok(manifest.files.includes("syntaxes/**"));
  assert.equal(grammarContribution?.path, "./syntaxes/aura.tmLanguage.json");
  const packagedGrammar = fs.readFileSync(
    path.join(extensionRoot, grammarContribution.path),
    "utf8"
  );
  assert.match(packagedGrammar, /assert/);
});

test("extension grammar and snippets cover Aura 0.3 string forms", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const grammar = JSON.parse(
    fs.readFileSync(path.join(extensionRoot, "syntaxes", "aura.tmLanguage.json"), "utf8")
  );
  const names = grammar.repository.strings.patterns.map((pattern) => pattern.name);
  assert.ok(names.includes("string.quoted.triple.double.aura"));
  assert.ok(names.includes("string.quoted.triple.single.aura"));
  assert.ok(names.includes("string.quoted.raw.double.aura"));
  assert.ok(names.includes("string.quoted.raw.single.aura"));
  const fstring = grammar.repository.strings.patterns.find(
    (pattern) => pattern.name === "string.interpolated.double.aura"
  );
  assert.match(JSON.stringify(fstring), /meta\.format-spec\.aura/);

  const snippets = JSON.parse(
    fs.readFileSync(path.join(extensionRoot, "snippets", "aura.json"), "utf8")
  );
  assert.ok(snippets["Multiline string"]);
  assert.ok(snippets["Formatted string"]);
});

test("language configuration indents block headers on enter without blank-line dedent", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const configurationPath = path.join(extensionRoot, "language-configuration.json");
  const configuration = JSON.parse(fs.readFileSync(configurationPath, "utf8"));

  assert.match(
    configuration.indentationRules.increaseIndentPattern,
    /class\|enum\|trait\|def\|if\|elif\|else\|while\|for\|match\|case\|with\|impl/
  );
  assert.equal(
    Object.prototype.hasOwnProperty.call(configuration.indentationRules, "decreaseIndentPattern"),
    false,
    "blank lines should not be treated as a dedent signal"
  );
  assert.ok(Array.isArray(configuration.onEnterRules), "onEnterRules should be configured");
  assert.ok(configuration.onEnterRules.length > 0, "at least one onEnterRules entry is required");
  assert.equal(configuration.onEnterRules[0].action.indent, "indent");
});

test("syntax grammar treats boolean operators as Aura keywords", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const grammarPath = path.join(extensionRoot, "syntaxes", "aura.tmLanguage.json");
  const grammar = JSON.parse(fs.readFileSync(grammarPath, "utf8"));
  const keywordRule = grammar.repository.keywords.patterns.find(
    (pattern) => pattern.name === "keyword.control.aura"
  );

  assert.ok(keywordRule);
  assert.match(keywordRule.match, /and\|or\|not/);
  assert.match(keywordRule.match, /pass/);
  assert.match(keywordRule.match, /assert/);
  assert.match(keywordRule.match, /lambda/);
});

test("extension packages an expression-lambda snippet", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const snippets = JSON.parse(
    fs.readFileSync(path.join(extensionRoot, "snippets", "aura.json"), "utf8")
  );

  assert.deepEqual(snippets.Lambda.body, ["lambda ${1:value}: ${2:expression}"]);
  assert.match(snippets.Lambda.description, /expression/i);
});

test("extension highlights and snippets ADR-0038 views and loan captures", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const grammar = JSON.parse(
    fs.readFileSync(path.join(extensionRoot, "syntaxes", "aura.tmLanguage.json"), "utf8")
  );
  const snippets = JSON.parse(
    fs.readFileSync(path.join(extensionRoot, "snippets", "aura.json"), "utf8")
  );
  const viewRule = grammar.repository.keywords.patterns.find(
    (pattern) => pattern.name === "keyword.declaration.view.aura"
  );

  assert.ok(viewRule);
  assert.equal(new RegExp(viewRule.match).test("view"), true);
  assert.equal(new RegExp(viewRule.match).test("from"), true);
  assert.deepEqual(snippets["Shared view"].body, ["view ${1:name} = ${2:place}"]);
  assert.deepEqual(snippets["Mutable view"].body, [
    "view mut ${1:name} = ${2:place}"
  ]);
  assert.match(snippets["Loan capture lambda"].body[0], /lambda \[.*mut.*own/);
  assert.match(snippets["Returned view function"].body[0], /-> view.*from/);
});

test("extension highlights and snippets the maintained extern C surface", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const grammar = JSON.parse(
    fs.readFileSync(
      path.join(extensionRoot, "syntaxes", "aura.tmLanguage.json"),
      "utf8"
    )
  );
  const keywordRule = grammar.repository.keywords.patterns.find(
    (pattern) => pattern.name === "keyword.declaration.ffi.aura"
  );
  const declarationRules = grammar.repository.declarations.patterns;
  const snippets = JSON.parse(
    fs.readFileSync(path.join(extensionRoot, "snippets", "aura.json"), "utf8")
  );

  assert.ok(keywordRule);
  assert.equal(new RegExp(keywordRule.match).test("extern"), true);
  assert.equal(new RegExp(keywordRule.match).test("opaque"), true);
  const functionRule = declarationRules.find(
    (pattern) => pattern.name === "meta.definition.ffi.function.aura"
  );
  const opaqueRule = declarationRules.find(
    (pattern) => pattern.name === "meta.definition.ffi.opaque.aura"
  );
  assert.ok(functionRule);
  assert.ok(opaqueRule);

  const functionMatch = new RegExp(functionRule.match).exec(
    'extern "C" def getpid'
  );
  assert.deepEqual(functionMatch?.slice(1), ["extern", '"C"', "def", "getpid"]);
  assert.equal(functionRule.captures["1"].name, "keyword.declaration.ffi.aura");
  assert.equal(functionRule.captures["2"].name, "string.quoted.double.aura");
  assert.equal(functionRule.captures["3"].name, "keyword.declaration.function.aura");
  assert.equal(functionRule.captures["4"].name, "entity.name.function.aura");

  const opaqueMatch = new RegExp(opaqueRule.match).exec(
    'extern "C" opaque class native_handle'
  );
  assert.deepEqual(opaqueMatch?.slice(1), [
    "extern",
    '"C"',
    "opaque",
    "class",
    "native_handle"
  ]);
  assert.equal(opaqueRule.captures["1"].name, "keyword.declaration.ffi.aura");
  assert.equal(opaqueRule.captures["2"].name, "string.quoted.double.aura");
  assert.equal(opaqueRule.captures["3"].name, "keyword.declaration.ffi.aura");
  assert.equal(opaqueRule.captures["4"].name, "keyword.declaration.aura");
  assert.equal(opaqueRule.captures["5"].name, "entity.name.type.aura");
  assert.deepEqual(snippets["Extern C function"].body, [
    'public extern "C" def ${1:name}(${2}) -> ${3:int32}'
  ]);
  assert.deepEqual(snippets["Extern C opaque handle"].body, [
    'public extern "C" opaque class ${1:Handle}'
  ]);
});

test("syntax grammar highlights the current storage modifiers", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const grammarPath = path.join(extensionRoot, "syntaxes", "aura.tmLanguage.json");
  const grammar = JSON.parse(fs.readFileSync(grammarPath, "utf8"));
  const modifierRule = grammar.repository.keywords.patterns.find(
    (pattern) => pattern.name === "storage.modifier.aura"
  );

  assert.ok(modifierRule);
  const modifierPattern = new RegExp(modifierRule.match);
  assert.equal(modifierPattern.test("mut"), true);
  assert.equal(modifierPattern.test("own"), true);
});

test("syntax grammar distinguishes ordinary quotes and nests strings in f-string interpolation", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const grammarPath = path.join(extensionRoot, "syntaxes", "aura.tmLanguage.json");
  const grammar = JSON.parse(fs.readFileSync(grammarPath, "utf8"));
  const stringRules = grammar.repository.strings.patterns;
  const fStringRule = stringRules.find(
    (pattern) => pattern.name === "string.interpolated.double.aura"
  );
  const doubleRule = stringRules.find(
    (pattern) => pattern.name === "string.quoted.double.aura"
  );
  const singleRule = stringRules.find(
    (pattern) => pattern.name === "string.quoted.single.aura"
  );

  assert.ok(fStringRule);
  assert.equal(fStringRule.begin, 'f"');
  assert.ok(doubleRule);
  assert.equal(doubleRule.begin, '"');
  assert.ok(singleRule);
  assert.equal(singleRule.begin, "'");

  const interpolation = fStringRule.patterns.find(
    (pattern) => pattern.name === "meta.interpolation.aura"
  );
  assert.ok(interpolation);
  assert.equal(interpolation.contentName, "meta.embedded.expression.aura");
  assert.match(
    interpolation.beginCaptures["0"].name,
    /constant\.character\.format\.placeholder\.other\.aura/
  );
  assert.match(
    interpolation.endCaptures["0"].name,
    /constant\.character\.format\.placeholder\.other\.aura/
  );
  assert.ok(
    interpolation.patterns.some((pattern) => pattern.include === "#strings"),
    "f-string interpolations should recognize nested ordinary strings"
  );
  assert.ok(
    interpolation.patterns.some(
      (pattern) => pattern.name === "variable.other.readwrite.aura"
    ),
    "f-string interpolation identifiers should receive expression scopes"
  );
  assert.ok(
    fStringRule.patterns.some(
      (pattern) => pattern.name === "constant.character.escape.aura"
        && pattern.match === "\\{\\{|\\}\\}"
    ),
    "doubled braces should remain literal f-string text"
  );

  const configurationPath = path.join(extensionRoot, "language-configuration.json");
  const configuration = JSON.parse(fs.readFileSync(configurationPath, "utf8"));
  assert.ok(configuration.autoClosingPairs.some(([open, close]) => open === "'" && close === "'"));
  assert.ok(configuration.autoClosingPairs.some(([open, close]) => open === '"' && close === '"'));
  assert.ok(configuration.surroundingPairs.some(([open, close]) => open === "'" && close === "'"));
  assert.ok(configuration.surroundingPairs.some(([open, close]) => open === '"' && close === '"'));
});

test("syntax grammar treats floor-division operators as single tokens", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const grammarPath = path.join(extensionRoot, "syntaxes", "aura.tmLanguage.json");
  const grammar = JSON.parse(fs.readFileSync(grammarPath, "utf8"));
  const operatorRule = grammar.repository.operators.patterns.find(
    (pattern) => pattern.name === "keyword.operator.aura"
  );

  assert.ok(operatorRule);
  const operatorPattern = new RegExp(operatorRule.match);
  assert.equal("//=".match(operatorPattern)?.[0], "//=", "//= should be one operator token");
  assert.equal("//".match(operatorPattern)?.[0], "//", "// should be one operator token");
  assert.equal("%=".match(operatorPattern)?.[0], "%=", "%= should be one operator token");
});

test("syntax grammar tracks maintained builtin types", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const grammarPath = path.join(extensionRoot, "syntaxes", "aura.tmLanguage.json");
  const grammar = JSON.parse(fs.readFileSync(grammarPath, "utf8"));
  const typeRule = grammar.repository.types.patterns.find(
    (pattern) => pattern.name === "support.type.primitive.aura"
  );

  assert.ok(typeRule);
  const typePattern = new RegExp(typeRule.match);
  for (const typeName of [
    "int",
    "int32",
    "int64",
    "Queue",
    "QueueReceive",
    "TaskResult",
    "SelectOutcome",
    "WaitAny",
    "WaitAll",
    "list",
    "dict",
    "set",
    "str",
    "Array",
    "process.Child",
    "fs.File",
    "net.TcpStream"
  ]) {
    assert.equal(typePattern.test(typeName), true, `${typeName} should be highlighted as a type`);
  }
  assert.doesNotMatch(typeRule.match, /Channel/);
});

test("syntax grammar highlights maintained builtin function calls", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const grammar = JSON.parse(
    fs.readFileSync(path.join(extensionRoot, "syntaxes", "aura.tmLanguage.json"), "utf8")
  );
  const builtinRule = grammar.repository.builtins.patterns.find(
    (pattern) => pattern.name === "support.function.builtin.aura"
  );

  assert.ok(builtinRule);
  const builtinPattern = new RegExp(builtinRule.match);
  for (const name of [
    "print",
    "range",
    "len",
    "str",
    "select",
    "wait_any",
    "wait_all",
    "parse_int64"
  ]) {
    assert.equal(builtinPattern.test(`${name}(`), true, `${name} should be highlighted`);
  }
  assert.equal(builtinPattern.test("user_function("), false);
});

test("extension activation registers the language client itself as disposable", () => {
  const extensionRoot = path.resolve(__dirname, "..");
  const source = fs.readFileSync(path.join(extensionRoot, "src", "extension.js"), "utf8");

  assert.match(source, /async function activate\(context\)/);
  assert.match(source, /await client\.start\(\)/);
  assert.match(source, /context\.subscriptions\.push\(client\)/);
  assert.doesNotMatch(source, /subscriptions\.push\(client\.start\(\)\)/);
});

test("Aura newline indentation inherits the current block indent", () => {
  assert.equal(computeAuraNewlineIndent("def main():", "def main():".length, "    "), "    ");
  assert.equal(computeAuraNewlineIndent("    total = 1", "    total = 1".length, "    "), "    ");
  assert.equal(computeAuraNewlineIndent("        ", 8, "    "), "        ");
  assert.equal(computeAuraNewlineIndent("    if score < 10:", "    if score < 10:".length, "    "), "        ");
  assert.equal(computeAuraNewlineIndent("print(1)", "print(1)".length, "    "), "");
});

test("Aura newline indentation handles source delimiters", () => {
  for (const line of [
    "    total = add(",
    "    values = [",
    "    mapping = {"
  ]) {
    assert.equal(computeAuraNewlineIndent(line, line.length, "    "), "        ", line);
  }

  const nested = "    value = ([{";
  assert.equal(
    computeAuraNewlineIndent(nested, nested.length, "    "),
    "        ",
    "nested delimiters add one continuation level, not one level per delimiter"
  );

  for (const line of [
    "    total = add(value)",
    "    values = [1, 2]",
    "    mapping = {1: 2}",
    "    value = ([{}])"
  ]) {
    assert.equal(computeAuraNewlineIndent(line, line.length, "    "), "    ", line);
  }

  const textAfterCursor = "    value = (later)";
  assert.equal(
    computeAuraNewlineIndent(textAfterCursor, "    value = (".length, "    "),
    "        ",
    "only text before the cursor determines the inserted newline indentation"
  );
});

test("Aura newline indentation ignores delimiters in strings, f-strings, and comments", () => {
  for (const line of [
    '    text = "("',
    "    text = ']'",
    '    text = "escaped \\"(\\""',
    '    text = f"("',
    '    text = f"{value[0]}"',
    '    text = f"{echo("(")}"',
    "    value = call() # ([{",
    '    text = "# (" # ['
  ]) {
    assert.equal(computeAuraNewlineIndent(line, line.length, "    "), "    ", line);
  }

  const blockWithStringDelimiter = '    if label == "(":';
  assert.equal(
    computeAuraNewlineIndent(
      blockWithStringDelimiter,
      blockWithStringDelimiter.length,
      "    "
    ),
    "        ",
    "block headers retain their single indentation level"
  );
});

test("Aura newline indentation recognizes multiline block headers", () => {
  assert.equal(
    computeAuraNewlineIndent(
      "    ) -> int64:",
      "    ) -> int64:".length,
      "    ",
      ["def total(", "    left: int64,", "    right: int64"]
    ),
    "    "
  );
  assert.equal(
    computeAuraNewlineIndent(
      "    ):",
      "    ):".length,
      "    ",
      ["    if (", "        ready"]
    ),
    "        "
  );
  assert.equal(
    computeAuraNewlineIndent(
      "            )",
      "            )".length,
      "    ",
      ["    value = call(", "        1"]
    ),
    "    ",
    "closing a continued expression returns to the logical line's base indent"
  );
});
