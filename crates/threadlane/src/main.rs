pub use makepad_widgets;

mod app;
mod components;
mod panels;
mod path_utils;
mod state;
mod updater;
mod workspace;

use app::App;
use makepad_widgets::*;

app_main!(App);
