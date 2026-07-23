# Workspace Header Hierarchy Design

## Goal

Make the workspace header easier to scan by emphasizing the active project, compacting long paths without losing useful context, and making the capabilities summary visibly actionable.

## Scope

This is a focused visual and presentation change to the existing workspace header. The current capabilities click behavior remains unchanged: clicking the control appends the capabilities summary to the chat. No new panel, navigation behavior, or dependency is introduced.

## Header layout

The left side uses a compact two-line project identity:

- A folder icon establishes project context.
- The project directory name is derived from the active working directory and displayed in bold, high-contrast text.
- A compact display path appears below it in smaller muted text.

The identity block remains vertically compact so the header does not materially reduce transcript space.

The right side retains the existing status indicator and capabilities control. The capabilities control displays an icon, the existing count text (for example, `25 skills · 4 agents`), and a chevron. Stronger hover, pressed, border, and text states communicate that it is clickable.

## Path presentation

A pure helper produces a stable display path:

- Paths under the current user's home directory use `~`.
- Short paths remain intact.
- Long paths preserve the beginning and final two components, replacing omitted middle components with `…`.
- Root and unusual paths fall back safely to their normal display representation.

The project name is derived from the working directory's final component, with a safe fallback when no final component exists.

## Behavior and data flow

At startup, the existing working directory is passed to the project-name and compact-path helpers. Their values populate separate project-name and path labels. Capability discovery continues to update the existing button text, and clicking that button follows the current event path without modification.

## Error handling

All display derivation is non-failing. Missing home-directory information simply disables `~` substitution. Paths that cannot be represented through normal components use their lossy display form. No filesystem mutation or canonicalization is added.

## Testing

Unit tests cover:

- Project-name extraction.
- Home-relative path formatting.
- Preservation of short paths.
- Middle compaction for long paths.
- Root/fallback behavior.

The GUI crate's existing test suite and compilation checks verify integration with the Makepad view.
