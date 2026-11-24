//! The user-initiated update flow in App Settings and Space Settings.

use gpui_kit::component::{Disableable as _, v_flex};
use gpui_kit::*;
use threadlane_core::client::HubUpdate;
use threadlane_updater::Component;

use crate::Desktop;
use crate::components::{button, meta};
use crate::tokens::XS;

#[derive(Debug)]
pub(crate) enum UpdateState {
    Checking,
    Available(String),
    Installing(String),
    Current,
    Installed(String),
    Failed(String),
}

impl Desktop {
    pub(crate) fn update_controls(
        &self,
        space: Option<&str>,
        hub_version: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let scope = space.map(str::to_owned);
        let mut controls = v_flex().gap(XS);
        if space.map_or(!self.desktop_updatable, |_| hub_version.is_none()) {
            return controls.child(meta(
                "In-app updates require an official release package.",
                cx,
            ));
        }

        let (label, status, install, busy) = match self.updates.get(&scope) {
            Some(UpdateState::Checking) => ("Checking for updates…", None, None, true),
            Some(UpdateState::Available(version)) => (
                if space.is_some() {
                    "Update and restart hub"
                } else {
                    "Update and restart app"
                },
                Some(if space.is_some() {
                    format!(
                        "Version {version} is available. The hub will stop active runs as it restarts."
                    )
                } else {
                    format!("Version {version} is available.")
                }),
                Some(version.clone()),
                false,
            ),
            Some(UpdateState::Installing(version)) => (
                "Updating…",
                Some(format!("Installing version {version} and restarting.")),
                None,
                true,
            ),
            state => (
                "Check for updates",
                match state {
                    Some(UpdateState::Current) => Some("You're up to date.".to_owned()),
                    Some(UpdateState::Installed(version)) => {
                        Some(format!("Hub restarted on version {version}."))
                    }
                    Some(UpdateState::Failed(error)) => Some(error.clone()),
                    _ => None,
                },
                None,
                false,
            ),
        };
        let id = SharedString::from(format!("update-release-{space:?}"));
        if let Some(status) = status {
            controls = controls.child(meta(status, cx));
        }
        controls.child(
            div().child(
                button(id)
                    .outline()
                    .label(label)
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_release(scope.clone(), install.clone(), cx);
                    })),
            ),
        )
    }

    fn update_release(
        &mut self,
        space: Option<String>,
        version: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            self.updates.get(&space),
            Some(UpdateState::Checking | UpdateState::Installing(_))
        ) {
            return;
        }
        let installing = version.is_some();
        self.updates.insert(
            space.clone(),
            version
                .clone()
                .map_or(UpdateState::Checking, UpdateState::Installing),
        );
        cx.notify();

        let core = self.core.clone();
        let target = space.clone();
        self.run(
            async move {
                if let Some(space) = target {
                    let request =
                        version.map_or(HubUpdate::Check, |version| HubUpdate::Install { version });
                    let update = core.read().await.update_hub(&space, request);
                    return update.await.map_err(|error| error.to_string());
                }
                tokio::task::spawn_blocking(move || {
                    if let Some(version) = version {
                        let installed = Component::Desktop.install(&version)?;
                        threadlane_updater::relaunch_desktop(&installed)?;
                        Ok(Some(version))
                    } else {
                        Component::Desktop.check()
                    }
                })
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| format!("{error:#}"))
            },
            move |this, result, cx| {
                let state = match result {
                    Ok(Some(_)) if installing && space.is_none() => {
                        cx.quit();
                        return;
                    }
                    Ok(Some(version)) if installing => {
                        this.refresh(cx);
                        UpdateState::Installed(version)
                    }
                    Ok(Some(version)) => UpdateState::Available(version),
                    Ok(None) => UpdateState::Current,
                    Err(error) => UpdateState::Failed(error),
                };
                this.updates.insert(space, state);
            },
            cx,
        );
    }
}
