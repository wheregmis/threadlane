# Threadlane website

A static landing page with the real `threadlane-ui-kit-preview` WASM app at
`demo/`. No Node dependencies or separate UI implementation.

## Build and preview

```sh
rustup toolchain install nightly-2026-06-18 --profile minimal --component rust-src --target wasm32-unknown-unknown
cargo +1.95.0 install trunk --locked --version 0.21.14
python3 -m unittest discover -s website -p 'test_*.py'
python3 website/build.py --public-url / --debug
python3 -m http.server 8000 --directory website/dist
```

Open `http://localhost:8000`. The site reloads once to install its isolation
service worker. This deliberately tests serving without COOP/COEP headers,
as GitHub Pages does. Use a regular browser window that permits service workers.
The demo uses shared WASM memory; supported browsers need cross-origin isolation.
Production builds omit `--debug`.

The public build **always** embeds `session.sample.json`, even if a private
`session.local.json` exists. It never modifies or publishes the local snapshot.

## GitHub Pages

In repository **Settings → Pages → Build and deployment**, select **GitHub Actions**.
The `Website` workflow builds on pull requests and publishes pushes to `main`.
It can also be run from the Actions tab. The published site is
`https://wheregmis.github.io/threadlane/`; `demo/` opens the preview full screen.
The Pages action supplies the base path for project sites and custom domains.

The landing page must be isolated as well as its embedded demo. The vendored
[`coi-serviceworker`](https://github.com/gzuidhof/coi-serviceworker) v0.1.7
(revision `7b1d2a092d0d2dd2b7270b6f12f13605de26f214`) adds the required headers
to same-origin responses without an external proxy. Its MIT license is shipped
with the site. It does not cache responses. Fonts, logo, and screenshot reuse
repository assets; the font license is also shipped.
