use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;
use threadlane_core::client::{TaskReply, TaskRequest};
use threadlane_core::ids::{EventId, OutboxId, ProjectId};
use threadlane_core::model::{Mode, OutboxMessage, OutboxStatus, Urgency};
use threadlane_core::time::Timestamp;

use super::*;

#[derive(Debug, Default)]
struct FakeHub {
    space: String,
    items: Mutex<Vec<FeedItem>>,
    sent: Mutex<Vec<UserMessage>>,
}

impl FakeHub {
    fn new(space: &str) -> Arc<Self> {
        Arc::new(Self {
            space: space.to_owned(),
            ..Self::default()
        })
    }

    fn push(&self, item: FeedItem) {
        self.items.lock().expect("lock").push(item);
    }
}

#[async_trait]
impl HubApi for FakeHub {
    async fn info(&self) -> Result<HubInfo, ApiError> {
        Ok(HubInfo {
            space: self.space.clone(),
            mode_ceiling: Mode::Assisted,
            schema_version: 1,
        })
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        Ok(vec![ProjectSummary {
            id: ProjectId(1),
            name: "General".to_owned(),
            is_general: true,
            paused: false,
            mode_ceiling: None,
            archived: false,
        }])
    }

    async fn send_message(&self, message: &UserMessage) -> Result<Accepted, ApiError> {
        let mut sent = self.sent.lock().expect("lock");
        sent.push(message.clone());
        Ok(Accepted {
            event_id: EventId(i64::try_from(sent.len()).expect("small")),
            project_id: ProjectId(1),
            duplicate: false,
        })
    }

    async fn feed(&self, after: i64, wait: Duration) -> Result<Vec<FeedItem>, ApiError> {
        let items: Vec<_> = self
            .items
            .lock()
            .expect("lock")
            .iter()
            .filter(|item| item.seq > after)
            .cloned()
            .collect();
        if items.is_empty() {
            tokio::time::sleep(wait).await;
        }
        Ok(items)
    }

    /// Answers every request with an empty agent list.
    async fn org(&self, _request: OrgRequest) -> Result<OrgReply, ApiError> {
        Ok(OrgReply::Agents { agents: Vec::new() })
    }

    /// Nothing waits on you here.
    async fn decisions(&self, _request: DecisionRequest) -> Result<DecisionReply, ApiError> {
        Ok(DecisionReply::Pending {
            decisions: Vec::new(),
        })
    }

    /// Answers every request with an empty client list.
    async fn pairing(&self, _request: PairingRequest) -> Result<PairingReply, ApiError> {
        Ok(PairingReply::Clients {
            clients: Vec::new(),
        })
    }

    /// Reports its delivered items and an empty pacing backlog.
    async fn overview(&self, request: OverviewRequest) -> Result<OverviewReply, ApiError> {
        match request {
            OverviewRequest::HeldResults => Ok(OverviewReply::HeldResults { held: None }),
            _ => Ok(OverviewReply::Recent {
                items: self.items.lock().expect("lock").clone(),
            }),
        }
    }

    /// Answers every request with no files.
    async fn memory(&self, _request: MemoryRequest) -> Result<MemoryReply, ApiError> {
        Ok(MemoryReply::Files { files: Vec::new() })
    }
    /// Refuses every request.
    async fn tasks(&self, request: TaskRequest) -> Result<TaskReply, ApiError> {
        Err(ApiError::Invalid(format!("unsupported {request:?}")))
    }
}

#[derive(Debug)]
struct DownHub;

#[async_trait]
impl HubApi for DownHub {
    async fn info(&self) -> Result<HubInfo, ApiError> {
        Err(ApiError::Unreachable("connection refused".to_owned()))
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        Err(ApiError::Unreachable("connection refused".to_owned()))
    }

    async fn send_message(&self, _message: &UserMessage) -> Result<Accepted, ApiError> {
        Err(ApiError::Unreachable("connection refused".to_owned()))
    }

