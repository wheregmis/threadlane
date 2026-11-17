"""Compile and execute the browser-target parser probe, without editing dependencies."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parent
cargo = ["cargo", "+nightly-2026-06-18"]
manifest = ["--manifest-path", str(root / "Cargo.toml")]
metadata = json.loads(subprocess.check_output(
    cargo + ["metadata", "--locked", "--format-version", "1"] + manifest,
    text=True,
))
language = next(p for p in metadata["packages"] if p["name"] == "tree-sitter-language")
upstream_headers = Path(language["manifest_path"]).parent / "wasm" / "include"

# The old grammar build scripts do not consume the header metadata exported by
# tree-sitter-language. Keep this compatibility experiment out of the app build.
with tempfile.TemporaryDirectory(prefix="threadlane-wasm-headers-") as directory:
    headers = Path(directory) / "include"
    shutil.copytree(upstream_headers, headers)
    additions = {
        "string.h": """static inline int strcmp(const char *left, const char *right) {
  const unsigned char *a = (const unsigned char *)left;
  const unsigned char *b = (const unsigned char *)right;
  while (*a && *a == *b) { ++a; ++b; }
  return (int)*a - (int)*b;
}
""",
        "ctype.h": "static inline int isdigit(int c) { return c >= '0' && c <= '9'; }\n",
    }
    for name, source in additions.items():
        path = headers / name
        text = path.read_text()
        boundary = text.rindex("#endif")
        path.write_text(text[:boundary] + source + text[boundary:])
    assertion = headers / "assert.h"
    assertion.write_text(assertion.read_text().replace(
        "__attribute__((noreturn)) void __assert_fail",
        "static __attribute__((noreturn)) void __assert_fail",
    ))
    env = os.environ.copy()
    env["CARGO_INCREMENTAL"] = "0"
    env["CC_SHELL_ESCAPED_FLAGS"] = "1"
    env["CFLAGS_wasm32_unknown_unknown"] = (
        env.get("CFLAGS_wasm32_unknown_unknown", "") + " "
        + shlex.join(["-I", str(headers)])
    )
    subprocess.run(cargo + ["build", "--locked", "--target", "wasm32-unknown-unknown"]
                   + manifest, env=env, check=True)

wasm = Path(metadata["target_directory"]) / "wasm32-unknown-unknown" / "debug" / "threadlane_highlight_smoke.wasm"
subprocess.run(["node", str(root / "run.cjs"), str(wasm)], check=True)
