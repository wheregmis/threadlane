pub use makepad_widgets;

mod app;
mod chat;
mod command_text_input;
mod components;
mod state;
mod workspace;

use app::App;
use makepad_widgets::*;

app_main!(App);