    async fn feed(&self, _after: i64, _wait: Duration) -> Result<Vec<FeedItem>, ApiError> {
        Err(ApiError::Unreachable("connection refused".to_owned()))
    }

    async fn org(&self, _request: OrgRequest) -> Result<OrgReply, ApiError> {
        Err(ApiError::Unreachable("connection refused".to_owned()))
    }

    async fn decisions(&self, _request: DecisionRequest) -> Result<DecisionReply, ApiError> {
        Err(ApiError::Unreachable("connection refused".to_owned()))
    }

    async fn pairing(&self, _request: PairingRequest) -> Result<PairingReply, ApiError> {
        Err(ApiError::Unreachable("connection refused".to_owned()))
    }

    async fn overview(&self, _request: OverviewRequest) -> Result<OverviewReply, ApiError> {
        Err(ApiError::Unreachable("connection refused".to_owned()))
    }

    async fn memory(&self, _request: MemoryRequest) -> Result<MemoryReply, ApiError> {
        Err(ApiError::Unreachable("connection refused".to_owned()))
    }
    async fn tasks(&self, _request: TaskRequest) -> Result<TaskReply, ApiError> {
        Err(ApiError::Unreachable("connection refused".to_owned()))
    }
}

fn item(seq: i64, kind: OutboxKind, project: Option<&str>, text: &str) -> FeedItem {
    FeedItem {
        digest: Vec::new(),
        seq,
        message: OutboxMessage {
            trace_id: None,
            id: OutboxId(seq),
            project_id: project.map(|_| ProjectId(1)),
            lane_id: None,
            session_id: None,
            run_id: None,
            tool_call_id: None,
            kind,
            decision_id: None,
            channel: "client".to_owned(),
            body: json!({ "text": text }),
            urgency: Urgency::Normal,
            status: OutboxStatus::Sent,
            not_before: None,
            attempts: 1,
            channel_message_id: None,
            created_at: Timestamp::from_millis(1_000),
            sent_at: Some(Timestamp::from_millis(2_000)),
            delivery_seq: Some(seq),
        },
        project_name: project.map(str::to_owned),
        delivered_at: Timestamp::from_millis(2_000),
    }
}

fn message(seq: i64, text: &str) -> FeedItem {
    item(seq, OutboxKind::Message, Some("General"), text)
}

fn notice(seq: i64, text: &str) -> FeedItem {
    item(seq, OutboxKind::SystemNotice, None, text)
}

fn seqs(items: &[SpaceFeedItem]) -> Vec<i64> {
    items.iter().map(|item| item.item.seq).collect()
}

fn client_dir() -> (TempDir, ClientDirs) {
    let dir = tempfile::tempdir().expect("tempdir");
    let dirs = ClientDirs::new(dir.path().join("client"));
    (dir, dirs)
}

#[test]
fn directory_pairing_persists_the_default_and_an_explicit_folder() {
    let (temp, dirs) = client_dir();
    let mut core = ClientCore::load(dirs.clone()).unwrap();
    let default = dirs.space_dir("Work_1").unwrap();
    let custom = temp.path().join("My custom directory");
    fs::create_dir(&custom).unwrap();
    fs::write(custom.join("existing"), "keep").unwrap();
    core.pair_directory("Work_1", None).unwrap();
    core.pair_directory("Personal", Some(custom.clone()))
        .unwrap();
    assert!(default.is_dir());
    assert_eq!(default, dirs.root.join("spaces/Work_1"));
    assert_eq!(fs::read(custom.join("existing")).unwrap(), b"keep");
    let saved = ClientCore::load(dirs.clone()).unwrap();
    for (hub, directory) in saved.hubs().iter().zip([default, custom]) {
        assert_eq!(
            hub.reach,
            Reach::Socket {
                socket: directory.join(SOCKET_FILE)
            }
        );
    }
    assert_eq!(saved.hubs().len(), 2);
    assert!(!dirs.space_dir("Personal").unwrap().exists());
}

