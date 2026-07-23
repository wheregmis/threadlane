# `threadlane`

The `threadlane` crate contains the Makepad-based desktop user interface for Threadlane.

## Overview

- **Framework**: Built on top of [Makepad](https://github.com/makepad/makepad), utilizing custom shaders, widgets, and reactive event loops for GPU-accelerated desktop UI.
- **Panels & Components**:
  - **Chat Panel**: Real-time streaming model text, markdown rendering, tool execution previews, reasoning accordion states, and image attachment chips.
  - **Command Palette & Input**: Slash-command autocompletion, skill discovery previews, keyboard navigation, and prompt templates.
  - **Sessions & Project Registry**: Persistent multi-project workspace switching (`~/.threadlane/gui/projects.json`), automatic session title generation, and draft isolation.

## Running

```bash
cargo run -p threadlane
```
