# Learning Makepad — Living Reference

> Auto-updated by background agent as we discover new patterns.
> Last updated: 2026-06-10 (round 2 — layout, animator, PortalList, LiveId macros, #[deref], Cx/CxDraw/Cx2d)

---

## Table of Contents
1. [Project Structure](#1-project-structure)
2. [Building & Running](#2-building--running)
3. [Studio Remote Bridge Protocol](#3-studio-remote-bridge-protocol)
4. [DSL: script_mod! vs live_design!](#4-dsl-script_mod-vs-live_design)
5. [Widget System](#5-widget-system)
6. [Draw System & Turtles](#6-draw-system--turtles)
7. [FileTree Widget](#7-filetree-widget)
8. [Custom Widgets](#8-custom-widgets)
9. [App Architecture Pattern](#9-app-architecture-pattern)
10. [Event & Action System](#10-event--action-system)
11. [Scope & State Passing](#11-scope--state-passing)
12. [Shader / SDF Drawing](#12-shader--sdf-drawing)
13. [Common Pitfalls & Hard-Won Lessons](#13-common-pitfalls--hard-won-lessons)
14. [makepad-ide Build Notes](#14-makepad-ide-build-notes)
15. [Walk / Size / Layout / Align / Inset](#15-walk--size--layout--align--inset)
16. [Animator & Animation State Machines](#16-animator--animation-state-machines)
17. [PortalList & Template System](#17-portallist--template-system)
18. [LiveId Macros: id! vs ids! vs live_id!](#18-liveid-macros-id-vs-ids-vs-live_id)
19. [#[deref] Widget Composition](#19-deref-widget-composition)
20. [Cx vs CxDraw vs Cx2d](#20-cx-vs-cxdraw-vs-cx2d)

---

## 1. Project Structure

```
makepad/
├── platform/          # Core platform (event loop, Cx, Cx2d, turtles, GPU)
├── draw/              # Draw primitives: turtle layout, DrawQuad, DrawText, Area
├── widgets/           # All standard widgets (FileTree, Button, Label, etc.)
│   └── src/
│       ├── lib.rs     # Widget registry + widget list
│       ├── file_tree.rs
│       ├── button.rs
│       └── ...
├── code_editor/       # Syntax-highlighted editor (CodeEditor, CodeSession, CodeDocument)
├── studio/
│   ├── desktop/       # Full Studio IDE (DesktopFileTree, AppData, etc.)
│   └── makepad_ide/   # Our lightweight IDE example
│       └── src/main.rs
└── Cargo.toml         # Workspace root — must list all member crates
```

**Key rule**: When adding a new example crate, update both `Cargo.toml` workspace members AND `makepad.splash` so Studio exposes the runnable item.

---

## 2. Building & Running

### Shell (non-UI tasks only)
```bash
# Check compilation without running UI
cargo check -p makepad-ide

# Build release (for perf-sensitive work)
cargo build --release -p makepad-ide

# Run tests
cargo test --release -p makepad-widgets
```

### UI Programs — use Studio Remote, NOT cargo run
Never use `cargo run` for UI apps that have a runnable item in Studio.

```bash
# 1. Start Studio once (user does this manually)
#    Usually: cargo run --release -p makepad-studio

# 2. Start the bridge (keep one persistent process)
target/release/cargo-makepad studio --studio=127.0.0.1:8001

# 3. Send JSON commands on stdin
{"ListBuilds":[]}
{"ClearBuild":{"build_id":[N]}}          # stop + clear tabs
{"RunItem":{"mount":"makepad","name":"makepad-ide"}}
{"Screenshot":{"build_id":[N]}}
{"WidgetTreeDump":{"build_id":[N]}}
{"Click":{"build_id":[N],"x":100,"y":200}}
{"TypeText":{"build_id":[N],"text":"hello"}}
{"Return":{"build_id":[N],"auto_dump":false}}
```

### Runnable Item Names
- Derived from the crate `name` field in `Cargo.toml`, e.g., `makepad-example-todo`, `makepad-ide`
- Must be present in `makepad.splash` for Studio to show it

---

## 3. Studio Remote Bridge Protocol

### Key Requests
| Request | Purpose |
|---------|---------|
| `{"ListBuilds":[]}` | Show all running/stopped builds |
| `{"ClearBuild":{"build_id":[N]}}` | Stop + remove Studio tabs for build N |
| `{"StopBuild":{"build_id":[N]}}` | Stop but keep tabs |
| `{"RunItem":{"mount":"makepad","name":"<name>"}}` | Build + launch a runnable item |
| `{"Screenshot":{"build_id":[N]}}` | Take a screenshot (returns file path) |
| `{"WidgetTreeDump":{"build_id":[N]}}` | Dump full widget tree (for click coords) |
| `{"WidgetQuery":{"build_id":[N],"query":"id:foo"}}` | Query specific widget |
| `{"Click":{"build_id":[N],"x":X,"y":Y}}` | Simulate a click |
| `{"TypeText":{"build_id":[N],"text":"..."}}` | Type text into focused widget |
| `{"FindInFiles":{"mount":"makepad","pattern":"foo","is_regex":false}}` | Search source |
| `{"ReadTextRange":{"path":"...","start_line":N,"end_line":M}}` | Read source range |

### Key Response Types
- `BuildStarted`, `AppStarted` — wait for these before interacting
- `Screenshot` — returns `{path, width, height}` (not inline bytes)
- `WidgetTreeDump` — returns text dump with pixel coordinates
- `BuildStopped` — app exited (check `exit_code`)
- `Builds` — list of current builds

### Control Flow
1. Start bridge once, keep it alive for the whole session
2. `ListBuilds` → find old build for same runnable
3. `ClearBuild` for old build → immediately send `RunItem` (don't wait for ack)
4. Wait for `BuildStarted` + `AppStarted`
5. Use `Screenshot` / `WidgetTreeDump` to verify UI
6. After any code change → repeat steps 2–5 (never inspect stale build)

### Important Notes
- Build IDs are `QueryId` tuple structs → JSON: `[6]` (one-element array)
- `Screenshot` arrives before the next redraw sometimes — if stale, request again
- Do NOT send `ObserveMount` — it steals RunView/framebuffer from Studio desktop

---

## 4. DSL: `script_mod!` vs `live_design!`

The new system uses **`script_mod!`** with runtime evaluation. The old `live_design!` macro is **deprecated**.

### Import Preludes
```rust
use mod.prelude.widgets.*           // For app development
use mod.prelude.widgets_internal.*  // For internal widget library work
```

### Syntax Differences (Old → New)

| Old (`live_design!`) | New (`script_mod!`) |
|---------------------|---------------------|
| `<BaseWidget>` | `mod.widgets.BaseWidget{}` |
| `{{StructName}}` | `#(Struct::register_widget(vm))` |
| `(THEME_COLOR_X)` | `theme.color_x` |
| `<THEME_FONT>` | `theme.font_regular` |
| `instance hover: 0.0` | `hover: instance(0.0)` |
| `uniform color: #fff` | `color: uniform(#fff)` |
| `draw_bg: {}` (replace) | `draw_bg +: {}` (merge) |
| `default: off` | `default: @off` |
| `fn pixel(self)` | `pixel: fn()` |
| `(expr)` interpolation | `#(expr)` interpolation |

### Property Merging with `+:`
```rust
mod.widgets.MyButton = mod.widgets.Button {
    draw_bg +: {
        color: #f00   // Only overrides color; other draw_bg props preserved
    }
}
```

### Runtime Property Updates
```rust
// Old
item.apply_over(cx, live!{ height: (height) });

// New
script_apply_eval!(cx, item, {
    height: #(height)
    draw_bg: { color: #(color) }
});
```

### Theme Tokens
Always use `theme.` prefix:
```rust
color: theme.color_bg_app
padding: theme.space_2
text_style: theme.font_regular { font_size: 11.0 }
text_style: theme.font_bold { font_size: 10.0 }
text_style: theme.font_code { font_size: 11.0 }
```

---

## 5. Widget System

### Widget Registration
```rust
script_mod! {
    use mod.prelude.widgets.*

    // Register a widget struct
    mod.widgets.MyWidgetBase = #(MyWidget::register_widget(vm))

    // Define a styled variant
    mod.widgets.MyWidget = set_type_default() do mod.widgets.MyWidgetBase {
        width: Fill
        height: Fit
        // ...
    }
}
```

### Widget Struct Derives
```rust
#[derive(Script, ScriptHook, Widget, WidgetRef, WidgetSet, WidgetRegister)]
pub struct MyWidget {
    #[uid]     uid: WidgetUid,        // Required for action dispatch
    #[source]  source: ScriptObjectRef, // Required for script integration
    #[walk]    walk: Walk,
    #[layout]  layout: Layout,
    #[redraw]  #[live] draw_bg: DrawQuad,
    #[live]    draw_text: DrawText,
    #[rust]    my_state: i32,          // Runtime-only (not in DSL)
}
```

### Named Widget Instances (`:=`)
```rust
// In DSL
my_button := Button { text: "Click Me" }

// In Rust code
self.ui.button(cx, ids!(my_button)).clicked(actions)
self.ui.label(cx, ids!(editor_tab_label)).set_text(cx, "Hello")
self.ui.text_input(cx, ids!(path_input)).set_text(cx, path)
self.ui.splitter(cx, ids!(main_splitter)).set_align(cx, align)
self.ui.file_tree(cx, ids!(file_tree))
```

### Available Widgets
| Category | Widgets |
|---------|---------|
| Core | `View`, `SolidView`, `RoundedView`, `ScrollXView`, `ScrollYView`, `ScrollXYView` |
| Text | `Label`, `H1`, `H2`, `H3`, `LinkLabel`, `TextInput`, `TextInputFlat` |
| Buttons | `Button`, `ButtonFlat`, `ButtonFlatter` |
| Toggles | `CheckBox`, `Toggle`, `RadioButton` |
| Input | `Slider`, `DropDown` |
| Layout | `Splitter`, `FoldButton`, `FoldHeader`, `Hr`, `Filler` |
| Lists | `PortalList` |
| Navigation | `StackNavigation`, `ExpandablePanel` |
| Overlays | `Modal`, `Tooltip`, `PopupNotification` |
| Media | `Image`, `Icon`, `LoadingSpinner` |
| Special | `FileTree`, `PageFlip`, `CachedWidget`, `RectView` |
| Window | `Window`, `Root` |

---

## 6. Draw System & Turtles

### The Turtle Layout Engine
Makepad uses a **"turtle" metaphor** for layout. Each widget draws into a rectangular region managed by a turtle context on the `Cx2d`.

Key concepts:
- `cx.begin_turtle(walk, layout)` — push a new layout turtle
- `cx.end_turtle()` — pop and finalize the turtle, returning the drawn `Rect`
- `cx.end_turtle_with_area(&mut self.area)` — pop and record the area for hit-testing
- `cx.walk_turtle(walk)` — advance the cursor without drawing

**Critical**: You must have an active turtle context before calling draw methods. Calling `begin_turtle` outside a valid layout pass crashes with `Option::unwrap()` on `None` in `turtle.rs`.

### Draw Flow
```
Window::draw_walk()
  └── begin_pass() + begin_root_turtle()
       └── View::draw_walk()
            └── begin_turtle(walk, layout)
                 └── Child::draw_walk()
                      └── begin_turtle(...)  ← must be inside parent turtle
```

### DrawStep Two-Pass Pattern
Some widgets (like `FileTree`, `PortalList`) use a **two-pass state machine**:
```rust
fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
    if self.draw_state.begin(cx, ()) {
        self.begin(cx, walk);        // Sets up turtle, initializes state
        return DrawStep::make_step(); // Signal parent: "step in and fill me"
    }
    if let Some(()) = self.draw_state.get() {
        self.end(cx);                // Tears down turtle, fills blanks
        self.draw_state.end();
    }
    DrawStep::done()
}
```

The parent intercepts this via:
```rust
while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
    if let Some(mut file_tree) = step.as_file_tree().borrow_mut() {
        // begin() was already called by draw_walk! Don't call it again.
        // Just fill in the content:
        draw_dir(cx, &mut *file_tree, ".", &mut path_map);
    }
}
```

---

## 7. FileTree Widget

### Drawing Pattern
`FileTree` uses the two-pass state machine (see §6). When you intercept it via `step()`:
- **DO NOT** call `file_tree.begin()` or `file_tree.end()` — `draw_walk` handles those
- **DO** call `begin_folder / end_folder / file` directly on the borrowed reference

```rust
// CORRECT
while let Some(step) = self.ui.draw_walk(cx2d, &mut scope, walk).step() {
    if let Some(mut file_tree) = step.as_file_tree().borrow_mut() {
        // draw_walk already called begin() internally.
        // Just populate the tree:
        let _ = draw_dir(cx2d, &mut *file_tree, ".", &mut path_map);
        // draw_walk will call end() on the next iteration automatically.
    }
}

// WRONG — causes double-begin → panic in turtle.rs:1278
file_tree.begin(cx2d, Walk::fill());   // ← DON'T DO THIS
draw_dir(cx2d, &mut *file_tree, ".", &mut path_map);
file_tree.end(cx2d);                   // ← DON'T DO THIS
```

### Populating the Tree
```rust
fn draw_dir(cx: &mut Cx2d, tree: &mut FileTree, path: &str, path_map: &mut HashMap<LiveId, String>) {
    let entries = fs::read_dir(path)...;
    for entry in entries {
        let node_id = LiveId::from_str(&full_path);
        path_map.insert(node_id, full_path.clone());

        if is_dir {
            if tree.begin_folder(cx, node_id, &name).is_ok() {
                let _ = draw_dir(cx, tree, &full_path, path_map);
                tree.end_folder();
            }
        } else {
            tree.file(cx, node_id, &name);
        }
    }
}
```

### Handling File Click Actions
```rust
let file_tree = self.ui.file_tree(cx, ids!(file_tree));
if let Some(file_tree_action) = file_tree.borrow().and_then(|t| {
    let item = actions.find_widget_action(t.widget_uid())?;
    Some(item.cast::<FileTreeAction>())
}) {
    if let FileTreeAction::FileClicked(node_id) = file_tree_action {
        if let Some(path) = self.state.path_map.get(&node_id).cloned() {
            self.open_path(cx, &path);
        }
    }
}
```

### Opening Folders Programmatically
```rust
let file_tree = self.ui.file_tree(cx, ids!(file_tree));
file_tree.set_folder_is_open(cx, live_id!(some_folder), true, Animate::No);
```

---

## 8. Custom Widgets

### Full Widget Example
```rust
#[derive(Script, ScriptHook, Widget, WidgetRef, WidgetSet, WidgetRegister)]
pub struct IdeTabBar {
    #[uid]     uid: WidgetUid,
    #[source]  source: ScriptObjectRef,
    #[walk]    walk: Walk,
    #[layout]  layout: Layout,
    #[live]    draw_bg: DrawQuad,
    #[live]    draw_tab_bg: DrawColor,
    #[live]    draw_text: DrawText,
    #[rust]    area: Area,
}

impl WidgetNode for IdeTabBar {
    fn widget_uid(&self) -> WidgetUid { self.uid }
    fn walk(&mut self, _cx: &mut Cx) -> Walk { self.walk }
    fn area(&self) -> Area { self.area }
    fn redraw(&mut self, cx: &mut Cx) { self.area.redraw(cx); }
}

impl Widget for IdeTabBar {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, self.layout);
        // ... draw content ...
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        match event.hits(cx, self.area) {
            Hit::FingerDown(fe) => {
                // Hit testing against self.area
                cx.widget_action(self.widget_uid(), MyAction::Clicked);
            }
            _ => {}
        }
    }
}
```

### Widget Actions
```rust
#[derive(Clone, Debug, Default)]
pub enum MyAction {
    #[default] None,
    Clicked,
    SelectTab(usize),
}

impl ActionDefaultRef for MyAction {
    fn default_ref() -> &'static Self { &MyAction::None }
}

// Dispatch action
cx.widget_action(self.widget_uid(), MyAction::Clicked);

// Listen for action in parent
for action in actions {
    if let Some(wa) = action.as_widget_action() {
        if wa.widget_uid == self.ui.widget(cx, ids!(my_widget)).widget_uid() {
            if let Some(my_action) = wa.action.downcast_ref::<MyAction>() {
                // handle
            }
        }
    }
}
```

### DrawColor vs DrawQuad
- `DrawQuad` — basic colored quad, supports custom shaders via `pixel: fn()`
- `DrawColor` — simpler colored rectangle (no custom shader needed)
- Both support `.draw_abs(cx, rect)` for absolute positioning

---

## 9. App Architecture Pattern

```rust
app_main!(App);

script_mod! {
    use mod.prelude.widgets.*

    load_all_resources() do #(App::script_component(vm)) {
        ui: Root {
            main_window := Window {
                window.inner_size: vec2(1240, 800)
                pass.clear_color: #x181a1f
                body +: {
                    // UI layout here
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live] pub ui: WidgetRef,
    #[rust] state: MyState,   // Runtime-only state
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        crate::makepad_code_editor::script_mod(vm);  // if using code editor
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);

        if let Event::Draw(draw_event) = event {
            let mut cx_draw = CxDraw::new(cx, draw_event);
            let cx2d = &mut Cx2d::new(&mut cx_draw);
            let mut scope = Scope::with_data(&mut self.state);
            let walk = Walk::fill();
            // Draw loop — intercept special widgets
            while let Some(step) = self.ui.draw_walk(cx2d, &mut scope, walk).step() {
                if let Some(mut file_tree) = step.as_file_tree().borrow_mut() {
                    // Fill file tree content here
                }
            }
            drop(scope);
            // Update state after scope is dropped (borrow checker)
            self.state.path_map = path_map;
        } else {
            self.ui.handle_event(cx, event, &mut Scope::with_data(&mut self.state));
        }
    }
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) { /* init */ }
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) { /* respond to UI */ }
}
```

---

## 10. Event & Action System

### Event Types
```rust
match event {
    Event::Draw(draw_event) => { /* draw pass */ }
    Event::Startup => { /* app ready */ }
    _ => {}
}
```

### Match Event Trait
`MatchEvent` provides named hooks dispatched automatically by `self.match_event(cx, event)`:
- `handle_startup` — app just launched
- `handle_actions` — called every frame with all queued actions
- `handle_key_down`, `handle_key_up`
- `handle_finger_down`, `handle_finger_up`, `handle_finger_move`

### Action Dispatch & Matching
```rust
// Widget fires action
cx.widget_action(self.widget_uid(), MyAction::Clicked);

// In handle_actions
if self.ui.button(cx, ids!(my_btn)).clicked(actions) { ... }
if let Some((text, _mods)) = self.ui.text_input(cx, ids!(input)).returned(actions) { ... }
```

### Hit Testing
```rust
match event.hits(cx, self.area) {
    Hit::FingerDown(fe) => { /* fe.abs is the click position */ }
    Hit::FingerUp(_) => {}
    Hit::KeyDown(ke) => {}
    _ => {}
}
```

---

## 11. Scope & State Passing

`Scope` passes runtime data down through the widget tree during draw and event handling.

```rust
// Create scope with data
let mut scope = Scope::with_data(&mut self.state);

// In child widget, read the data
if let Some(state) = scope.data.get_mut::<MyState>() {
    // use state
}

// BORROW CHECKER PITFALL:
// If you need to update self.state after a draw loop that uses scope,
// you MUST drop the scope first:
let mut path_map = HashMap::new();
let mut scope = Scope::with_data(&mut self.state);
while let Some(step) = self.ui.draw_walk(cx2d, &mut scope, walk).step() {
    // populate path_map using &mut local, NOT self.state (which is borrowed by scope)
}
drop(scope);                          // ← drop before re-accessing self.state
self.state.path_map = path_map;       // ← now safe
```

---

## 12. Shader / SDF Drawing

### SDF2D Primitives
```rust
draw_bg +: {
    pixel: fn() {
        let sdf = Sdf2d.viewport(self.pos * self.rect_size)

        // Shapes
        sdf.rect(x, y, w, h)
        sdf.circle(cx, cy, r)
        sdf.box(x, y, w, h, radius)    // Rounded rect

        // Lines
        sdf.move_to(x, y)
        sdf.line_to(x, y)

        // Fill & stroke
        sdf.fill(color)
        sdf.stroke(color, width)

        return sdf.result
    }
}
```

### Shader Variables
- `self.pos` — normalized UV position (0..1)
- `self.rect_size` — pixel size of the widget rect
- `self.hover` — animator value for hover state (0.0–1.0)
- `self.down` — animator value for pressed state

### Instance vs Uniform
```rust
draw_bg +: {
    hover: instance(0.0)        // Per-draw-call, varies per widget
    color: uniform(theme.x)     // Shared across all instances
}
```

### Color Mixing in Shaders
```rust
// Chained (preferred)
color1.mix(color2, hover).mix(color3, down)

// With hover color interpolation
let hover_color = mix(#0000, #x2d3139, self.hover)
```

---

## 13. Common Pitfalls & Hard-Won Lessons

### ❌ Double-begin on FileTree → Panic
**Symptom**: `called Option::unwrap() on a None value` at `turtle.rs:1278`

**Cause**: Calling `file_tree.begin(cx, walk)` after intercepting the `DrawStep::make_step()` signal. `FileTree::draw_walk` already calls `begin()` on the first pass.

**Fix**: Remove manual `begin()`/`end()` calls. Just fill content:
```rust
// CORRECT
if let Some(mut file_tree) = step.as_file_tree().borrow_mut() {
    draw_dir(cx2d, &mut *file_tree, ".", &mut path_map);
}

// WRONG
file_tree.begin(cx2d, Walk::fill());  // ← panic here
```

---

### ❌ Borrow Checker: Scope borrows self.state
**Symptom**: `cannot borrow 'self.state' as mutable because it is also borrowed`

**Cause**: `Scope::with_data(&mut self.state)` mutably borrows `self.state`. Any attempt to access `self.state` inside the loop fails.

**Fix**: Use a local variable, drop scope, then assign:
```rust
let mut local_path_map = HashMap::new();
let mut scope = Scope::with_data(&mut self.state);
while let Some(step) = self.ui.draw_walk(cx2d, &mut scope, walk).step() {
    if let Some(mut ft) = step.as_file_tree().borrow_mut() {
        draw_dir(cx2d, &mut *ft, ".", &mut local_path_map);
    }
}
drop(scope);
self.state.path_map = local_path_map;  // safe now
```

---

### ❌ Using `cargo run` for UI programs
**Rule**: Never use `cargo run` / `cargo makepad` for runnable UI targets.
Always use the Studio `RunItem` bridge flow. Otherwise Cargo fingerprints, env vars, target dirs, and flags diverge from Studio's, causing subtle build or behavior differences.

---

### ❌ widget_flood for draw interception
`widget_flood` locates a widget by ID but does **not** set up a turtle context for it. Calling draw methods on a widget found via `widget_flood` outside the normal draw pass will crash.

**Fix**: Use the `draw_walk` step loop pattern instead.

---

### ❌ Old DSL syntax in new system
The `live_design!` macro and its syntax (`<Widget>`, `(expr)`, `Key = Value`) no longer work. Always use `script_mod!` with `Name: value` syntax.

---

### ❌ `ObserveMount` from bridge client
Sending `{"ObserveMount":...}` from the Studio remote bridge claims primary UI ownership for the mount, diverting RunView/framebuffer traffic away from Studio desktop. This breaks the Studio UI. Never send it from the bridge.

---

### ⚠️ Screenshot timing
Screenshots can arrive before the next redraw after rapid input bursts. If the screenshot looks stale, request `WidgetTreeDump` first, then another `Screenshot`.

---

### ⚠️ Splitter nested IDs
For nested splitters, use the full dot-path to target the correct one:
```rust
self.ui.splitter(cx, ids!(main_splitter))        // outer splitter
self.ui.splitter(cx, ids!(main_splitter.b))      // inner splitter (in 'b' pane)
```

---

### ⚠️ `DrawColor` in widget needs `#[live]`
If you have a `draw_tab_bg: DrawColor` field in a widget, mark it `#[live]` so it gets registered and initialized from the DSL.

---

## 14. makepad-ide Build Notes

### Crate Location
`studio/makepad_ide/src/main.rs`

### Cargo.toml Key Section
```toml
[package]
name = "makepad-ide"
version = "0.1.0"
edition = "2021"

[dependencies]
makepad-widgets = { path = "../../widgets" }
makepad-code-editor = { path = "../../code_editor" }
```

### Known Issues (as of 2026-06-10)

| Issue | Status | Notes |
|-------|--------|-------|
| FileTree double-begin panic | **Fixed** | Removed manual `begin()`/`end()` calls |
| Borrow checker scope issue | **Fixed** | Use local `path_map`, drop scope before assign |
| Nested splitter toggle | **Fixed** | Use `ids!(main_splitter.b)` for inner splitter |
| User icon in title bar | **Removed** | Per user request |

### Architecture Decisions

- **Tab state** is managed manually in `IdeState::tabs` (not a Dock widget)
- **File path → node ID** mapping uses `LiveId::from_str(&full_path)` as the key
- **Custom `IdeTabBar`** widget draws tabs imperatively (no template system) for flexibility
- **`IdeCodeEditor`** wraps `CodeEditor` and reads the active `CodeSession` from `Scope` data
- **Draw/event split**: Draw path uses `Scope::with_data(&mut self.state)`; event path uses same, separately

---

*This document is maintained automatically. Add discoveries as `### ⚠️ ...` or `### ❌ ...` entries in §13.*

---

## 15. Walk / Size / Layout / Align / Inset

Source: `draw/src/turtle.rs`

### ✅ The Layout Primitives Hierarchy

Every widget's position is controlled by two structs:
- **`Walk`** — describes how THIS widget occupies space in its parent's layout.
- **`Layout`** — describes how THIS widget's CHILDREN are arranged internally.

```
Parent Layout (Flow, Padding, Align, Spacing)
  └── Child Walk (Width, Height, Margin)
        └── Child Layout (its children's arrangement)
```

### ✅ `Walk` Fields

```rust
pub struct Walk {
    pub abs_pos: Option<Vec2d>,  // Override absolute position (bypass turtle)
    pub margin: Inset,           // Space OUTSIDE the widget rect
    pub width: Size,             // How much horizontal space to consume
    pub height: Size,            // How much vertical space to consume
    pub metrics: Metrics,        // Font metrics (descender, line_gap, line_scale)
}
```

**Convenience constructors:**
- `Walk::fill()` — both width and height are `Size::Fill { weight: 100.0 }`
- `Walk::fit()` — both width and height are `Size::Fit`
- `Walk::fixed(w, h)` — both fixed pixel sizes
- `Walk::fill_fit()` — width Fill, height Fit
- `Walk::empty()` — zero size

### ✅ `Size` Enum

```rust
pub enum Size {
    Fill { weight: f64, min: Option<f64>, max: Option<f64> },
    Fixed(f64),
    Fit { min: Option<FitBound>, max: Option<FitBound> },
}
```

| Variant | DSL spelling | Meaning |
|---------|-------------|--------|
| `Fill { weight: 100.0 }` | `Fill` | Take all remaining space (weighted) |
| `Fixed(n)` | `200.0` (bare number) | Exactly n pixels |
| `Fit` | `Fit` | Shrink to content |

**Key insight**: In the DSL, a bare number like `width: 200.0` is automatically converted to `Size::Fixed(200.0)` by `ScriptHook::on_custom_apply`.

**`Fill` weight** is used when multiple siblings all request `Fill` — they split remaining space proportionally by `weight`. Default weight is `100.0`, so equal-weight fills split evenly.

**`FitBound`** — min/max constraints for Fit sizing:
- `FitBound::Abs(n)` — absolute pixel constraint
- `FitBound::Rel { base: Base::Full, factor: 1.0 }` — relative to parent size

### ✅ `Layout` Fields

```rust
pub struct Layout {
    pub scroll: Vec2d,      // Scroll offset applied to children
    pub clip_x: bool,       // Clip children horizontally (default: true)
    pub clip_y: bool,       // Clip children vertically (default: true)
    pub flow: Flow,         // Direction children are arranged
    pub spacing: f64,       // Pixels between each child
    pub wrap_spacing: f64,  // Pixels between rows when wrapping
    pub padding: Inset,     // Space INSIDE the widget, before children
    pub align: Align,       // How to align children in unused space
}
```

**`Flow` variants:**
- `Flow::Right { row_align: RowAlign::Top, wrap: false }` — left-to-right (default)
- `Flow::Right { row_align: RowAlign::Center, wrap: true }` — left-to-right, wrapping
- `Flow::Down` — top-to-bottom
- `Flow::Overlay` — all children stacked at same position

**`RowAlign`** (for wrapped right-flow rows):
- `RowAlign::Top` — items sit at top of row (default)
- `RowAlign::Bottom` — items aligned to text baseline
- `RowAlign::Center` — items vertically centered in row

### ✅ `Align` — Aligning Children in Unused Space

```rust
pub struct Align {
    pub x: f64,  // 0.0 = left, 0.5 = center, 1.0 = right
    pub y: f64,  // 0.0 = top, 0.5 = middle, 1.0 = bottom
}
```

Pre-defined aliases in `mod.turtle`:
- `TopLeft` → `Align{x:0., y:0.}`
- `Center` → `Align{x:0.5, y:0.5}`
- `HCenter` → `Align{x:0.5, y:0.}`
- `VCenter` → `Align{x:0., y:0.5}`

### ✅ `Inset` — Padding and Margin

`Inset` has four fields: `left`, `top`, `right`, `bottom`.

In DSL:
```rust
padding: 8.0          // All sides equal
margin: {left: 4, top: 2, right: 4, bottom: 2}
```

In Rust:
```rust
cx.begin_turtle(walk.with_margin(Inset::all(8.0)), layout.with_padding(Inset::all(4.0)));
```

### ⚠️ Margin vs Padding
- **`Walk::margin`** = space OUTSIDE the widget's rect (affects parent's layout)
- **`Layout::padding`** = space INSIDE the widget's rect (affects children placement)
- **`Walk::abs_pos`** = bypasses turtle positioning entirely — widget is drawn at exact coords

---

## 16. Animator & Animation State Machines

Source: `widgets/src/animator.rs`, `widgets/src/button.rs`

### ✅ How Animators Work

The `Animator` struct is a runtime state machine. It holds:
- **Groups** — named sets of states (e.g., `hover`, `focus`, `disabled`)
- **States** — named end-values within a group (e.g., `off`, `on`, `down`)
- **Tracks** — active animations (interpolating from snapshot → target)

Animations run via `NextFrame` events. On each frame, the animator interpolates all active tracks and writes the result into a shared `state_object`, which is then applied to the widget.

### ✅ DSL Definition (in `script_mod!`)

```rust
animator: Animator {
    // Group: "hover"
    hover: {
        default: @off        // The initial/default state

        off: AnimatorState {
            from: { all: Forward { duration: 0.1 } }  // How to enter "off" from any state
            apply: {
                draw_bg: { hover: 0.0 }
                draw_text: { hover: 0.0 }
            }
        }

        on: AnimatorState {
            from: {
                all: Forward { duration: 0.1 }    // Default: from any state
                down: Forward { duration: 0.01 }  // Override: from "down" state
            }
            apply: {
                draw_bg: { hover: snap(1.0) }  // snap() = no interpolation
                draw_text: { hover: snap(1.0) }
            }
        }

        down: AnimatorState {
            from: { all: Forward { duration: 0.2 } }
            apply: {
                draw_bg: { down: snap(1.0), hover: 1.0 }
                draw_text: { down: snap(1.0), hover: 1.0 }
            }
        }
    }
}
```

### ✅ `Play` Modes

| Mode | Meaning |
|------|--------|
| `Forward { duration }` | Animate from current value to target in `duration` seconds |
| `Snap` | Jump immediately to target (no interpolation) |
| `Reverse { duration, end }` | Animate backwards |
| `Loop { duration, end }` | Loop forward continuously |
| `ReverseLoop { duration, end }` | Loop backwards continuously |
| `BounceLoop { duration, end }` | Ping-pong back and forth |

`from: { all: X }` sets the default play mode. `from: { state_name: Y }` overrides for transitions from a specific state.

### ✅ `Ease` Functions

Available ease variants: `Linear`, `None` (snap at 0.5), `InQuad`, `OutQuad`, `InOutQuad`, `InCubic`, `OutCubic`, `InOutCubic`, `InQuart`–`InOutQuint`, `InSine`–`InOutSine`, `InExp`–`InOutExp`, `InCirc`–`InOutCirc`, `InElastic`–`InOutElastic`, `InBack`–`InOutBack`, `InBounce`–`InOutBounce`, `ExpDecay { d1, d2, max }`, `Pow { begin, end }`, `Bezier { cp0, cp1, cp2, cp3 }`.

### ✅ Rust Struct Setup

```rust
// Derive Animator on your widget struct
#[derive(Script, ScriptHook, Widget, Animator)]
pub struct MyWidget {
    #[apply_default]    // ← required attribute (not #[live])
    animator: Animator,
    // ...
}
```

**Important**: Use `#[apply_default]` not `#[live]` for the `animator` field.

### ✅ Triggering Animations from Rust

```rust
impl Widget for MyWidget {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        // Step 1: Always call this first — it drives NextFrame interpolation
        if self.animator_handle_event(cx, event).must_redraw() {
            self.draw_bg.redraw(cx);
        }

        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(_) => {
                // Animated transition to "on" state of "hover" group
                self.animator_play(cx, ids!(hover.on));
            }
            Hit::FingerHoverOut(_) => {
                self.animator_play(cx, ids!(hover.off));
            }
            Hit::FingerDown(_) => {
                self.animator_play(cx, ids!(hover.down));
            }
            Hit::FingerUp(_) => {
                // Cut = jump immediately (no animation)
                self.animator_cut(cx, ids!(hover.off));
            }
            _ => {}
        }
    }
}
```

### ✅ Key `AnimatorImpl` Methods

| Method | Effect |
|--------|-------|
| `animator_handle_event(cx, event)` | Must be called every frame; drives NextFrame interpolation |
| `animator_play(cx, ids!(group.state))` | Animated transition to state |
| `animator_cut(cx, ids!(group.state))` | Immediate jump to state (no animation) |
| `animator_toggle(cx, bool, animate, ids!(s1), ids!(s2))` | Toggle between two states |
| `animator_in_state(cx, ids!(group.state))` | Query whether currently in a state |
| `animator_play_with(cx, ids!(g.s), play)` | Override the Play mode for this transition |

### ✅ `snap()` in `apply`

In `apply` blocks, wrap a value with `snap()` to prevent it from being interpolated:
```rust
apply: {
    draw_bg: { down: snap(1.0), hover: 1.0 }
    // `down` jumps instantly; `hover` interpolates smoothly
}
```

### ⚠️ `animator_handle_event` must come first
Always call `self.animator_handle_event(cx, event)` at the TOP of `handle_event`, before processing hits. If called after, animations won't fire on the same frame as the triggering input.

### ⚠️ Using `#[apply_default]` not `#[live]`
The `animator` field MUST be annotated `#[apply_default]`, NOT `#[live]`. Using `#[live]` breaks the state persistence between frames.

---

## 17. PortalList & Template System

Source: `widgets/src/portal_list.rs`

### ✅ How Templates Work

In a `PortalList`, named items using `:=` are **templates** — they go into the script object's `vec` (not `map`), and are collected in `on_after_apply` into a `HashMap<LiveId, ScriptObjectRef>`.

```rust
my_list := PortalList {
    width: Fill
    height: Fill
    flow: Down

    // Regular property — goes into map, applied to struct field directly
    scroll_bar: mod.widgets.ScrollBar {}

    // Templates — go into vec, stored in self.templates HashMap
    Item := View {
        height: 40
        title := Label { text: "" }
    }
    Header := View {
        height: 24
        draw_bg: { color: #333 }
        label := Label { text: "" }
    }
}
```

### ✅ The `on_after_apply` Collection

```rust
impl ScriptHook for PortalList {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if let Some(id) = kv.key.as_id() {
                            if let Some(template_obj) = kv.value.as_object() {
                                // Root the template to prevent GC collection
                                self.templates.insert(id, vm.bx.heap.new_object_ref(template_obj));
                            }
                        }
                    }
                });
            }
        }
    }
}
```

### ✅ Draw Loop Pattern

```rust
impl Widget for MyListWidget {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.borrow_mut::<PortalList>() {
                // Set the total item count (0..99 = 100 items)
                list.set_item_range(cx, 0, 100);

                // Draw loop — yields only the currently visible items
                while let Some(item_id) = list.next_visible_item(cx) {
                    // item() returns existing widget or instantiates from template
                    // Template key must match the := name in DSL
                    let item = list.item(cx, item_id, id!(Item));

                    // Populate item content
                    item.label(ids!(title)).set_text(cx, &format!("Row {}", item_id));

                    // Draw the item — required to close out the turtle
                    item.draw_all(cx, &mut Scope::empty());
                }
            }
        }
        DrawStep::done()
    }
}
```

### ✅ Multiple Templates

You can have multiple template types and switch based on item content:
```rust
while let Some(item_id) = list.next_visible_item(cx) {
    let (item, is_header) = if item_id % 10 == 0 {
        (list.item(cx, item_id, id!(Header)), true)
    } else {
        (list.item(cx, item_id, id!(Item)), false)
    };

    if is_header {
        item.label(ids!(label)).set_text(cx, &format!("Section {}", item_id / 10));
    } else {
        item.label(ids!(title)).set_text(cx, &format!("Item {}", item_id));
    }
    item.draw_all(cx, &mut Scope::empty());
}
```

### ✅ PortalList Properties

| Property | Default | Purpose |
|---------|---------|--------|
| `auto_tail` | `false` | Automatically scroll to keep bottom visible |
| `smooth_tail` | `false` | Smooth animation for auto-tail scrolling |
| `reuse_items` | `false` | Pool and reuse widget instances for performance |
| `keep_invisible` | `false` | Keep off-screen items alive (for text selection) |
| `selectable` | `false` | Enable cross-item text selection |
| `align_top_when_empty` | `true` | Align content at top when not filling viewport |
| `drag_scrolling` | `true` | Enable touch/mouse drag scrolling |

### ❌ Forgetting `draw_all()` Inside the Loop
Every item retrieved with `list.item(cx, ...)` MUST have `item.draw_all(cx, scope)` called. If you skip it, the turtle state machine gets misaligned and the layout corrupts.

### ❌ Calling `set_item_range` After `next_visible_item`
`set_item_range` must be called BEFORE the `next_visible_item` loop. It sets up the range used by the draw state machine.

---

## 18. LiveId Macros: `id!` vs `ids!` vs `live_id!`

Source: `libs/live_id/id_macros/src/lib.rs`

### ✅ What `LiveId` Is

`LiveId(u64)` is a 64-bit hash of a string, computed at compile time using a FNV-like hash. It's the universal identifier type throughout Makepad for widgets, properties, states, etc.

```rust
LiveId::from_str("my_name")  // Runtime computation (const fn)
live_id!(my_name)            // Compile-time expansion to LiveId(0xABC...)
```

### ✅ The Three Macros Compared

| Macro | Return type | Example usage |
|-------|------------|---------------|
| `live_id!(foo)` | `LiveId` | Single ID, typically for named constants or node IDs |
| `id!(foo)` | `LiveId` | Alias for `live_id!`, identical output |
| `ids!(foo.bar)` | `&[LiveId]` | Dot-separated path — used for widget lookup AND animator state |

```rust
// live_id! — single LiveId constant
let node_id = live_id!(root);
file_tree.set_folder_is_open(cx, live_id!(root), true, Animate::No);

// id! — identical to live_id!, just shorter
let template = id!(Item);  // ← used with list.item(cx, item_id, id!(Item))

// ids! — a &[LiveId; N] slice
// Dot separators become separate elements in the array:
self.animator_play(cx, ids!(hover.on));     // &[LiveId(hover), LiveId(on)]
self.ui.button(ids!(my_btn)).clicked(actions); // navigate widget tree by path
self.ui.label(ids!(panel.header.title)).set_text(cx, "Hello");
```

### ✅ When to Use Each

- **`live_id!(name)`** — when you need a single `LiveId` value. Used for:
  - FileTree node IDs: `file_tree.begin_folder(cx, live_id!(src), "src")`
  - Animator group/state comparison: `if prop == live_id!(root)`
  - Internal widget method dispatch

- **`id!(name)`** — identical to `live_id!`. Prefer for template keys in `list.item(cx, item_id, id!(Item))`.

- **`ids!(a.b.c)`** — produces `&[LiveId]`. Used for:
  - Widget tree navigation: `self.ui.button(ids!(panel.footer.save_btn))`
  - Animator state: `self.animator_play(cx, ids!(hover.on))`
  - Any API that takes `&[LiveId]` (widget path)

### ⚠️ `ids!` is NOT just two IDs joined by a dot
The dots are literal separators that create array elements. `ids!(hover.on)` = `&[LiveId("hover"), LiveId("on")]`. Both the widget lookup system and the animator API consume these as paths.

### ⚠️ Collisions are theoretically possible but extremely rare
`LiveId` is a 46-bit hash (top bits reserved). The seed `0xd6e8_feb8_6659_fd93` is fixed. In practice there are no collisions in the Makepad codebase itself.

---

## 19. `#[deref]` Widget Composition

Source: `widgets/src/expandable_panel.rs`, `widgets/src/widget.rs`

### ✅ What `#[deref]` Does

Marking a field `#[deref]` on a widget struct tells the `Widget` derive macro to delegate `Widget`/`WidgetNode` trait methods to that inner field. It's the standard way to wrap an existing widget:

```rust
#[derive(Script, ScriptHook, Widget)]
pub struct ExpandablePanel {
    #[source]
    source: ScriptObjectRef,

    #[deref]            // ← All Widget + WidgetNode methods delegate to `view`
    view: View,         //   unless overridden in this impl block

    #[rust]
    touch_gesture: Option<TouchGesture>,
    #[live]
    initial_offset: f64,
}

impl Widget for ExpandablePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // Delegate to inner view first (handles children)
        self.view.handle_event(cx, event, scope);
        // Then add custom behavior
        // ...
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // Apply custom margin before drawing
        let panel_ref = self.view(cx, ids!(panel));
        // ...
        // Delegate drawing to inner view
        self.view.draw_walk(cx, scope, walk)
    }
}
```

### ✅ What Gets Delegated

With `#[deref]`, the derive macro auto-generates `WidgetNode` impls that forward:
- `widget_uid()` — returns the deref field's uid
- `walk()`, `area()`, `redraw()` — geometry/area from deref field
- `children()` — widget tree enumeration
- `selection_*()` methods — text selection support

You only need to override the methods where your custom logic differs.

### ✅ Typical Pattern

```rust
// 1. Wrap a View (the most common deref target)
#[derive(Script, ScriptHook, Widget)]
pub struct MyPanel {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,           // ← all layout/draw/children delegate here
    #[rust] my_state: Vec<String>, // ← custom runtime state
}

impl Widget for MyPanel {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // Put custom drawing logic here, then delegate:
        self.view.draw_walk(cx, scope, walk)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // Custom event handling...
    }
}

// 2. In script_mod!, define the widget using the View as a container
script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.MyPanelBase = #(MyPanel::register_widget(vm))

    mod.widgets.MyPanel = mod.widgets.MyPanelBase {
        width: Fill
        height: Fill
        flow: Down
        // All View properties work here because MyPanel derives from View via #[deref]

        header := Label { text: "Header" }
        body := View { width: Fill, height: Fill }
    }
}
```

### ✅ Multiple `#[deref]`-like Attributes

The `Widget` derive macro supports related attributes:
- `#[deref]` — primary delegation (most common, use for the main inner widget)
- `#[wrap]` — similar to deref but for container widgets that wrap a single child
- `#[find]` — enables widget tree search to descend into this field

### ⚠️ Only One `#[deref]` Per Struct
Using multiple `#[deref]` fields causes a compile error. If you need multiple inner widgets, use `#[live]` for the others and delegate manually.

### ⚠️ `#[deref]` vs `#[live]` for Inner Widgets
- `#[deref]` — use when the inner widget IS the widget (layout/area/children come from it)
- `#[live]` — use when the inner widget is just a draw primitive (e.g., `draw_bg: DrawQuad`)

---

## 20. Cx vs CxDraw vs Cx2d

Source: `draw/src/cx_2d.rs`, `draw/src/cx_draw.rs`, `platform/src/`

### ✅ The Three Context Types

```
Cx          — the platform context (always available, event handling, GPU state)
 │
 └── CxDraw  — wraps Cx during a draw pass (adds fonts, pass stack, draw lists)
       │       (accessible as &mut Cx via Deref)
       │
       └── Cx2d — wraps CxDraw during 2D rendering (adds turtles, align list, overlay)
                  (accessible as &mut CxDraw and &mut Cx via Deref chain)
```

### ✅ `Cx` — Platform Context

Available everywhere (event handling, actions, startup, etc.).

Key capabilities:
- `cx.widget_action(uid, action)` — dispatch widget actions
- `cx.set_key_focus(area)` — keyboard focus management
- `cx.set_cursor(cursor)` — mouse cursor
- `cx.redraw_all()` — request full redraw
- `cx.new_next_frame()` — schedule a NextFrame event (for animations)
- `cx.with_vm(|vm| {...})` — access ScriptVm (for script calls)
- `cx.get_global::<T>()` / `cx.set_global(val)` — global state storage
- Platform: `cx.start_stdin_service()` (macOS)

### ✅ `CxDraw` — Draw Pass Context

Only available during `Event::Draw`. Constructed with `CxDraw::new(cx, draw_event)`.

Adds to `Cx`:
- `cx_draw.time()` — current animation time from the draw event
- `cx_draw.current_dpi_factor()` — display DPI
- `cx_draw.begin_pass(pass, dpi_override)` / `end_pass(pass)` — render pass management
- `cx_draw.current_pass_size()` — size of the current render pass
- Font system: `cx_draw.fonts` — shared font atlas
- Navigation tree: `cx_draw.nav_tree_rc`

`CxDraw` derefs to `Cx`, so all `Cx` methods are available on `&mut CxDraw`.

### ✅ `Cx2d` — 2D Rendering Context

Only available inside `draw_walk`. Wraps `CxDraw` and adds the turtle layout engine.

Adds to `CxDraw`:
- `cx.begin_turtle(walk, layout)` / `end_turtle()` — layout rectangle allocation
- `cx.end_turtle_with_area(&mut area)` — record area for hit testing
- `cx.turtle()` — access current turtle's rect/size/pos
- `cx.walk_turtle(walk)` — allocate space without beginning a child turtle
- `cx.peek_walk_turtle(walk)` — peek at where a walk would land (for dirty checking)
- `cx.add_nav_stop(area, role, inset)` — keyboard navigation
- `cx.will_redraw(draw_list, walk)` — dirty-check before drawing

`Cx2d` derefs to `CxDraw` (which derefs to `Cx`), so all `Cx` and `CxDraw` methods are available.

### ✅ Constructing in `handle_event`

```rust
impl AppMain for App {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);  // uses &mut Cx

        if let Event::Draw(draw_event) = event {
            // Must create CxDraw first
            let mut cx_draw = CxDraw::new(cx, draw_event);
            // Then Cx2d wraps CxDraw
            let cx2d = &mut Cx2d::new(&mut cx_draw);

            // Now draw — cx2d has access to all turtle methods
            let walk = Walk::fill();
            let mut scope = Scope::empty();
            while self.ui.draw_walk(cx2d, &mut scope, walk).is_step() {}
        } else {
            // Event handling only needs &mut Cx
            self.ui.handle_event(cx, event, &mut Scope::empty());
        }
    }
}
```

### ⚠️ Never use `Cx2d` outside a draw pass
`Cx2d` is only valid inside the draw callback. The turtle stack, align list, and draw list stack are all frame-local. Storing `Cx2d` or trying to use turtle methods outside `Event::Draw` will panic or produce undefined behavior.

### ⚠️ `CxDraw::new` increments `redraw_id`
Every call to `CxDraw::new` increments `cx.redraw_id`, which is used to dirty-check draw lists. This happens automatically — you don't need to manage it.

### ⚠️ `CxDraw` Drop prepares font textures
When `CxDraw` is dropped (at the end of the draw block), it automatically calls `fonts.prepare_textures(cx)`. If new font glyphs were rasterized, it schedules a `redraw_all()`. This is why font changes take effect on the next frame.
