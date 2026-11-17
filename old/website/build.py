#!/usr/bin/env python3
"""Build the landing page and public UI-Kit demo with no JS build dependencies."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "website"
PREVIEW = ROOT / "crates/threadlane-ui-kit/examples/preview"


def build(public_url, release=True):
    if not public_url.startswith("/") or not public_url.endswith("/"):
        raise ValueError("public URL must be an absolute path ending in /, e.g. /threadlane/")
    output = SITE / "dist"
    output.mkdir(exist_ok=True)
    for name in ("index.html", "style.css", "site.js"):
        shutil.copy2(SITE / name, output / name)
    shutil.copy2(SITE / "vendor/coi-serviceworker.js", output / "coi-serviceworker.js")
    shutil.copy2(SITE / "vendor/LICENSE", output / "coi-serviceworker-LICENSE.txt")
    assets = output / "assets"
    assets.mkdir(exist_ok=True)
    for name in ("threadlane-logo.svg", "threadlane-workspace.png"):
        shutil.copy2(ROOT / "assets/images" / name, assets / name)
    for name in ("IBMPlexSans-Regular.ttf", "IBMPlexSans-SemiBold.ttf", "OFL.txt"):
        shutil.copy2(ROOT / "crates/threadlane-ui-theme/assets/fonts" / name, assets / name)
    # Never use a private imported conversation, even on a developer's machine.
    env = {**os.environ, "THREADLANE_PREVIEW_PUBLIC_SAMPLE": "1"}
    command = ["trunk", "build", "--locked", "--dist", str(output / "demo"),
               "--public-url", public_url + "demo/"]
    if release:
        command.append("--release")
    subprocess.run(command, cwd=PREVIEW, env=env, check=True)
    demo = output / "demo/index.html"
    html = demo.read_text()
    # Also bootstrap isolation when visitors open the full-screen demo directly.
    html = html.replace("<head>", '<head>\n<script src="../coi-serviceworker.js"></script>', 1)
    demo.write_text(html)
    (output / ".nojekyll").touch()
    print(f"Built {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--public-url", default="/threadlane/")
    parser.add_argument("--debug", action="store_true", help="Use the faster development WASM build")
    args = parser.parse_args()
    build(args.public_url, release=not args.debug)