#[test]
fn directory_pairing_validates_before_creating_and_preserves_collisions() {
    let (_temp, dirs) = client_dir();
    let mut core = ClientCore::load(dirs.clone()).unwrap();
    for name in ["", ".", "..", "/outside", "../outside", "a/b"] {
        assert!(matches!(
            core.pair_directory(name, None),
            Err(ClientError::InvalidName(_))
        ));
    }
    assert!(matches!(
        core.pair_directory("..", Some(dirs.root.join("custom"))),
        Err(ClientError::InvalidName(_))
    ));
    assert!(!dirs.root.exists());
    let taken = dirs.space_dir("taken").unwrap();
    fs::create_dir_all(&taken).unwrap();
    assert!(matches!(
        core.pair_directory("taken", None),
        Err(ClientError::DirectoryTaken(_))
    ));
    assert!(taken.is_dir());
    core.pair_directory("taken", Some(taken)).unwrap();
    let unused = dirs.root.join("unused");
    assert!(matches!(
        core.pair_directory("taken", Some(unused.clone())),
        Err(ClientError::AlreadyPaired(_))
    ));
    assert!(!unused.exists());
}

#[test]
fn a_failed_directory_pairing_removes_only_its_new_folder() {
    let (temp, dirs) = client_dir();
    let mut core = ClientCore::load(dirs.clone()).unwrap();
    fs::create_dir_all(dirs.hubs_file()).unwrap(); // Force the final rename to fail.
    let existing = temp.path().join("existing");
    fs::create_dir(&existing).unwrap();
    for path in [None, Some(existing.clone())] {
        assert!(core.pair_directory("work", path).is_err());
        assert!(core.hubs().is_empty());
        assert!(!dirs.space_dir("work").unwrap().exists());
        assert!(existing.is_dir());
    }
}

/// A core with `work` paired (to a socket nobody serves) and answered by `hub`.
fn paired(dirs: &ClientDirs, hub: &Arc<FakeHub>) -> ClientCore {
    let mut core = ClientCore::load(dirs.clone()).expect("load");
    if core.hubs().is_empty() {
        core.pair("work", PathBuf::from("/nonexistent/hub.sock"))
            .expect("pair");
    }
    core.with_hub("work", Arc::clone(hub) as Arc<dyn HubApi>)
}

#[tokio::test]
async fn a_single_paired_hub_is_the_default() {
    let (_dir, dirs) = client_dir();
    let hub = FakeHub::new("Work");
    let core = paired(&dirs, &hub);

    let sent = core.send(None, None, "hello").await.expect("send");
    assert_eq!(sent.space, "work");
    assert_eq!(sent.project, None);
    let projects = core.projects(None).await.expect("projects");
    assert_eq!(projects.space, "work");

    let again = core.send(None, Some("api"), "again").await.expect("send");
    assert_ne!(sent.client_message_id, again.client_message_id);
    let messages = hub.sent.lock().expect("lock");
    assert_eq!(messages[0].text, "hello");
    assert_eq!(messages[1].project.as_deref(), Some("api"));
    assert_eq!(messages[0].client_message_id, sent.client_message_id);
}

#[tokio::test]
async fn a_message_to_a_task_names_the_task_and_no_project() {
    let (_dir, dirs) = client_dir();
    let hub = FakeHub::new("Work");
    let core = paired(&dirs, &hub);

    let sent = core
        .send_to_task(None, "T-0007", "add tests first")
        .await
        .expect("send");
    assert_eq!((sent.space.as_str(), sent.project), ("work", None));
    let error = core
        .send_to_task(None, "D-0007", "no")
        .await
        .expect_err("not a task");
    assert!(format!("{error:?}").contains("not a task id"), "{error:?}");
    let messages = hub.sent.lock().expect("lock").clone();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].task, Some(TaskId(7)));
    assert_eq!(messages[0].project, None);
    assert_eq!(messages[0].client_message_id, sent.client_message_id);

    let files = core
        .memory(None, MemoryRequest::List { project: None })
        .await
        .expect("memory");
    assert_eq!(files, MemoryReply::Files { files: Vec::new() });
}

