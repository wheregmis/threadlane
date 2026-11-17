# Local vt100 patch

Source: crates.io `vt100` 0.16.2, from <https://github.com/doy/vt100-rust>.
The original MIT license is retained. Production dependencies are unchanged;
upstream test fixtures and development dependencies are omitted.

`Grid::set_size` now moves normal-screen rows above an out-of-bounds cursor
into the existing bounded scrollback before shrinking, and restores available
scrollback rows when growing. Cursor and saved-cursor positions move with the
rows, and a reader's scrollback offset stays attached to the retained output.
`Row::resize` keeps soft-wrap metadata when only the row count changes.

This fixes output loss when opening terminal Find or resizing its panel. The
public resize API only truncates rows and does not expose mutable rows or
scrollback. Replaying synthetic escape sequences through `Parser::process`
would corrupt a partially received escape sequence, so the fix belongs inside
the existing grid. Alternate-screen and column-reflow behavior stays upstream.

Regression checks live in `threadlane-ui-terminal`: `parser_resize_preserves_ansi_and_an_incomplete_escape_sequence`
and `parser_worker_retains_output_when_find_resizes_the_grid`.

Remove the patch when an upstream release preserves normal-screen rows during
vertical resize; verify both regression checks before switching back.
