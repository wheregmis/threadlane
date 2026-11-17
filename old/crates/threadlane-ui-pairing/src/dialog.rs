//! Remembered devices and explicit LAN pairing invitations.

use gpui::prelude::*;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{v_flex, ActiveTheme, Disableable, Sizable, WindowExt};
use threadlane_ui_state::AppState;

/// Open the pairing dialog; call from a click handler that owns a
/// `&mut Window`.
pub fn open_pairing_dialog(model: Entity<AppState>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| PairingDialogView::new(model, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title("Shared devices")
            .w(px(448.))
            .child(view.clone())
    });
}

/// Dialog content: a live view over `AppState::pairing` so the QR appears
/// the moment the async listener bind resolves.
struct PairingDialogView {
    model: Entity<AppState>,
    remove_confirmation: Option<String>,
    remove_all_confirmation: bool,
    _refresh: Task<()>,
    _subscription: Subscription,
}

impl PairingDialogView {
    fn new(model: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&model, |_, _, cx| cx.notify());
        let refresh = cx.spawn(async move |this, cx| {
            let mut previous = None;
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                if this.update(cx, |this, cx| {
                    let state = this.model.read(cx);
                    let snapshot = (
                        state.paired_devices(),
                        state.pending_pairing_invitation().map(|info| info.device_id),
                    );
                    if previous.as_ref() != Some(&snapshot) {
                        previous = Some(snapshot);
                        cx.notify();
                    }
                }).is_err() {
                    break;
                }
            }
        });
        Self {
            model,
            remove_confirmation: None,
            remove_all_confirmation: false,
            _refresh: refresh,
            _subscription: subscription,
        }
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self.model.update(cx, |state, _| {
            let task = state.start_pairing()?;
            Ok((state.pairing_generation(), task))
        });
        cx.notify();
        match pending {
            Ok((generation, task)) => {
                let model = self.model.clone();
                cx.spawn_in(window, async move |_, cx| {
                    let result = match task.await {
                        Ok(result) => result,
                        Err(error) => Err(format!("pairing listener failed: {error}")),
                    };
                    model.update(cx, |state, cx| {
                        state.finish_pairing_start(generation, result);
                        cx.notify();
                    });
                })
                .detach();
            }
            Err(error) => {
                self.model.update(cx, |state, cx| {
                    state.pairing_error = Some(error);
                    cx.notify();
                });
            }
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.remove_all_confirmation = false;
        let pending = self
            .model
            .update(cx, |state, _| state.remove_all_pairing());
        match pending {
            Ok((generation, task)) => {
                let model = self.model.clone();
                cx.spawn(async move |_, cx| {
                    let result = match task.await {
                        Ok(result) => result,
                        Err(error) => Err(format!("pairing shutdown failed: {error}")),
                    };
                    model.update(cx, |state, cx| {
                        state.finish_remove_all_pairing(generation, result);
                        cx.notify();
                    });
                })
                .detach();
            }
            Err(error) => {
                self.model.update(cx, |state, cx| {
                    state.pairing_error = Some(error);
                    cx.notify();
                });
            }
        }
        cx.notify();
    }

    fn add_device(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |state, cx| {
            state.pairing_error = state.begin_pairing().err();
            cx.notify();
        });
    }

    fn remove_device(&mut self, id: &str, cx: &mut Context<Self>) {
        self.model.update(cx, |state, cx| {
            state.pairing_error = state.remove_paired_device(id).err();
            cx.notify();
        });
        self.remove_confirmation = None;
        cx.notify();
    }
}