#[tokio::test]
async fn with_several_hubs_an_unnamed_request_fails_listing_them() {
    let (_dir, dirs) = client_dir();
    let core = ClientCore::load(dirs)
        .expect("load")
        .with_hub("work", FakeHub::new("Work"))
        .with_hub("home", FakeHub::new("Home"));

    match core.send(None, None, "hello").await {
        Err(ClientError::AmbiguousHub(names)) => assert_eq!(names, ["work", "home"]),
        other => panic!("expected AmbiguousHub, got {other:?}"),
    }
    let error = core.projects(None).await.expect_err("ambiguous");
    assert!(error.to_string().contains("work, home"), "{error}");

    let named = core.send(Some("home"), None, "hello").await.expect("send");
    assert_eq!(named.space, "home");

    let error = core.projects(Some("play")).await.expect_err("unknown");
    assert!(matches!(error, ClientError::UnknownHub { .. }));
    assert!(error.to_string().contains("paired: work, home"), "{error}");
}

#[tokio::test]
async fn an_organization_reply_of_the_wrong_kind_is_an_error() {
    let (_dir, dirs) = client_dir();
    let hub = FakeHub::new("Work");
    let core = paired(&dirs, &hub);

    assert!(core.agents(None).await.expect("agents").is_empty());
    let error = core
        .create_project(None, "web", "")
        .await
        .expect_err("the fake answers with agents");
    assert!(
        matches!(
            &error,
            ClientError::Api {
                source: ApiError::Internal(reason),
                ..
            } if reason.contains("create project")
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn with_no_hub_paired_a_request_says_so() {
    let (_dir, dirs) = client_dir();
    let core = ClientCore::load(dirs).expect("load");
    assert!(matches!(
        core.projects(None).await,
        Err(ClientError::NoHubs)
    ));
}

#[tokio::test]
async fn the_cursor_advances_and_survives_a_reload() {
    let (_dir, dirs) = client_dir();
    let hub = FakeHub::new("Work");
    hub.push(message(1, "one"));
    hub.push(message(2, "two"));

    let mut core = paired(&dirs, &hub);
    let first = core.poll_feed(None, Duration::ZERO).await.expect("poll");
    assert_eq!(seqs(&first), [1, 2]);
    assert!(
        core.poll_feed(None, Duration::ZERO)
            .await
            .expect("poll")
            .is_empty()
    );
    hub.push(message(3, "three"));
    assert_eq!(
        seqs(&core.poll_feed(None, Duration::ZERO).await.expect("poll")),
        [3]
    );
    core.commit_feed().expect("commit");
    drop(core);

    let mut core = paired(&dirs, &hub);
    assert!(
        core.poll_feed(None, Duration::ZERO)
            .await
            .expect("poll")
            .is_empty()
    );
    hub.push(message(4, "four"));
    assert_eq!(
        seqs(&core.poll_feed(None, Duration::ZERO).await.expect("poll")),
        [4]
    );

    core.rewind_feed(None).expect("rewind");
    assert_eq!(
        seqs(&core.poll_feed(None, Duration::ZERO).await.expect("poll")),
        [1, 2, 3, 4]
    );
}

#[tokio::test]
async fn an_item_is_not_lost_if_the_process_stops_before_the_cursor_is_saved() {
    let (_dir, dirs) = client_dir();
    let hub = FakeHub::new("Work");
    hub.push(message(1, "one"));

    let mut core = paired(&dirs, &hub);
    assert_eq!(
        seqs(&core.poll_feed(None, Duration::ZERO).await.expect("poll")),
        [1]
    );
    // The process stops before the item was shown: no commit, no next poll.
    drop(core);

    let mut core = paired(&dirs, &hub);
    assert_eq!(
        seqs(&core.poll_feed(None, Duration::ZERO).await.expect("poll")),
        [1],
        "the item comes again"
    );
}

#[tokio::test]
async fn pairing_and_unpairing_persist() {
    let (dir, dirs) = client_dir();
    let data_dir = dir.path().join("hub-data");
    fs::create_dir_all(&data_dir).expect("mkdir");

    let mut core = ClientCore::load(dirs.clone()).expect("load");
    core.pair("work", data_dir.clone())
        .expect("pair a directory");
    core.pair("home", PathBuf::from("/srv/home/hub.sock"))
        .expect("pair a socket");
    assert!(matches!(
        core.pair("work", PathBuf::from("/x")),
        Err(ClientError::AlreadyPaired(_))
    ));
    assert!(matches!(
        core.pair("my/space", PathBuf::from("/x")),
        Err(ClientError::InvalidName(_))
    ));
    assert!(matches!(
        core.pair("", PathBuf::from("/x")),
        Err(ClientError::InvalidName(_))
    ));

    let core = ClientCore::load(dirs.clone()).expect("reload");
    assert_eq!(
        core.hubs(),
        [
            PairedHub {
                name: "work".to_owned(),
                reach: Reach::Socket {
                    socket: data_dir.join(SOCKET_FILE),
                },
            },
            PairedHub {
                name: "home".to_owned(),
                reach: Reach::Socket {
                    socket: PathBuf::from("/srv/home/hub.sock"),
                },
            },
        ]
    );

    let hub = FakeHub::new("Work");
    hub.push(message(1, "one"));
    let mut core = core.with_hub("work", Arc::clone(&hub) as Arc<dyn HubApi>);
    core.poll_feed(Some("work"), Duration::ZERO)
        .await
        .expect("poll");
    core.commit_feed().expect("commit");

    core.unpair("work").expect("unpair");
    assert!(matches!(
        core.unpair("work"),
        Err(ClientError::UnknownHub { .. })
    ));
    let mut core = ClientCore::load(dirs.clone()).expect("reload");
    assert_eq!(core.hubs().len(), 1);
    assert_eq!(core.hubs()[0].name, "home");

    // Pairing the name again starts its feed from the beginning.
    core.pair("work", data_dir).expect("pair again");
    let mut core = core.with_hub("work", hub as Arc<dyn HubApi>);
    assert_eq!(
        seqs(
            &core
                .poll_feed(Some("work"), Duration::ZERO)
                .await
                .expect("poll")
        ),
        [1]
    );
}

#[tokio::test]
async fn injected_hubs_are_not_written_to_the_hub_list() {
    let (_dir, dirs) = client_dir();
    let mut core = ClientCore::load(dirs.clone())
        .expect("load")
        .with_hub("test", FakeHub::new("Test"));
    core.pair("work", PathBuf::from("/srv/work/hub.sock"))
        .expect("pair");
    assert_eq!(core.hubs().len(), 2);

    let core = ClientCore::load(dirs).expect("reload");
    assert_eq!(core.hubs().len(), 1);
    assert_eq!(core.hubs()[0].name, "work");
}

#[tokio::test]
async fn status_shows_an_unreachable_hub_next_to_a_reachable_one() {
    let (_dir, dirs) = client_dir();
    let core = ClientCore::load(dirs)
        .expect("load")
        .with_hub("work", FakeHub::new("Work"))
        .with_hub("home", Arc::new(DownHub));

    let statuses = core.status().await;
    assert_eq!(statuses.len(), 2);
    assert_eq!(statuses[0].name, "work");
    assert!(statuses[0].is_reachable());
    assert_eq!(
        statuses[0].info.as_ref().map(|info| info.space.as_str()),
        Ok("Work")
    );
    assert_eq!(statuses[1].name, "home");
    assert_eq!(
        statuses[1].info,
        Err(ApiError::Unreachable("connection refused".to_owned()))
    );

    let error = core.projects(Some("home")).await.expect_err("down");
    assert!(matches!(error, ClientError::Unreachable { .. }));

    // A listing from every hub reports the one that is away in its place.
    let recent = core.recent(None, 10).await.expect("recent");
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].space, "work");
    assert!(recent[0].listed.as_ref().is_ok_and(Vec::is_empty));
    assert!(matches!(
        recent[1].listed,
        Err(ClientError::Unreachable { .. })
    ));
    let wrong = core.running(Some("work")).await.expect("running");
    assert!(
        matches!(wrong[0].listed, Err(ClientError::Api { .. })),
        "an answer of the wrong kind is an error"
    );
    assert_eq!(
        error.to_string(),
        "hub home is unreachable: connection refused"
    );
}

#[tokio::test]
async fn every_feed_item_carries_its_space_name() {
    let (_dir, dirs) = client_dir();
    let work = FakeHub::new("Work");
    work.push(message(1, "from work"));
    let home = FakeHub::new("Home");
    home.push(notice(1, "from home"));
    let mut core = ClientCore::load(dirs)
        .expect("load")
        .with_hub("work", work)
        .with_hub("home", home);

    let items = core
        .poll_feed(Some("work"), Duration::ZERO)
        .await
        .expect("poll");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].space, "work");
    assert_eq!(items[0].label(), "work/General");
    assert_eq!(items[0].text(), "from work");

    let items = core
        .poll_feed(Some("home"), Duration::ZERO)
        .await
        .expect("poll");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].space, "home");
    assert_eq!(items[0].label(), "home");
    assert_eq!(items[0].kind(), OutboxKind::SystemNotice);
}

