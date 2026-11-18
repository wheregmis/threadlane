"""Check the public build without compiling WASM or touching local snapshots."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import build


class PublicBuildTest(unittest.TestCase):
    def test_public_sample_paths_and_isolation(self):
        with tempfile.TemporaryDirectory() as directory:
            site = Path(directory)
            for name in ("index.html", "style.css", "site.js", "vendor/coi-serviceworker.js", "vendor/LICENSE"):
                path = site / name
                path.parent.mkdir(exist_ok=True)
                path.write_text("fixture")

            def trunk(command, cwd, env, check):
                self.assertEqual(env["THREADLANE_PREVIEW_PUBLIC_SAMPLE"], "1")
                self.assertEqual(cwd, build.PREVIEW)
                self.assertIn("/threadlane/demo/", command)
                self.assertIn("--locked", command)
                self.assertIn("--release", command)
                self.assertTrue(check)
                demo = site / "dist/demo"
                demo.mkdir()
                (demo / "index.html").write_text("<html><head></head><body></body></html>")
                return subprocess.CompletedProcess(command, 0)

            with patch.object(build, "SITE", site), patch.object(build.subprocess, "run", trunk), patch.dict(os.environ, {"THREADLANE_PREVIEW_PUBLIC_SAMPLE": ""}):
                build.build("/threadlane/")
            html = (site / "dist/demo/index.html").read_text()
            self.assertIn('<script src="../coi-serviceworker.js"></script>', html)
            self.assertTrue((site / "dist/.nojekyll").is_file())
            self.assertTrue((site / "dist/assets/threadlane-logo.svg").is_file())
            self.assertTrue((site / "dist/coi-serviceworker-LICENSE.txt").is_file())

    def test_invalid_public_url(self):
        with self.assertRaises(ValueError):
            build.build("threadlane")

    def test_rust_build_ignores_private_snapshot(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable = root / "preview-build"
            subprocess.run(["rustc", "+nightly-2026-06-18", str(build.PREVIEW / "build.rs"),
                            "-o", str(executable)], check=True)
            (root / "session.local.json").write_text("private fixture")
            (root / "session.sample.json").write_text("public fixture")
            env = {**os.environ, "OUT_DIR": str(root)}
            env.pop("THREADLANE_PREVIEW_PUBLIC_SAMPLE", None)
            subprocess.run([str(executable)], cwd=root, env=env, check=True, capture_output=True)
            self.assertEqual((root / "session.json").read_text(), "private fixture")
            env["THREADLANE_PREVIEW_PUBLIC_SAMPLE"] = "1"
            subprocess.run([str(executable)], cwd=root, env=env, check=True, capture_output=True)
            self.assertEqual((root / "session.json").read_text(), "public fixture")


if __name__ == "__main__":
    unittest.main()
