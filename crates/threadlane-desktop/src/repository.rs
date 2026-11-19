//! Inspect an existing checkout before registering its clone source with a hub.

use std::path::Path;
use std::process::Command;
use std::rc::Rc;

use gpui_kit::*;
use threadlane_client::Reach;
use threadlane_desktop::Activity;

use crate::Desktop;

struct Checkout {
    name: String,
    source: String,
    branch: String,
    hint: String,
}

fn inspect(folder: &Path, local_hub: bool) -> Result<Checkout, String> {
    let git = |args: &[&str]| -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(folder)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .output()
            .map_err(|e| format!("Could not run Git: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
        }
        String::from_utf8(out.stdout)
            .map(|s| s.strip_suffix('\n').unwrap_or(&s).to_owned())
            .map_err(|_| "Git returned a path or setting that is not valid UTF-8.".into())
    };
    let root = git(&["rev-parse", "--show-toplevel"])
        .map_err(|e| format!("Choose a folder in a Git checkout. {e}"))?;
    let name = Path::new(&root)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_owned();
    if git(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).is_err() {
        return Err(
            "This checkout has no commit. Create its first commit before adding it.".into(),
        );
    }
    let remotes = git(&["remote"])?;
    let mut hint = format!("Checkout: {root}.");
    let mut branch = String::new();
    let mut source = String::new();
    let remote = remotes
        .lines()
        .find(|r| *r == "origin")
        .or_else(|| (remotes.lines().count() == 1).then_some(remotes.as_str()));
    if let Some(remote) = remote {
        let url = git(&["remote", "get-url", "--", remote])?;
        let prefix = format!("refs/remotes/{remote}/");
        if let Ok(reference) = git(&["symbolic-ref", "--quiet", &format!("{prefix}HEAD")]) {
            branch = reference.strip_prefix(&prefix).unwrap_or("").to_owned();
        }
        let path_source =
            !url.contains("://") && !url.split('/').next().unwrap_or("").contains(':');
        let http_auth = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .is_some_and(|rest| {
                rest.split('/')
                    .next()
                    .is_some_and(|host| host.contains('@'))
            });
        if http_auth {
            hint.push_str(" Enter a clone URL without embedded credentials.");
        } else if !local_hub && (path_source || url.starts_with("file:")) {
            hint.push_str(
                " The remote is a local path. Enter a clone source accessible to the hub.",
            );
        } else if path_source {
            source = Path::new(&root).join(url).display().to_string();
        } else {
            source = url;
        }
    } else if remotes.is_empty() && local_hub {
        source.clone_from(&root);
        branch = git(&["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default();
        hint.push_str(" No remote is configured. The hub can clone this checkout.");
    } else if remotes.is_empty() {
        hint.push_str(" No remote is configured. Enter a clone source accessible to the hub.");
    } else {
        hint.push_str(" Several remotes are configured. Enter the one the hub should clone.");
    }
    if branch.is_empty() {
        hint.push_str(" Default branch unknown. Enter the branch the hub should use.");
    }
    Ok(Checkout {
        name,
        source,
        branch,
        hint,
    })
}

impl Desktop {
    pub(crate) fn reset_repository_form(&mut self) {
        self.repository_hint = None;
        self.repository_picking = None;
        self.clear_fields.extend([
            self.repository_name.clone(),
            self.repository_remote.clone(),
            self.repository_branch.clone(),
        ]);
    }

    pub(crate) fn choose_repository_folder(
        &mut self,
        space: String,
        project: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            directories: true,
            files: false,
            multiple: false,
            prompt: Some("Choose repository folder".into()),
        });
        let core = self.core.clone();
        let request = Rc::new(());
        self.repository_picking = Some(request.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let paths = selection
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?;
                let Some(folder) = paths.and_then(|p| p.into_iter().next()) else {
                    return Ok(None);
                };
                let local = core
                    .read()
                    .await
                    .hubs()
                    .iter()
                    .any(|h| h.name == space && matches!(h.reach, Reach::Socket { .. }));
                cx.background_spawn(async move { inspect(&folder, local) })
                    .await
                    .map(Some)
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this
                    .repository_picking
                    .as_ref()
                    .is_some_and(|active| Rc::ptr_eq(active, &request))
                {
                    return;
                }
                this.repository_picking = None;
                if this.state.repository_form_open(&space, &project) {
                    match result {
                        Ok(Some(checkout)) => {
                            this.repository_hint = Some(checkout.hint);
                            for (field, value) in [
                                (&this.repository_name, checkout.name),
                                (&this.repository_remote, checkout.source),
                                (&this.repository_branch, checkout.branch),
                            ] {
                                field.update(cx, |f, cx| f.set_value(value, window, cx));
                            }
                        }
                        Err(e) => this.state.report(Activity::Failed(e)),
                        Ok(None) => {}
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, Path, inspect};

    fn git(dir: &Path, args: &[&str]) {
        let result = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn linked_checkout_uses_the_selected_remotes_head() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "trunk"]);
        git(dir.path(), &["commit", "--allow-empty", "-m", "initial"]);
        let remote = "git@example.invalid:team/site.git";
        git(dir.path(), &["remote", "add", "upstream", remote]);
        git(
            dir.path(),
            &["update-ref", "refs/remotes/upstream/trunk", "HEAD"],
        );
        git(
            dir.path(),
            &[
                "symbolic-ref",
                "refs/remotes/upstream/HEAD",
                "refs/remotes/upstream/trunk",
            ],
        );
        git(dir.path(), &["worktree", "add", "-b", "feature", "site"]);
        let nested = dir.path().join("site/src");
        std::fs::create_dir(&nested).unwrap();
        let checkout = inspect(&nested, false).unwrap();
        assert_eq!(
            (
                checkout.name.as_str(),
                checkout.source.as_str(),
                checkout.branch.as_str()
            ),
            ("site", remote, "trunk")
        );
        git(
            dir.path(),
            &["symbolic-ref", "--delete", "refs/remotes/upstream/HEAD"],
        );
        assert!(inspect(&nested, true).unwrap().branch.is_empty());
        git(dir.path(), &["remote", "add", "another", "../elsewhere"]);
        assert!(inspect(&nested, true).unwrap().source.is_empty());
        git(dir.path(), &["remote", "add", "origin", "../preferred"]);
        assert!(inspect(&nested, false).unwrap().source.is_empty());
        assert!(
            inspect(&nested, true)
                .unwrap()
                .source
                .ends_with("/site/../preferred")
        );
        git(
            dir.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "https://user:example@example.invalid/team/site.git",
            ],
        );
        let checkout = inspect(&nested, true).unwrap();
        assert!(checkout.source.is_empty() && checkout.hint.contains("credentials"));
    }

    #[test]
    fn path_fallback_depends_on_the_hub_location() {
        let dir = tempfile::tempdir().unwrap();
        assert!(inspect(dir.path(), true).is_err());
        git(dir.path(), &["init", "-b", "trunk"]);
        assert!(inspect(dir.path(), true).is_err());
        git(dir.path(), &["commit", "--allow-empty", "-m", "initial"]);
        let local = inspect(dir.path(), true).unwrap();
        assert_eq!(Path::new(&local.source), dir.path().canonicalize().unwrap());
        assert_eq!(local.branch, "trunk");
        git(dir.path(), &["checkout", "--detach"]);
        assert!(inspect(dir.path(), true).unwrap().branch.is_empty());
        let network = inspect(dir.path(), false).unwrap();
        assert!(network.source.is_empty() && network.branch.is_empty());
    }
}
