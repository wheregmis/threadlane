use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PrField {
    Title,
    Description,
}

pub(super) fn generation_prompt(field: PrField, diff: &str) -> String {
    let request = match field {
        PrField::Title => "Return only a concise pull request title (maximum 72 characters).",
        PrField::Description => "Return a concise Markdown pull request description covering the change and tests. Do not include a title. Do not claim tests passed without evidence.",
    };
    let diff: String = diff.chars().take(24_000).collect();
    format!("{request}\n\nPR base-to-HEAD diff:\n{diff}")
}

pub(super) async fn current_diff(
    client: &std::sync::Arc<dyn threadlane_client::DaemonClient>,
    work_dir: &Path,
    base: &str,
) -> Result<String, String> {
    threadlane_ui_state::project_io::draft_pr_diff(client, work_dir, base.to_string())
        .await
        .map(|diff| diff.chars().take(24_000).collect())
}
