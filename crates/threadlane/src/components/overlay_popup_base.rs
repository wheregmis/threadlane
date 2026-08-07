//! Overlay popup utility helpers for layout clamping and input dismissal.

use makepad_widgets::*;

/// Clamps popup origin coordinates `(x, y)` so the popup surface stays within pass dimensions `pass_size`
/// while preserving a margin gap `edge_gap`.
pub fn clamp_popup_position(
    mut pos: DVec2,
    popup_size: DVec2,
    pass_size: DVec2,
    edge_gap: f64,
) -> DVec2 {
    if pos.x + popup_size.x > pass_size.x - edge_gap {
        pos.x = (pass_size.x - popup_size.x - edge_gap).max(edge_gap);
    }
    if pos.y + popup_size.y > pass_size.y - edge_gap {
        pos.y = (pass_size.y - popup_size.y - edge_gap).max(edge_gap);
    }
    pos.x = pos.x.max(edge_gap);
    pos.y = pos.y.max(edge_gap);
    pos
}

/// Checks if a pointer event or keypress event should trigger dismissal of an overlay popup.
pub fn is_overlay_dismissal_event(event: &Event, popup_rect: Rect) -> bool {
    match event {
        Event::MouseUp(e) if e.button.is_primary() => !popup_rect.contains(e.abs),
        Event::KeyDown(e) if e.key_code == KeyCode::Escape => true,
        _ => false,
    }
}
