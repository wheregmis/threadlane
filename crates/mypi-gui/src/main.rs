pub use makepad_widgets;

mod app;
mod chat;
mod components;
mod command_text_input;
mod state;

use app::App;
use makepad_widgets::*;

app_main!(App);
