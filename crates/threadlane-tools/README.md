# `threadlane-tools`

`threadlane-tools` contains built-in file system tools, pattern searching primitives, and sandboxed process execution for Threadlane.

## Included Primitives

- **File Operations**: `view_file`, `replace_file_content`, `multi_replace_file_content`, and `write_to_file`.
- **Directory & Search**: `list_dir` for directory enumeration and `grep_search` using `ripgrep` for fast pattern matching.
- **Process Execution**: `run_command` with strict working directory containment validation and timeout boundaries.