#[tokio::test]
async fn waiting_for_a_message_stops_at_the_first_one_and_leaves_the_rest_unread() {
    let (_dir, dirs) = client_dir();
    let hub = FakeHub::new("Work");
    hub.push(notice(1, "thinking"));
    hub.push(message(2, "reply"));
    hub.push(message(3, "later"));
    let mut core = paired(&dirs, &hub);

    let mut shown = Vec::new();
    let reply = core
        .wait_for_message(None, Duration::from_secs(5), |item| {
            shown.push(item.item.seq);
        })
        .await
        .expect("a message");
    assert_eq!(reply.item.seq, 2);
    assert_eq!(shown, [1, 2]);

    // Saved as shown: a new process starts after the reply.
    let mut core = paired(&dirs, &hub);
    assert_eq!(
        seqs(&core.poll_feed(None, Duration::ZERO).await.expect("poll")),
        [3]
    );
}

#[tokio::test]
async fn waiting_for_a_message_times_out() {
    let (_dir, dirs) = client_dir();
    let hub = FakeHub::new("Work");
    hub.push(notice(1, "thinking"));
    let mut core = paired(&dirs, &hub);

    let mut shown = Vec::new();
    let error = core
        .wait_for_message(None, Duration::from_millis(200), |item| {
            shown.push(item.item.seq);
        })
        .await
        .expect_err("no message comes");
    assert!(matches!(error, ClientError::Timeout { .. }), "{error:?}");
    assert_eq!(shown, [1]);
}

