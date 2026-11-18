//! Session routing and the native-session rule (Plan.md §4.4, §8.6).

use std::sync::Arc;
use std::time::Duration;

use threadlane_core::backend::NativeSession;
use threadlane_core::model::SessionStatus;
use threadlane_core::store::SessionStore;
use threadlane_hub::HubSettings;
use threadlane_testkit::CrashPlan;

use crate::common::{Reply, TestHub, play};

const WITHIN_WINDOW: Duration = Duration::from_secs(10 * 60);
const PAST_WINDOW: Duration = Duration::from_secs(31 * 60);

fn native_id(session: &NativeSession) -> &str {
    match session {
        NativeSession::Start { id } | NativeSession::Resume { id } => id,
    }
}

#[tokio::test]
async fn a_message_within_the_idle_window_resumes_the_session() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "first").await;
    let first = play(&*hub.link, Reply::Silent).await.unwrap();
    let first = first.only_run().clone();
    let NativeSession::Start { id: native } = &first.native_session else {
        panic!("a first run starts its session");
    };
    uuid_v4(native);
    let session = hub.db.session(first.session_id).await.unwrap().unwrap();
    assert_eq!(session.native_session_id.as_deref(), Some(native.as_str()));

    hub.clock.advance(WITHIN_WINDOW);
    hub.send("m-2", "second").await;
    let second = play(&*hub.link, Reply::Silent).await.unwrap();
    let second = second.only_run();
    assert_eq!(second.session_id, first.session_id);
    assert_eq!(
        second.native_session,
        NativeSession::Resume { id: native.clone() }
    );
}

#[tokio::test]
async fn after_the_idle_window_a_new_session_starts() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "first").await;
    let first = play(&*hub.link, Reply::Silent).await.unwrap();
    let first = first.only_run().clone();

    hub.clock.advance(PAST_WINDOW);
    hub.send("m-2", "second").await;
    let second = play(&*hub.link, Reply::Silent).await.unwrap();
    let second = second.only_run();
    assert_ne!(second.session_id, first.session_id);
    assert!(matches!(second.native_session, NativeSession::Start { .. }));
    assert_ne!(
        native_id(&second.native_session),
        native_id(&first.native_session)
    );
}

#[tokio::test]
async fn a_session_reference_to_an_open_session_wins() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "first").await;
    let first = play(&*hub.link, Reply::Silent).await.unwrap();
    let first = first.only_run().clone();
    hub.clock.advance(PAST_WINDOW);
    hub.send("m-2", "second").await;
    let second = play(&*hub.link, Reply::Silent).await.unwrap();
    let second = second.only_run().clone();
    assert_ne!(
        second.session_id, first.session_id,
        "the latest is another one"
    );

    hub.insert(|event| {
        event.source_event_id = "follow-up".into();
        event.session_ref = Some(first.session_id);
    })
    .await;
    let third = play(&*hub.link, Reply::Silent).await.unwrap();
    let third = third.only_run();
    assert_eq!(third.session_id, first.session_id);
    assert_eq!(
        third.native_session,
        NativeSession::Resume {
            id: native_id(&first.native_session).to_owned()
        }
    );
}

#[tokio::test]
async fn a_session_last_used_on_another_machine_is_closed_and_replaced() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "first").await;
    let first = play(&*hub.link, Reply::Silent).await.unwrap();
    let first = first.only_run().clone();

    let (laptop, laptop_link) = hub.other_machine("laptop").await;
    hub.send("m-2", "second").await;
    let second = play(&*laptop_link, Reply::Silent).await.unwrap();
    assert_eq!(second.lease.unwrap().machine_id, laptop);
    let second = second.only_run();
    assert_ne!(second.session_id, first.session_id);
    assert!(matches!(second.native_session, NativeSession::Start { .. }));
    let old = hub.db.session(first.session_id).await.unwrap().unwrap();
    assert_eq!(old.status, SessionStatus::Closed);
    let new = hub.db.session(second.session_id).await.unwrap().unwrap();
    assert_eq!(new.machine_id, Some(laptop));
}

#[tokio::test]
async fn a_session_past_the_age_limit_is_closed_and_replaced_however_recently_used() {
    let hour = Duration::from_secs(60 * 60);
    let settings = HubSettings {
        idle_window: 48 * hour,
        ..HubSettings::default()
    };
    assert_eq!(settings.session_max_age, 24 * hour, "the default age limit");
    let hub = TestHub::with(settings, Arc::new(CrashPlan::new())).await;
    hub.hello().await;
    hub.send("m-1", "first").await;
    let first = play(&*hub.link, Reply::Silent).await.unwrap();
    let first = first.only_run().clone();

    // Within the age limit: resumed.
    hub.clock.advance(23 * hour);
    hub.send("m-2", "second").await;
    let second = play(&*hub.link, Reply::Silent).await.unwrap();
    let second = second.only_run().clone();
    assert_eq!(second.session_id, first.session_id);
    assert!(matches!(
        second.native_session,
        NativeSession::Resume { .. }
    ));

    // Used an hour ago, but older than a day: a fresh session.
    hub.clock.advance(hour + Duration::from_secs(1));
    hub.send("m-3", "third").await;
    let third = play(&*hub.link, Reply::Silent).await.unwrap();
    let third = third.only_run();
    assert_ne!(third.session_id, first.session_id);
    assert!(matches!(third.native_session, NativeSession::Start { .. }));
    let old = hub.db.session(first.session_id).await.unwrap().unwrap();
    assert_eq!(old.status, SessionStatus::Closed);
}

fn uuid_v4(id: &str) {
    let parsed = uuid::Uuid::parse_str(id).expect("a UUID");
    assert_eq!(parsed.get_version_num(), 4);
}
