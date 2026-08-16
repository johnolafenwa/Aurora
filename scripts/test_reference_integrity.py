#!/usr/bin/env python3
"""Behavioral tests for the Manual reference-integrity gate."""

from __future__ import annotations

import json
import re
import stat
import tempfile
import textwrap
import unittest
from pathlib import Path
from unittest import mock

import reference_integrity


REQUIRED_SECTIONS = (
    "Grammar",
    "Typing Rules",
    "Runtime Semantics",
    "Ownership And Evaluation Order",
    "Diagnostics",
    "Backend Support",
    "Limits And Implementation-Defined Behavior",
    "Status",
)


class ReferenceIntegrityTests(unittest.TestCase):
    def test_lsp_request_examples_pin_the_required_semantic_interface(self) -> None:
        root = Path(__file__).resolve().parent.parent
        cli = (root / "docs/manual/cli-and-tooling.md").read_text(encoding="utf-8")
        request_block = (
            cli.split("`aura lsp` is a persistent JSON-lines compiler service.", 1)[1]
            .split("```json", 1)[1]
            .split("```", 1)[0]
        )
        requests = [
            json.loads(line) for line in request_block.splitlines() if line.strip()
        ]

        self.assertEqual(len(requests), 2)
        self.assertEqual(
            [request["semantic_interface_version"] for request in requests],
            [6, 6],
        )
        lsp_readme = (root / "tools/aura-language-server/README.md").read_text(
            encoding="utf-8"
        )
        self.assertIn('"semantic_interface_version":6', lsp_readme)

    def test_cli_tooling_registry_matches_the_complete_diagnostic_code_table(
        self,
    ) -> None:
        root = Path(__file__).resolve().parent.parent
        diagnostics = (root / "docs/manual/diagnostics.md").read_text(
            encoding="utf-8"
        )
        cli = (root / "docs/manual/cli-and-tooling.md").read_text(encoding="utf-8")
        canonical_table = diagnostics.split("| Band |", 1)[1].split("\n\n", 1)[0]
        cli_registry = cli.split(
            "Compiler-backed commands can surface the complete append-only registry.",
            1,
        )[1].split(
            "The structured schema is defined in [Diagnostics](/manual/diagnostics).",
            1,
        )[0]

        self.assertEqual(
            set(re.findall(r"\bAU\d{4}\b", cli_registry)),
            set(re.findall(r"\bAU\d{4}\b", canonical_table)),
        )

    def test_default_cli_resolution_rebuilds_before_using_existing_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "target/debug/aura"
            binary.parent.mkdir(parents=True)
            binary.write_text("#!/bin/sh\n", encoding="utf-8")
            binary.chmod(binary.stat().st_mode | stat.S_IXUSR)

            with mock.patch.object(reference_integrity.subprocess, "run") as run:
                resolved = reference_integrity._resolve_aura_binary(root, None)

            self.assertEqual(resolved, binary)
            run.assert_called_once_with(
                ["cargo", "build", "--quiet", "-p", "aura"],
                cwd=root,
                check=True,
            )

    def test_inventory_extracts_only_aura_fences_with_stable_ordinals(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manual = Path(directory)
            (manual / "feature.md").write_text(
                textwrap.dedent(
                    """\
                    # Feature

                    ```bash
                    aura check example.au
                    ```

                    ```python
                    print("one")
                    ```

                    ```aura title="example.au"
                    print("two")
                    ```
                    """
                ),
                encoding="utf-8",
            )
            nested = manual / "nested"
            nested.mkdir()
            (nested / "detail.md").write_text(
                "# Detail\n\n```aura\nprint(\"nested\")\n```\n",
                encoding="utf-8",
            )

            inventory = reference_integrity.collect_manual(manual)

            self.assertEqual(inventory.page_count, 2)
            self.assertEqual(inventory.fence_count, 4)
            self.assertEqual(
                [block.identifier for block in inventory.aura_blocks],
                [
                    "docs/manual/feature.md#aura-1",
                    "docs/manual/nested/detail.md#aura-1",
                ],
            )
            self.assertEqual(inventory.aura_blocks[0].source, 'print("two")\n')
            self.assertIn("docs/manual/feature.md#python-1", {
                block.identifier for block in inventory.blocks
            })

    def test_metadata_rejects_stale_hash_and_unexplained_illustration(self) -> None:
        block = reference_integrity.ReferenceBlock(
            path="docs/manual/feature.md",
            ordinal=1,
            line=4,
            language="aura",
            source='print("hello")\n',
        )
        metadata = {
            "docs/manual/feature.md#aura-1": {
                "sha256": "stale",
                "mode": "illustrative",
                "reason": "",
            }
        }

        errors = reference_integrity.validate_block_metadata([block], metadata)

        self.assertTrue(any("stale sha256" in error for error in errors))
        self.assertTrue(any("non-empty reason" in error for error in errors))

    def test_metadata_requires_a_contract_for_non_aura_fences(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manual = Path(directory)
            (manual / "feature.md").write_text(
                textwrap.dedent(
                    """\
                    # Feature

                    ```bash
                    aura check example.au
                    ```

                    ```aura
                    print("checked")
                    ```
                    """
                ),
                encoding="utf-8",
            )

            inventory = reference_integrity.collect_manual(manual)
            aura = inventory.aura_blocks[0]
            metadata = {
                aura.identifier: {
                    "sha256": aura.sha256,
                    "mode": "check",
                    "stdout": "ok\n",
                    "stderr": "",
                }
            }

            errors = reference_integrity.validate_block_metadata(
                inventory.blocks, metadata
            )

            self.assertEqual(
                [block.identifier for block in inventory.blocks],
                [
                    "docs/manual/feature.md#bash-1",
                    "docs/manual/feature.md#aura-1",
                ],
            )
            self.assertTrue(
                any(
                    "missing metadata for docs/manual/feature.md#bash-1" in error
                    for error in errors
                )
            )

    def test_normative_inventory_requires_all_nonempty_sections_and_diagnostic_contract(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manual = Path(directory)
            headings = []
            for heading in REQUIRED_SECTIONS:
                if heading == "Backend Support":
                    continue
                body = "Defined here."
                if heading == "Diagnostics":
                    body = "Diagnostics are described in prose only."
                headings.append(f"## {heading}\n\n{body}\n")
            (manual / "feature.md").write_text(
                "# Feature\n\n" + "\n".join(headings), encoding="utf-8"
            )

            missing = reference_integrity.audit_normative_sections(
                manual,
                {"docs/manual/feature.md": {"kind": "feature"}},
                REQUIRED_SECTIONS,
            )

            self.assertIn("Backend Support", missing["docs/manual/feature.md"])
            self.assertIn(
                "Diagnostics (must name AU#### codes or explicitly state none)",
                missing["docs/manual/feature.md"],
            )

    def test_every_feature_page_requires_a_verified_executable_example(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manual = Path(directory)
            sections = "\n".join(
                f"## {heading}\n\n"
                + (
                    "No feature-specific diagnostics."
                    if heading == "Diagnostics"
                    else "Defined here."
                )
                for heading in REQUIRED_SECTIONS
            )
            (manual / "feature.md").write_text(
                "# Feature\n\n"
                "```aura\nprint(\"fragment\")\n```\n\n"
                + sections,
                encoding="utf-8",
            )
            inventory = reference_integrity.collect_manual(manual)
            block = inventory.aura_blocks[0]
            metadata = {
                block.identifier: {
                    "sha256": block.sha256,
                    "mode": "illustrative",
                    "reason": "Fragment intentionally has no standalone entrypoint.",
                }
            }
            roles = {"docs/manual/feature.md": {"kind": "feature"}}

            missing = reference_integrity.audit_feature_executable_examples(
                inventory.blocks, metadata, roles
            )

            self.assertEqual(missing, ["docs/manual/feature.md"])

    def test_runner_pins_check_run_and_rejection_outcomes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "fake-aura"
            binary.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env python3
                    import pathlib
                    import sys

                    command = sys.argv[1]
                    source = pathlib.Path(sys.argv[2]).read_text()
                    if "reject_me" in source:
                        print("error: AU1234 rejected example", file=sys.stderr)
                        raise SystemExit(1)
                    if command == "check":
                        print("ok")
                    else:
                        print(source.strip())
                    """
                ),
                encoding="utf-8",
            )
            binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
            blocks = [
                reference_integrity.ReferenceBlock(
                    path="docs/manual/feature.md",
                    ordinal=1,
                    line=1,
                    language="aura",
                    source="check_me\n",
                ),
                reference_integrity.ReferenceBlock(
                    path="docs/manual/feature.md",
                    ordinal=2,
                    line=4,
                    language="aura",
                    source="run_me\n",
                ),
                reference_integrity.ReferenceBlock(
                    path="docs/manual/feature.md",
                    ordinal=3,
                    line=7,
                    language="aura",
                    source="reject_me\n",
                ),
            ]
            metadata = {
                blocks[0].identifier: {
                    "sha256": blocks[0].sha256,
                    "mode": "check",
                    "stdout": "ok\n",
                    "stderr": "",
                },
                blocks[1].identifier: {
                    "sha256": blocks[1].sha256,
                    "mode": "run",
                    "stdout": "run_me\n",
                    "stderr": "",
                },
                blocks[2].identifier: {
                    "sha256": blocks[2].sha256,
                    "mode": "check-fail",
                    "exit_code": 1,
                    "stderr_contains": "AU1234",
                },
            }

            errors = reference_integrity.execute_examples(
                blocks, metadata, binary, root
            )

            self.assertEqual(errors, [])

    def test_runner_executes_only_allowlisted_cli_commands(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "examples").mkdir()
            (root / "examples/example.au").write_text(
                "print(1)\n", encoding="utf-8"
            )
            binary = root / "fake-aura"
            binary.write_text(
                "#!/bin/sh\n"
                "if [ \"$1\" = check ] && [ -f \"$2\" ]; "
                "then printf 'ok\\n'; exit 0; fi\n"
                "exit 9\n",
                encoding="utf-8",
            )
            binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
            block = reference_integrity.ReferenceBlock(
                path="docs/manual/cli.md",
                ordinal=1,
                line=1,
                language="bash",
                source="aura check examples/example.au\n",
                identifier_kind="bash",
            )
            metadata = {
                block.identifier: {
                    "sha256": block.sha256,
                    "mode": "command",
                    "stdout": "ok\n",
                    "stderr": "",
                }
            }

            errors = reference_integrity.execute_examples(
                [block], metadata, binary, root
            )

            self.assertEqual(errors, [])

    def test_runner_builds_a_safe_local_package_example(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "fake-aura"
            binary.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env python3
                    import pathlib
                    import sys

                    entry = pathlib.Path(sys.argv[2])
                    package = entry.parent.parent
                    assert sys.argv[1] == "check"
                    assert (package / "Aura.toml").is_file()
                    assert (package / "src/helpers/text.au").is_file()
                    print("ok")
                    """
                ),
                encoding="utf-8",
            )
            binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
            block = reference_integrity.ReferenceBlock(
                path="docs/manual/packages.md",
                ordinal=1,
                line=1,
                language="aura",
                source="import helpers.text\n",
            )
            metadata = {
                block.identifier: {
                    "sha256": block.sha256,
                    "mode": "package-check",
                    "entry": "src/main.au",
                    "files": {
                        "Aura.toml": "[package]\nname='docs'\n",
                        "src/helpers/text.au": "public def helper():\n    pass\n",
                    },
                    "stdout": "ok\n",
                    "stderr": "",
                }
            }

            errors = reference_integrity.execute_examples(
                [block], metadata, binary, root
            )

            self.assertEqual(errors, [])


if __name__ == "__main__":
    unittest.main()
