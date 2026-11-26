//! Filing an issue in a repository outside the project (#779), through the
//! real gate and runner with a recording forge.
use serde_json::{Value as Json, json};
use threadlane_core::client::DecisionRequest;
use threadlane_core::forge::{FILE_UPSTREAM_ISSUE, RepositoryAction};
use threadlane_core::model::{DecisionKind, Mode};
use threadlane_testkit::fakes::ForgeCall;
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{Sim, World};
use crate::invariants::assert_invariants;
use crate::permissions::engineer_in;
use crate::publish::publish_world;

fn filing() -> Json {
    json!({
        "upstream": "tokio-rs/tokio",
        "title": "read_buf panics on an empty buffer",
        "body": "Calling read_buf with an empty buffer panics since 1.2.3.\n\nSteps: ...",
    })
}

async fn world(args: Json, mode: Mode) -> World {
    let w = publish_world(
        World::new().await,
        Script::new()
            .call("upstream", FILE_UPSTREAM_ISSUE, args)
            .finish("Done."),
    )
    .await;
    engineer_in(&w, mode).await;
    w
}

async fn work(sim: &Sim) {
    sim.work_until_idle().await.unwrap();
    sim.dispatch().await.unwrap();
}

#[tokio::test]
async fn the_user_approves_each_filing_in_every_mode() {
    for mode in [Mode::OnRequest, Mode::Assisted, Mode::Autonomous] {
        let w = world(filing(), mode).await;
        // A reviewer's answer is never asked for: the user decides.
        w.backend
            .push_answer(Ok(json!({"verdict":"allow","reason":"Fine."})));
        let sim = w.boot(CrashPlan::new()).await;
        sim.send("m", "Finish the page").await;
        work(&sim).await;

        let runs = w.backend.received();
        assert!(
            runs.iter()
                .any(|run| run.tools.iter().any(|tool| tool == FILE_UPSTREAM_ISSUE))
        );
        let decisions = sim.decisions().await;
        assert_eq!(decisions.len(), 1, "{mode}");
        let decision = &decisions[0];
        assert_eq!(decision.kind, DecisionKind::Approval);
        assert_eq!(decision.tool, FILE_UPSTREAM_ISSUE);
        assert!(!decision.approved(), "{mode}");
        assert!(!decision.offers_standing_rule, "{mode}");
        assert!(
            decision
                .question
                .contains("“read_buf panics on an empty buffer” in tokio-rs/tokio"),
            "{}",
            decision.question
        );
        // Visibility is not checked, so the card does not claim it (#780 review).
        assert!(
            decision
                .question
                .contains("its visibility is not checked: treat it as public"),
            "{}",
            decision.question
        );
        assert!(!decision.question.contains("It is public"));
        assert!(decision.context.contains("Calling read_buf"));
        assert!(w.backend.questions().is_empty(), "{mode}");
        assert!(w.forge.calls().is_empty(), "{mode}");

        sim.api
            .decisions(DecisionRequest::Answer {
                decision: decision.id,
                option: "approve".into(),
                note: None,
            })
            .await
            .unwrap();
        work(&sim).await;
        let calls = w.forge.calls();
        let [
            ForgeCall::Apply {
                remote,
                action: RepositoryAction::CreateIssue { title, body },
            },
        ] = calls.as_slice()
        else {
            panic!("expected one upstream issue, got {calls:?}");
        };
        assert_eq!(remote, "https://github.com/tokio-rs/tokio");
        assert_eq!(title, "read_buf panics on an empty buffer");
        assert!(body.starts_with("Calling read_buf"));
        assert_invariants(&sim, FILE_UPSTREAM_ISSUE).await;
    }
}

#[tokio::test]
async fn private_details_and_registered_repositories_are_refused_before_any_ask() {
    let mut local_path = filing();
    local_path["body"] = json!("It fails in /Users/me/work/app/src/main.rs");
    let mut task_id = filing();
    task_id["title"] = json!("Regression found in T-0650");
    let mut repository_name = filing();
    repository_name["body"] = json!("Breaks the app build.");
    let mut secret = filing();
    secret["body"] = json!("token ghp_abcdefghijklmnopqrstuvwxyz0123");
    secret["include_project_details"] = json!(true);
    let mut registered = filing();
    registered["upstream"] = json!("acme/site");
    for args in [local_path, task_id, repository_name, secret, registered] {
        let w = world(args.clone(), Mode::Autonomous).await;
        w.add_github_repository("site", "https://github.com/acme/site.git")
            .await;
        let sim = w.boot(CrashPlan::new()).await;
        sim.send("m", "Finish the page").await;
        work(&sim).await;
        assert!(sim.decisions().await.is_empty(), "{args}");
        assert!(w.forge.calls().is_empty(), "{args}");
    }
}

#[tokio::test]
async fn shared_project_details_are_named_on_the_card() {
    let mut args = filing();
    args["body"] = json!("Breaks the app build at /Users/me/app.");
    args["include_project_details"] = json!(true);
    let w = world(args, Mode::OnRequest).await;
    let sim = w.boot(CrashPlan::new()).await;
    sim.send("m", "Finish the page").await;
    work(&sim).await;
    let decision = sim.decisions().await.remove(0);
    assert!(
        decision
            .context
            .contains("include project details: a local path, the registered repository \"app\""),
        "{}",
        decision.context
    );
}
