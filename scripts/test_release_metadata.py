#!/usr/bin/env python3
"""Release-metadata regression tests for the Aura 0.3 technical preview."""

from __future__ import annotations

import json
import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
VERSION = "0.3.3"
EXTENSION_VERSION = "0.3.4"


class ReleaseMetadataTests(unittest.TestCase):
    def test_product_manifests_and_locks_record_current_versions(self) -> None:
        cargo_manifest = (ROOT / "Cargo.toml").read_text()
        workspace_version = re.search(
            r"\[workspace\.package\].*?^version\s*=\s*\"([^\"]+)\"",
            cargo_manifest,
            re.MULTILINE | re.DOTALL,
        )
        self.assertIsNotNone(workspace_version)
        self.assertEqual(workspace_version.group(1), VERSION)

        cargo_lock = (ROOT / "Cargo.lock").read_text()
        workspace_packages = {}
        for package in cargo_lock.split("[[package]]")[1:]:
            name_match = re.search(r'^name = "([^"]+)"$', package, re.MULTILINE)
            version_match = re.search(r'^version = "([^"]+)"$', package, re.MULTILINE)
            if (
                name_match is not None
                and version_match is not None
                and name_match.group(1) in {"aura", "aura-compiler"}
            ):
                workspace_packages[name_match.group(1)] = version_match.group(1)
        self.assertEqual(
            workspace_packages,
            {"aura": VERSION, "aura-compiler": VERSION},
        )

        fuzz_lock = (ROOT / "fuzz/Cargo.lock").read_text()
        fuzz_compiler = re.search(
            r'\[\[package\]\]\nname = "aura-compiler"\nversion = "([^"]+)"',
            fuzz_lock,
        )
        self.assertIsNotNone(fuzz_compiler)
        self.assertEqual(fuzz_compiler.group(1), VERSION)

        manifests = {
            "root": ROOT / "package.json",
            "lsp": ROOT / "tools/aura-language-server/package.json",
        }
        for label, path in manifests.items():
            with self.subTest(label=label):
                self.assertEqual(json.loads(path.read_text())["version"], VERSION)
        self.assertEqual(
            json.loads((ROOT / "tools/vscode-aura/package.json").read_text())["version"],
            EXTENSION_VERSION,
        )

        root_lock = json.loads((ROOT / "package-lock.json").read_text())
        self.assertEqual(root_lock["version"], VERSION)
        self.assertEqual(root_lock["packages"][""]["version"], VERSION)
        self.assertEqual(
            root_lock["packages"]["tools/aura-language-server"]["version"],
            VERSION,
        )
        self.assertEqual(
            root_lock["packages"]["tools/vscode-aura"]["version"],
            EXTENSION_VERSION,
        )

        # This npm workspace intentionally uses one root lock. The LSP and
        # extension entries above are their lock records; package-local lock
        # files would split dependency resolution and are not maintained.

    def test_changelog_separates_next_work_from_published_0_3_3(self) -> None:
        changelog = (ROOT / "CHANGELOG.md").read_text()
        self.assertIn("## 0.3.4 — Unreleased (technical preview)", changelog)
        self.assertIn("## 0.3.3 — 2026-09-06 (technical preview)", changelog)
        self.assertNotIn("## 0.3.3 — Unreleased", changelog)
        pending = changelog.split("## VS Code extension 0.3.4", 1)[0]
        shipped = changelog.split("## 0.3.3 — 2026-09-06", 1)[1].split("## VS Code extension 0.3.3", 1)[0]
        self.assertNotIn("Published VS Code extension 0.3.4", pending)
        self.assertIn("`view name = place`", shipped)
        self.assertIn("`view mut name = place`", shipped)
        self.assertIn("Aura 0.3.3 is a technical preview", changelog)
        self.assertIn("## 0.3.2 — 2026-08-04 (technical preview)", changelog)
        self.assertIn("## 0.3.1 — 2026-08-04 (technical preview)", changelog)
        self.assertIn("## 0.3.0 — 2026-08-03 (technical preview)", changelog)
        self.assertIn("## 0.2.0 — 2026-07-31 (technical preview)", changelog)
        for heading in (
            "Ownership surface",
            "Language",
            "Runtime and structured concurrency",
            "Callables and closures",
            "Foreign function interface",
            "Numeric arrays",
            "Tooling and diagnostics",
            "Current limits",
        ):
            with self.subTest(heading=heading):
                self.assertIn(f"### {heading}", changelog)
        self.assertNotIn("one compatibility release", changelog)
        self.assertNotIn("capability_migrate.py", changelog)
        self.assertIn("ADR-0038", changelog)

    def test_manual_declares_release_and_dynamic_implementation_stamp(self) -> None:
        manual = (ROOT / "docs/manual/index.md").read_text()
        language_spec = (ROOT / "docs/manual/language-specification.md").read_text()
        current_limits = (ROOT / "docs/manual/current-limits.md").read_text()
        config = (ROOT / "docs/.vitepress/config.mts").read_text()
        metadata = (ROOT / "docs/.vitepress/release-metadata.mjs").read_text()
        component = (ROOT / "docs/.vitepress/theme/ReleaseStamp.vue").read_text()
        theme = (ROOT / "docs/.vitepress/theme/index.ts").read_text()

        self.assertIn("Aura 0.3.3", manual)
        self.assertIn("technical preview", manual.lower())
        self.assertIn("implementation baseline commit", manual.lower())
        self.assertIn("AURA_DOCS_COMMIT", metadata)
        self.assertIn("GITHUB_SHA", metadata)
        self.assertIn("local-uncommitted-checkout", manual)
        self.assertIn("git", metadata)
        self.assertIn("release-metadata.mjs", config)
        self.assertIn("implementationCommit", config)
        self.assertIn("__AURA_IMPLEMENTATION_COMMIT__", component)
        self.assertIn("Implementation baseline commit", component)
        self.assertIn("ReleaseStamp", theme)

        self.assertNotIn(
            "passing for a copy type and shared borrowing for a non-copy",
            language_spec,
        )
        self.assertIn("Shared access for every type", language_spec)
        self.assertIn("Same-dtype scalar\n  broadcast is implemented", current_limits)
        self.assertIn("no array-shape broadcasting", current_limits)
        self.assertNotIn("There is no broadcasting", current_limits)

    def test_current_release_prose_no_longer_calls_itself_0_1(self) -> None:
        roots = (
            ROOT / "docs/manual",
            ROOT / "docs/learn",
            ROOT / "tutorials",
        )
        files = [ROOT / "SUPPORTED_PLATFORMS.md", ROOT / "crates/aura/README.md"]
        for directory in roots:
            files.extend(directory.glob("*.md"))
            files.extend(directory.glob("**/*.md"))

        stale: list[str] = []
        pattern = re.compile(r"\bAura 0\.1(?:\.x)?\b|\b0\.1\.x\b")
        for path in sorted(set(files)):
            for number, line in enumerate(path.read_text().splitlines(), start=1):
                if pattern.search(line):
                    stale.append(f"{path.relative_to(ROOT)}:{number}: {line.strip()}")
        self.assertEqual(stale, [], "stale current-release prose:\n" + "\n".join(stale))

    def test_current_release_prose_no_longer_calls_itself_0_2(self) -> None:
        roots = (
            ROOT / "docs/manual",
            ROOT / "docs/learn",
            ROOT / "tutorials",
        )
        files = [
            ROOT / "README.md",
            ROOT / "SUPPORTED_PLATFORMS.md",
            ROOT / "crates/aura/README.md",
            ROOT / "docs/downloads.md",
            ROOT / "docs/positioning.md",
            ROOT / "docs/release-process.md",
        ]
        for directory in roots:
            files.extend(directory.glob("*.md"))
            files.extend(directory.glob("**/*.md"))

        stale: list[str] = []
        pattern = re.compile(
            r"\bAura 0\.2(?:\.0|\.x)?\b|\bv0\.2\.0-preview\b|\b0\.2\.x\b"
        )
        for path in sorted(set(files)):
            for number, line in enumerate(path.read_text().splitlines(), start=1):
                if pattern.search(line):
                    stale.append(f"{path.relative_to(ROOT)}:{number}: {line.strip()}")
        self.assertEqual(stale, [], "stale current-release prose:\n" + "\n".join(stale))

    def test_current_release_prose_no_longer_calls_itself_0_3_0(self) -> None:
        roots = (
            ROOT / "docs/manual",
            ROOT / "docs/learn",
            ROOT / "docs/install",
            ROOT / "tutorials",
        )
        files = [
            ROOT / "README.md",
            ROOT / "SUPPORTED_PLATFORMS.md",
            ROOT / "crates/aura/README.md",
            ROOT / "docs/downloads.md",
            ROOT / "docs/positioning.md",
            ROOT / "docs/release-process.md",
        ]
        for directory in roots:
            files.extend(directory.glob("*.md"))
            files.extend(directory.glob("**/*.md"))

        stale: list[str] = []
        pattern = re.compile(r"\bAura 0\.3\.0\b|\bv0\.3\.0-preview\b")
        for path in sorted(set(files)):
            for number, line in enumerate(path.read_text().splitlines(), start=1):
                if pattern.search(line):
                    stale.append(f"{path.relative_to(ROOT)}:{number}: {line.strip()}")
        self.assertEqual(stale, [], "stale current-release prose:\n" + "\n".join(stale))

    def test_prepublish_truth_polish_is_retained(self) -> None:
        cli_tests = (ROOT / "crates/aura/tests/cli.rs").read_text()
        cache_test = cli_tests.split(
            "fn native_run_cache_serializes_concurrent_cold_runs_into_one_build_and_verified_hits()",
            1,
        )[1].split("\n#[", 1)[0]
        self.assertIn("Duration::from_secs(120)", cache_test)
        self.assertNotIn("Duration::from_secs(60)", cache_test)

        parity = (ROOT / "crates/aura/tests/backend_parity.rs").read_text()
        self.assertNotIn('root.join("target/debug/libaura_compiler.a")', parity)
        self.assertIn('"--message-format=json"', parity)
        self.assertIn('"compiler-artifact"', parity)
        self.assertIn('"native-static-libs:"', parity)

        proposal_name = "auro" + "ra_language_proposal.md"
        proposal = (ROOT / "docs" / proposal_name).read_text()
        self.assertIn("canonical 0.1/0.2 contract", proposal)
        self.assertIn("maintained 0.1/0.2 sources win", proposal)
        self.assertNotIn("canonical 0.1 contract", proposal)

        build_script = (ROOT / "crates/aura/build.rs").read_text()
        cli = (ROOT / "crates/aura/src/main.rs").read_text()
        smoke = (ROOT / "scripts/smoke-cli-archive.sh").read_text()
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertIn("AURA_BUILD_COMMIT", build_script)
        self.assertIn("AURA_BUILD_CHANNEL", build_script)
        self.assertIn("--short=12", build_script)
        self.assertIn('"aura {}-{} ({})\\n"', cli)
        self.assertIn(
            'expected_version="aura $release_version ($expected_commit)"', smoke
        )
        self.assertIn("AURA_BUILD_CHANNEL: preview", workflow)

    def test_scheduled_safety_jobs_pin_the_instrumented_toolchain_and_leaks(self) -> None:
        workflow = (ROOT / ".github/workflows/safety.yml").read_text()
        tsan = (ROOT / "scripts/sanitizer-scheduler-tsan.sh").read_text()
        asan = (ROOT / "scripts/sanitizer-native-runtime.sh").read_text()
        native_runtime = (
            ROOT / "crates/aura-compiler/src/native_runtime.rs"
        ).read_text()
        native_runtime_exports = (ROOT / "crates/aura-compiler/src/lib.rs").read_text()
        ffi_tests = (
            ROOT / "crates/aura-compiler/tests/native_runtime_ffi.rs"
        ).read_text()

        self.assertIn("RUSTUP_TOOLCHAIN: nightly-2026-07-01", workflow)
        self.assertEqual(
            workflow.count("cargo +nightly-2026-07-01 fuzz run"),
            2,
        )
        self.assertEqual(
            workflow.count("--target x86_64-unknown-linux-gnu"),
            2,
        )
        self.assertIn("CARGO_TARGET_", tsan)
        self.assertNotIn("export RUSTFLAGS=", tsan)
        self.assertNotIn("--test cli", tsan)
        self.assertIn("CARGO_TARGET_", asan)
        self.assertNotIn("export RUSTFLAGS=", asan)
        self.assertIn('ASAN_OPTIONS="$asan_options" cargo', asan)
        self.assertNotIn('"$aura_bin" build', asan)
        self.assertIn("DIRECT_VALUE_LIVE_COUNT", native_runtime)
        self.assertIn("DIRECT_VALUE_LIVE_COUNT", native_runtime_exports)
        self.assertNotIn("aura_direct_coverage_live_value_count", native_runtime_exports)
        self.assertEqual(ffi_tests.count("direct_runtime_ffi_test_guard()"), 8)

    def test_scheduler_stress_filters_resolve_and_fit_the_hosted_budget(self) -> None:
        workflow = (ROOT / ".github/workflows/safety.yml").read_text()
        stress = (ROOT / "scripts/stress-scheduler.sh").read_text()
        cli_tests = (ROOT / "crates/aura/tests/cli.rs").read_text()

        configured = re.findall(r'^  "([a-z0-9_]+)"$', stress, re.MULTILINE)
        self.assertEqual(
            configured,
            [
                "queue_consumers_share_work_fairly_on_one_worker",
                "cancelled_sleeping_children_resume_and_can_observe_cancellation",
                "scheduler_mixed_wakeups_complete_in_mir_and_direct_backends",
            ],
        )
        for test_name in configured:
            with self.subTest(test_name=test_name):
                self.assertIn(f"fn {test_name}()", cli_tests)

        self.assertIn("timeout-minutes: 45", workflow)
        self.assertIn("AURA_STRESS_RUNS=10 scripts/stress-scheduler.sh", workflow)
        self.assertNotIn("AURA_STRESS_RUNS=50 scripts/stress-scheduler.sh", workflow)


if __name__ == "__main__":
    unittest.main()
