//! Consequence and worktree-choice presentation. Hosts perform the requested operation.
use gpui::{prelude::*, *};
use gpui_component::button::ButtonVariant;
use gpui_component::checkbox::Checkbox;
use gpui_component::dialog::{AlertDialog, DialogButtonProps};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarSessionRemoval {
    Archive,
    Remove,
}
impl SidebarSessionRemoval {
    fn verb(self) -> &'static str {
        match self {
            Self::Archive => "Archive",
            Self::Remove => "Remove",
        }
    }
    fn prefix(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Remove => "remove",
        }
    }
}

#[derive(Clone)]
pub struct SidebarSessionRemovalTarget {
    id: String,
    title: String,
    project: String,
    is_worktree: bool,
    branch: Option<String>,
}
impl SidebarSessionRemovalTarget {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        project: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            project: project.into(),
            is_worktree: false,
            branch: None,
        }
    }
    pub fn worktree(mut self, branch: Option<String>) -> Self {
        self.is_worktree = true;
        self.branch = branch;
        self
    }
    pub fn description(&self, kind: SidebarSessionRemoval, delete_worktree: bool) -> String {
        let base = match kind {
            SidebarSessionRemoval::Archive => format!("In {}, this session will leave the active list. Its transcript stays in the archive.", self.project),
            SidebarSessionRemoval::Remove => format!("In {}, this session will be permanently deleted, transcript included.", self.project),
        };
        if !self.is_worktree {
            return base;
        }
        let branch = self
            .branch
            .as_deref()
            .map(|branch| format!(" on branch '{branch}'"))
            .unwrap_or_default();
        let note = if delete_worktree {
            format!("Its worktree{branch} will be deleted too. Uncheck below to keep it.")
        } else {
            format!("Its worktree{branch} will be kept.")
        };
        format!("{base}\n{note}")
    }
}

/// Controlled confirmation surface. The host owns the checkbox value, on-ok
/// callback, scope guards and file operations; rendering never deletes data.
pub fn sidebar_session_removal_dialog(
    alert: AlertDialog,
    kind: SidebarSessionRemoval,
    target: &SidebarSessionRemovalTarget,
    delete_worktree: bool,
    on_delete_worktree: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> AlertDialog {
    let props = DialogButtonProps::default()
        .ok_text(kind.verb())
        .show_cancel(true);
    let props = if kind == SidebarSessionRemoval::Remove {
        props.ok_variant(ButtonVariant::Danger)
    } else {
        props
    };
    alert
        .title(format!("{} “{}”?", kind.verb(), target.title))
        .description(target.description(kind, delete_worktree))
        .button_props(props)
        .when(target.is_worktree, |alert| {
            let id = format!("{}-delete-worktree-{}", kind.prefix(), target.id);
            let selector = id.clone();
            let label = target
                .branch
                .as_deref()
                .map(|branch| format!("Delete associated worktree ({branch})"))
                .unwrap_or_else(|| "Delete associated worktree".into());
            alert.child(
                div().pt_2().child(
                    Checkbox::new(SharedString::from(id))
                        .debug_selector(move || selector.clone())
                        .checked(delete_worktree)
                        .label(label)
                        .on_click(move |checked, window, cx| {
                            on_delete_worktree(*checked, window, cx)
                        }),
                ),
            )
        })
}