#[test]
fn the_text_of_an_item_falls_back_from_text_to_question_to_json() {
    let mut feed_item = SpaceFeedItem {
        space: "work".to_owned(),
        item: message(1, "plain"),
    };
    assert_eq!(feed_item.text(), "plain");
    feed_item.item.message.body = json!({ "question": "merge it?" });
    assert_eq!(feed_item.text(), "merge it?");
    feed_item.item.message.body = json!({ "other": 1 });
    assert_eq!(feed_item.text(), r#"{"other":1}"#);
}

#[test]
fn the_client_directory_comes_from_the_environment() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(value))
        }
    };
    let all = env(&[
        ("THREADLANE_CLIENT_DIR", "/a"),
        ("XDG_CONFIG_HOME", "/b"),
        ("HOME", "/c"),
    ]);
    assert_eq!(ClientDirs::resolve(all).expect("dir").root, Path::new("/a"));

    let xdg = env(&[
        ("THREADLANE_CLIENT_DIR", ""),
        ("XDG_CONFIG_HOME", "/b"),
        ("HOME", "/c"),
    ]);
    assert_eq!(
        ClientDirs::resolve(xdg).expect("dir").root,
        Path::new("/b/threadlane")
    );

    let home = env(&[("XDG_CONFIG_HOME", "relative"), ("HOME", "/c")]);
    assert_eq!(
        ClientDirs::resolve(home).expect("dir").root,
        Path::new("/c/.config/threadlane")
    );

    assert!(matches!(
        ClientDirs::resolve(env(&[])),
        Err(ClientError::NoClientDir)
    ));
}

