# Browser syntax-highlighting probe

Run from the repository root:

```sh
python3 crates/threadlane-ui-kit/examples/preview/experiments/wasm-highlighting/run.py
```

Requires the preview's `nightly-2026-06-18` toolchain with the
`wasm32-unknown-unknown` target, Node.js, and a Clang with a WASM backend
(`CC=/opt/homebrew/opt/llvm/bin/clang` on this development Mac).

The probe executes a linked WASM module and checks Rust, JSON, a Bash command,
and a Bash heredoc for syntax errors. Its dependency versions match the current
desktop lockfile. It does not change app features or claim complete highlighting
parity.

The locked Tree-sitter runtime includes browser-target C support. The older
grammar build scripts need its `tree-sitter-language` headers explicitly passed
to Clang. This probe copies those headers into a temporary directory and adds
`strcmp`, `isdigit`, and local linkage for `__assert_fail`; source dependencies
and Cargo's cache remain unchanged. The copied headers retain their upstream
content; [Tree-sitter's license](https://github.com/tree-sitter/tree-sitter/blob/master/LICENSE)
applies.

Integration still needs these changes in the shared GPUI Kit dependency:

- Make grammar dependencies available on WASM; they are currently declared under
  `cfg(not(target_family = "wasm"))`.
- Let grammar build scripts consume the header metadata, including the missing
  C helpers and assertion-linkage repair demonstrated here.
- Use the already available `instant` clock in the highlighter instead of
  `std::time::Instant`, which cannot run on this browser target.
- Enable and test the actual desktop grammar set, injections, and themed capture
  ranges in the web preview, including Markdown code fences and tool previews.

Reuse GPUI Kit's parser, queries, and theme resolver for integration. Replacing
them with a separate browser lexer would leave the components with different
behavior across platforms.
