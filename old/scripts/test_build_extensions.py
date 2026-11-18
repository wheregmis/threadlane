#!/usr/bin/env python3
"""Regression tests for the isolated WASI extension build/deploy script."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "build_extensions.sh"
PACKAGES = [
    "broker-smoke-ext",
    "debug-ext",
    "goal-ext",
    "lsp-ext",
    "web_ext",
]
EXTENSIONS = {
    "broker_smoke_ext": "broker-smoke-ext",
    "debug_ext": "debug-ext",
    "goal_ext": "goal-ext",
    "lsp_ext": "lsp-ext",
    "web_ext": "web_ext",
}


class BuildExtensionsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "fixture"
        (self.root / "scripts").mkdir(parents=True)
        shutil.copy2(SCRIPT, self.root / "scripts" / SCRIPT.name)
        (self.root / "Cargo.toml").write_text("[workspace]\nmembers = []\n")

        self.call_log = Path(self.temp.name) / "cargo-calls.jsonl"
        cargo_dir = Path(self.temp.name) / "bin"
        cargo_dir.mkdir()
        self.fake_cargo = cargo_dir / "cargo"
        self.fake_cargo.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "with open(os.environ['CARGO_CALL_LOG'], 'a') as calls:\n"
            "    calls.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            "sys.exit(int(os.environ.get('FAKE_CARGO_EXIT', '0')))\n"
        )
        self.fake_cargo.chmod(0o755)
        self.env = os.environ.copy()
        self.env["PATH"] = f"{cargo_dir}{os.pathsep}{self.env.get('PATH', '')}"
        self.env["CARGO_CALL_LOG"] = str(self.call_log)

        extensions = self.root / "extensions"
        for directory, package in EXTENSIONS.items():
            extension = extensions / directory
            extension.mkdir(parents=True)
            (extension / "Cargo.toml").write_text(
                f'[package]\nname = "{package}"\nversion = "0.1.0"\n'
                'edition = "2021"\n'
            )
            if directory == "broker_smoke_ext":
                (extension / "agents").mkdir()
                (extension / "agents" / "bundled-agent.md").write_text(
                    "bundled agent"
                )
                (extension / "prompts").mkdir()
                (extension / "prompts" / "bundled-prompt.md").write_text(
                    "bundled prompt"
                )

        self.release_dir = self.root / "target" / "wasm32-wasip1" / "release"
        self.release_dir.mkdir(parents=True)
        for directory in EXTENSIONS:
            (self.release_dir / f"{directory}.wasm").write_text(f"module {directory}")

    def run_script(self, **extra_env):
        env = self.env.copy()
        env.update(extra_env)
        return subprocess.run(
            ["bash", str(self.root / "scripts" / SCRIPT.name)],
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )

    def cargo_calls(self):
        if not self.call_log.exists():
            return []
        return [json.loads(line) for line in self.call_log.read_text().splitlines()]

    def test_one_build_invocation_deploys_without_overwriting_user_state(self):
        deploy = self.root / ".threadlane"
        extensions = deploy / "extensions"
        agents = deploy / "agents"
        prompts = deploy / "prompts"
        extensions.mkdir(parents=True)
        agents.mkdir()
        prompts.mkdir()
        (extensions / "user-installed.wasm").write_text("user module")
        (extensions / "debug_ext.wasm.disabled").write_text("disabled")
        (agents / "user-agent.md").write_text("user agent")
        (prompts / "user-prompt.md").write_text("user prompt")

        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.cargo_calls()
        self.assertEqual(len(calls), 1)
        args = calls[0]
        self.assertEqual(args[0], "build")
        self.assertIn("--target", args)
        self.assertEqual(args[args.index("--target") + 1], "wasm32-wasip1")
        self.assertIn("--release", args)
        selected = [
            args[index + 1]
            for index, value in enumerate(args[:-1])
            if value == "--package"
        ]
        self.assertEqual(selected, PACKAGES)

        for directory in EXTENSIONS:
            self.assertEqual(
                (extensions / f"{directory}.wasm").read_text(),
                f"module {directory}",
            )
        self.assertEqual(
            (extensions / "user-installed.wasm").read_text(), "user module"
        )
        self.assertEqual(
            (extensions / "debug_ext.wasm.disabled").read_text(), "disabled"
        )
        self.assertEqual((agents / "user-agent.md").read_text(), "user agent")
        self.assertEqual((prompts / "user-prompt.md").read_text(), "user prompt")
        self.assertEqual((agents / "bundled-agent.md").read_text(), "bundled agent")
        self.assertEqual((prompts / "bundled-prompt.md").read_text(), "bundled prompt")

    def test_build_failure_is_fatal(self):
        result = self.run_script(FAKE_CARGO_EXIT="17")

        self.assertEqual(result.returncode, 17)
        self.assertEqual(len(self.cargo_calls()), 1)
        self.assertNotIn("Successfully deployed", result.stdout)

    def test_missing_expected_binary_is_fatal(self):
        missing = "goal_ext.wasm"
        (self.release_dir / missing).unlink()

        result = self.run_script()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            f"Missing compiled WASI module: {self.release_dir / missing}",
            result.stderr,
        )
        self.assertEqual(len(self.cargo_calls()), 1)


if __name__ == "__main__":
    unittest.main()