#[tokio::test]
async fn an_iroh_pairing_survives_a_reload_beside_a_socket_one() {
    let (_dir, dirs) = client_dir();
    fs::create_dir_all(&dirs.root).expect("mkdir");
    let id = threadlane_transport_iroh::SecretKey::generate().public();
    fs::write(
        dirs.hubs_file(),
        format!(
            "[[hub]]\nname = \"work\"\nsocket = \"/srv/work/hub.sock\"\n\n\
             [[hub]]\nname = \"home\"\nhub = \"{id}\"\naddrs = [\"127.0.0.1:4433\"]\n"
        ),
    )
    .expect("write");
    let iroh = Reach::Iroh {
        hub: id,
        addrs: vec!["127.0.0.1:4433".parse().expect("addr")],
    };

    let mut core = ClientCore::load(dirs.clone()).expect("load");
    assert_eq!(core.hubs()[1].reach, iroh);
    assert_eq!(
        core.hubs()[1].reach.to_string(),
        format!("iroh {}", id.fmt_short())
    );
    core.pair("play", PathBuf::from("/srv/play/hub.sock"))
        .expect("pair");
    let core = ClientCore::load(dirs).expect("reload");
    assert_eq!(core.hubs().len(), 3);
    assert_eq!(core.hubs()[1].reach, iroh, "written back as it was read");

    // No key for it in this client directory: a clear error, not a panic.
    let error = core.projects(Some("home")).await.expect_err("no key");
    assert!(matches!(error, ClientError::Read { .. }), "{error:?}");
}

#[tokio::test]
async fn following_every_hub_merges_their_feeds_labeled_with_their_spaces() {
    let (_dir, dirs) = client_dir();
    let work = FakeHub::new("Work");
    work.push(message(1, "from work"));
    let home = FakeHub::new("Home");
    home.push(notice(1, "from home"));
    home.push(notice(2, "again from home"));
    let mut core = ClientCore::load(dirs.clone())
        .expect("load")
        .with_hub("work", work)
        .with_hub("home", Arc::clone(&home) as Arc<dyn HubApi>);

    let mut merged = core.follow_all().expect("follow");
    let mut seen = Vec::new();
    while seen.len() < 3 {
        let item = merged.recv().await.expect("open").expect("an item");
        seen.push(format!("{}: {}", item.label(), item.text()));
        core.acknowledge(&item).expect("acknowledge");
    }
    seen.sort();
    assert_eq!(
        seen,
        [
            "home: again from home",
            "home: from home",
            "work/General: from work"
        ]
    );
    drop(merged);

    // Acknowledged items are saved at once.
    let mut core = ClientCore::load(dirs)
        .expect("reload")
        .with_hub("home", home as Arc<dyn HubApi>);
    assert!(
        core.poll_feed(Some("home"), Duration::ZERO)
            .await
            .expect("poll")
            .is_empty()
    );
}

#[test]
fn a_corrupt_hub_list_is_reported() {
    let (_dir, dirs) = client_dir();
    fs::create_dir_all(&dirs.root).expect("mkdir");
    fs::write(dirs.hubs_file(), "this is = not [ toml").expect("write");
    assert!(matches!(
        ClientCore::load(dirs),
        Err(ClientError::Corrupt { .. })
    ));
}