impl Render for PairingDialogView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.model.read(cx);
        let theme = cx.theme().colors;
        let sharing = state.pairing.is_some();
        let pairing = state.pending_pairing_invitation();
        let devices = state.paired_devices();
        let pairing_error = state.pairing_error.clone();
        let recovery = !sharing && pairing_error.is_some();
        let starting = state.pairing_starting;

        let mut content = v_flex()
            .id("shared-devices-content")
            .max_h(rems(34.))
            .overflow_y_scroll()
            .gap_4()
            .text_sm()
            .child(div().text_color(theme.muted_foreground).child(
                "Pair once. Saved devices reconnect when both apps are open on the same network, until you remove access.",
            ));
        for device in &devices {
            let id = device.id.clone();
            let confirming = self.remove_confirmation.as_deref() == Some(&id);
            let mut row = v_flex().gap_2().pb_3().border_b_1().border_color(theme.border)
                .child(div().flex().items_center().gap_3()
                    .child(v_flex().flex_1().min_w_0().gap_1()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(device.name.clone()))
                        .child(div().text_xs().text_color(theme.muted_foreground).child("Remembered · automatic reconnect")))
                    .child(Button::new(SharedString::from(format!("remove-{}", id)))
                        .ghost().small().label("Remove…")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_confirmation = Some(id.clone());
                            this.remove_all_confirmation = false;
                            cx.notify();
                        }))));
            if confirming {
                let id = device.id.clone();
                row = row.child(div().text_color(theme.muted_foreground)
                    .child(format!("Remove access for {}? It will disconnect and need a new pairing code.", device.name)))
                    .child(div().flex().gap_2()
                        .child(Button::new("cancel-remove-device").small().label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.remove_confirmation = None;
                                cx.notify();
                            })))
                        .child(Button::new("confirm-remove-device").small().danger().label("Remove access")
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_device(&id, cx)))));
            }
            content = content.child(row);
        }
        if let Some(error) = pairing_error {
            content = content.child(div().text_xs().text_color(theme.danger).child(error));
        }
        if recovery {
            content = content.child(div().text_xs().text_color(theme.muted_foreground)
                .child("Retry after resolving the error, or remove saved access and set up sharing again."));
        }
        if let Some(info) = pairing {
            let uri = info.uri();
            content = content
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("Scan once with your phone’s camera to add this device."),
                )
                .child(div().flex().justify_center().child(qr_grid(&uri)))
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("{}:{}", info.host, info.port)),
                        )
                        .child(
                            Button::new("pairing-copy-uri")
                                .ghost()
                                .small()
                                .label("Copy pairing link")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(uri.clone()));
                                }),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.warning)
                        .child("This code grants control of your sessions. Share it only with a device you trust."),
                );
        } else if sharing {
            content = content.child(Button::new("pairing-add").label("Add device…")
                .on_click(cx.listener(|this, _, _, cx| this.add_device(cx))));
        } else {
            content = content.child(
                Button::new("pairing-start")
                    .primary()
                    .label(if starting {
                        "Restoring sharing…"
                    } else {
                        "Share with a device"
                    })
                    .disabled(starting)
                    .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
            );
        }
        if sharing || (recovery && !starting) {
            if self.remove_all_confirmation {
                content = content
                    .child(div().text_color(theme.muted_foreground)
                        .child("Remove all devices and turn off sharing? Every device will need to pair again."))
                    .child(div().flex().gap_2()
                        .child(Button::new("cancel-remove-all").small().label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.remove_all_confirmation = false;
                                cx.notify();
                            })))
                        .child(Button::new("confirm-remove-all").small().danger().label("Remove all devices")
                            .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))));
            } else {
                content = content.child(Button::new("pairing-stop").ghost().small()
                    .label(if recovery { "Remove saved devices…" } else if devices.is_empty() { "Turn off sharing…" } else { "Remove all devices…" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.remove_all_confirmation = true;
                        this.remove_confirmation = None;
                        cx.notify();
                    })));
            }
        }
        content
    }
}

/// QR cell size. A pairing URI is ~90 chars → a version ~5 code, 37×37
/// modules incl. quiet zone → about 220px square, which fits the dialog.
const QR_CELL: f32 = 5.0;

/// Render `data` as a QR bitmap made of GPUI divs — no image pipeline
/// needed and it rasterizes crisply at any display scale.
fn qr_grid(data: &str) -> AnyElement {
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else {
        return div().child("Could not encode pairing link").into_any_element();
    };
    let colors = code.to_colors();
    let width = code.width();
    let cell = px(QR_CELL);
    div()
        // The QR spec requires a quiet zone of ≥4 modules around the
        // code for scanners to detect it reliably.
        .p(px(QR_CELL * 4.0))
        // Audited exception to theme tokens: scanners need dark modules on a
        // light field with maximum contrast, and many cannot read inverted
        // codes, so the QR stays literal black-on-white in every theme.
        .bg(rgb(0xffffff))
        .rounded_md()
        .flex()
        .flex_col()
        .children((0..width).map(|y| {
            div()
                .flex()
                .children((0..width).map(|x| {
                    let dark = colors[y * width + x] == qrcode::Color::Dark;
                    div()
                        .w(cell)
                        .h(cell)
                        .when(dark, |cell| cell.bg(rgb(0x000000)))
                }))
        }))
        .into_any_element()
}
