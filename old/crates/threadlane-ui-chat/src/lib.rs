mod composer;
mod context_meter;
mod image_preview;
mod markdown;
mod model_picker;
mod tool_detail;
mod tool_preview;
mod trajectory_view;
mod transcript;
mod view;

pub use trajectory_view::TrajectoryView;
pub use view::{init, CentralTab, ChatListView, ConversationFindHandoff};

// Owned by the surface that offers it; the workspace handles panel navigation.
gpui::actions!(
    threadlane_chat,
    [
        OpenWorkspaceReview,
        OpenWorkspaceBranches,
        OpenWorkspaceCommit,
        PullWorkspaceBranch,
        PushWorkspaceBranch,
        CreateWorkspacePullRequest,
        CreateWorkspaceBranch,
        OpenWorkspaceFiles,
        OpenWorkspaceAgents,
        OpenWorkspaceTrajectory
    ]
);
