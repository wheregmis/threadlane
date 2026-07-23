# AI-Generated Session Titles

**Date:** 2026-07-26
**Status:** Approved design

## Goal

Replace sidebar session labels that currently show a truncated first prompt with concise, AI-generated titles. Generate the title after the first user message, without delaying or changing the normal agent turn, and persist it across application restarts.

## Scope

This enhancement generates one title per session. It does not include manual renaming or continuous title revision.

## Existing Behavior

`crates/threadlane-gui/src/panels/sessions/state.rs` already prefers `SessionTree.name` when populated, then falls back to the first user message truncated to 42 characters. `SessionTree` has a `name` field, but the current JSONL persistence writes only session nodes and does not restore the name.

## Architecture

Add a lightweight title-generation path at the GUI/agent boundary. It will use the configured provider to make a separate, minimal model request with tools disabled. The request will include the first user message and instruct the model to return only a concise session title.

The title request runs independently of the normal agent turn. The conversation must remain responsive if title generation is slow or unavailable.

## Title Request Contract

The title prompt should require:

- title only, with no explanation;
- concise wording suitable for a sidebar;
- a maximum of 42 Unicode characters;
- no Markdown formatting.

The implementation should validate and normalize the model output by trimming whitespace, removing surrounding quotes and common prefixes such as `Title:`, collapsing repeated whitespace, and enforcing the 42-character limit. Empty or unusable output is treated as a generation failure.

## Persistence

Extend the existing session JSONL format with a metadata record, for example:

```json
{"type":"session_metadata","name":"Improve session titles"}
```

`SessionTree::load_from_file` must recognize metadata records while continuing to load existing node-only files. `save_to_file` must write the metadata record when a name exists. Updating a title must use the existing session file safely and must not corrupt the session if the process exits during the update.

Legacy sessions without metadata remain valid and continue to use the existing prompt-derived fallback.

## Data Flow

1. The user submits the first message.
2. The session is created and the sidebar displays the current prompt-based fallback immediately.
3. The title-generation request starts in the background.
4. On success, the title is normalized and assigned to `SessionTree.name`.
5. The title is persisted to the session file.
6. The GUI refreshes the affected sidebar row.

Generation must happen only when the session has no title and must be guarded against duplicate concurrent requests for the same session.

## Error Handling

- Provider, network, timeout, or parsing failures leave the prompt fallback in place.
- Title-generation failures must not fail, cancel, or alter the normal conversation turn.
- Failures should be logged at an appropriate warning/debug level without presenting a conversation error.
- An existing explicit/session title must never be overwritten by automatic generation.

## Testing

Add focused tests for:

- metadata serialization and deserialization;
- loading legacy node-only JSONL files;
- title normalization, prefix/quote removal, whitespace cleanup, and Unicode length limits;
- empty and invalid model output fallback;
- no regeneration when a title already exists;
- duplicate-generation protection;
- sidebar discovery preferring persisted names.

## Non-Goals

- Manual session renaming.
- Retitling after subsequent messages.
- Changes to project naming.
- Changes to the main agent prompt or tool execution behavior.
