//! The client core (Plan.md §11.3, §18.2): connections to paired hubs and the
//! merging of what they report, with no user interface in it.
//!
//! [`ClientCore`] keeps the list of paired hubs in `hubs.toml` and the feed
//! position per hub in `state.json`, both in the client directory. Every item
//! it hands out is labeled with its space, which is the name the hub was
//! paired under. See `docs/milestone-1.md` §7.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File};
use std::io::{self, Write as _};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use threadlane_core::client::{
    Accepted, ApiError, BindingSummary, DecisionReply, DecisionRequest, FeedItem, HubApi, HubInfo,
    OrgReply, OrgRequest, ProjectSummary, UserMessage,
};
use threadlane_core::ids::DecisionId;
use threadlane_core::model::{Agent, Decision, Mode, OutboxKind, Repository};
use threadlane_transport_local::{LocalClient, SOCKET_FILE};
use tokio::time::Instant;

/// The file in the client directory that lists the paired hubs.
pub const HUBS_FILE: &str = "hubs.toml";

/// The file in the client directory that holds the feed position per hub.
pub const STATE_FILE: &str = "state.json";

/// How long [`ClientCore::status`] waits for one hub before calling it
/// unreachable.
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest single feed wait while following, kept below the server's
/// 60 s cap so the request is never cut short by it.
const LONGEST_POLL: Duration = Duration::from_secs(55);

/// Where the client keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientDirs {
    /// The client directory holding [`HUBS_FILE`] and [`STATE_FILE`].
    pub root: PathBuf,
}

impl ClientDirs {
    /// A client directory at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `$THREADLANE_CLIENT_DIR`, else `$XDG_CONFIG_HOME/threadlane`, else
    /// `$HOME/.config/threadlane`. Empty variables count as unset, and a
    /// relative `XDG_CONFIG_HOME` is ignored as the XDG specification asks.
    ///
    /// # Errors
    ///
    /// [`ClientError::NoClientDir`] if none of the three is set.
    pub fn from_env() -> Result<Self, ClientError> {
        Self::resolve(|key| std::env::var_os(key))
    }

    fn resolve(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, ClientError> {
        let set = |key: &str| {
            var(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        if let Some(dir) = set("THREADLANE_CLIENT_DIR") {
            return Ok(Self::new(dir));
        }
        if let Some(config) = set("XDG_CONFIG_HOME").filter(|path| path.is_absolute()) {
            return Ok(Self::new(config.join("threadlane")));
        }
        if let Some(home) = set("HOME") {
            return Ok(Self::new(home.join(".config").join("threadlane")));
        }
        Err(ClientError::NoClientDir)
    }

    /// The path of [`HUBS_FILE`].
    #[must_use]
    pub fn hubs_file(&self) -> PathBuf {
        self.root.join(HUBS_FILE)
    }

    /// The path of [`STATE_FILE`].
    #[must_use]
    pub fn state_file(&self) -> PathBuf {
        self.root.join(STATE_FILE)
    }
}

/// A hub this client is paired with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedHub {
    /// The name the hub was paired under; it labels everything from this hub.
    pub name: String,
    /// The hub's socket.
    pub socket: PathBuf,
}

/// One hub as [`ClientCore::status`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubStatus {
    /// The name the hub was paired under.
    pub name: String,
    /// The hub's socket. Empty for a hub injected with [`ClientCore::with_hub`].
    pub socket: PathBuf,
    /// What the hub says about itself, or why it could not be asked.
    pub info: Result<HubInfo, ApiError>,
}

impl HubStatus {
    /// Whether the hub answered.
    #[must_use]
    pub fn is_reachable(&self) -> bool {
        self.info.is_ok()
    }
}

/// A message the hub accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    /// The space it went to.
    pub space: String,
    /// The project named when sending; `None` means the General project.
    pub project: Option<String>,
    /// The id the client chose; sending it again is a no-op on the hub.
    pub client_message_id: String,
    /// The hub's acknowledgement.
    pub accepted: Accepted,
}

/// The decisions waiting on you in one space (Plan.md §11.3: their ids are
/// per space).
#[derive(Debug)]
pub struct SpaceDecisions {
    /// The space they belong to.
    pub space: String,
    /// Oldest first, or why the hub could not list them.
    pub decisions: Result<Vec<Decision>, ClientError>,
}

/// A decision you answered.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceAnswer {
    /// The space it belongs to.
    pub space: String,
    /// The decision as it now stands, with the answer that counts.
    pub decision: Decision,
    /// `false` if it had been answered before and this answer was ignored.
    pub recorded: bool,
}

/// The projects of one space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceProjects {
    /// The space they belong to.
    pub space: String,
    /// The projects, as the hub lists them.
    pub projects: Vec<ProjectSummary>,
}

/// A feed item labeled with the space it came from (Plan.md §11.3).
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceFeedItem {
    /// The name the hub was paired under.
    pub space: String,
    /// The item as the hub delivered it.
    pub item: FeedItem,
}

impl SpaceFeedItem {
    /// Message, decision, or system notice.
    #[must_use]
    pub fn kind(&self) -> OutboxKind {
        self.item.message.kind
    }

    /// `<space>/<project>`, or just `<space>` for an item without a project.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.item.project_name {
            Some(project) => format!("{}/{project}", self.space),
            None => self.space.clone(),
        }
    }

    /// The readable text of the body: its `text` field, else its `question`
    /// field (decision cards), else the body itself as JSON.
    #[must_use]
    pub fn text(&self) -> String {
        let body = &self.item.message.body;
        ["text", "question"]
            .iter()
            .find_map(|field| body.get(field).and_then(Json::as_str))
            .or_else(|| body.as_str())
            .map_or_else(|| body.to_string(), str::to_owned)
    }
}

/// Why a client operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// None of the variables that locate the client directory is set.
    #[error("cannot locate the client directory: set THREADLANE_CLIENT_DIR or HOME")]
    NoClientDir,
    /// No hub is paired yet.
    #[error("no hub is paired; pair one with `threadlane hub add <name> <data-dir>`")]
    NoHubs,
    /// Several hubs are paired and the request did not name one.
    #[error("several hubs are paired ({}); choose one with --space", .0.join(", "))]
    AmbiguousHub(Vec<String>),
    /// The named hub is not paired.
    #[error("no hub named {name:?} is paired ({})", paired_list(.paired))]
    UnknownHub {
        /// The name asked for.
        name: String,
        /// The names that are paired.
        paired: Vec<String>,
    },
    /// A hub is already paired under this name.
    #[error("a hub named {0:?} is already paired")]
    AlreadyPaired(String),
    /// The name cannot be used for a hub.
    #[error("invalid hub name {0:?}: use 1 to 64 letters, digits, '-', '_' or '.'")]
    InvalidName(String),
    /// The hub could not be reached.
    #[error("hub {space} is unreachable: {reason}")]
    Unreachable {
        /// The hub's name.
        space: String,
        /// What went wrong.
        reason: String,
    },
    /// The hub answered with an error.
    #[error("hub {space} refused the request")]
    Api {
        /// The hub's name.
        space: String,
        /// The hub's error.
        #[source]
        source: ApiError,
    },
    /// No message arrived in time.
    #[error("no message from {space} within {} s", timeout.as_secs())]
    Timeout {
        /// The hub's name.
        space: String,
        /// How long the client waited.
        timeout: Duration,
    },
    /// A client file could not be read.
    #[error("cannot read {}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// A client file could not be written.
    #[error("cannot write {}", path.display())]
    Write {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// A client file exists but does not hold what it should.
    #[error("{} is not valid: {message}", path.display())]
    Corrupt {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        message: String,
    },
}

fn paired_list(paired: &[String]) -> String {
    if paired.is_empty() {
        "no hub is paired".to_owned()
    } else {
        format!("paired: {}", paired.join(", "))
    }
}

/// The error for an organization reply of the wrong kind.
fn wrong_reply(space: &str, request: &str) -> ClientError {
    api_error(
        space,
        ApiError::Internal(format!(
            "the hub answered a {request} request with a different kind of answer"
        )),
    )
}

fn api_error(space: &str, error: ApiError) -> ClientError {
    match error {
        ApiError::Unreachable(reason) => ClientError::Unreachable {
            space: space.to_owned(),
            reason,
        },
        source => ClientError::Api {
            space: space.to_owned(),
            source,
        },
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HubsFile {
    #[serde(default, rename = "hub")]
    hubs: Vec<PairedHub>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    /// The `seq` of the last feed item shown, per hub name.
    #[serde(default)]
    cursors: BTreeMap<String, i64>,
}

/// The paired hubs, one connection to each, and the feed position per hub.
///
/// Feed positions are saved only after the caller has had the items: by the
/// next [`ClientCore::poll_feed`], by [`ClientCore::commit_feed`], or as each
/// item is shown by [`ClientCore::wait_for_message`]. A process that stops in
/// between shows an item twice rather than never.
#[derive(Debug)]
pub struct ClientCore {
    dirs: ClientDirs,
    hubs: Vec<PairedHub>,
    /// APIs injected with [`ClientCore::with_hub`]; other hubs are reached
    /// through their socket.
    injected: HashMap<String, Arc<dyn HubApi>>,
    /// Hubs that exist only because they were injected; never written out.
    transient: BTreeSet<String>,
    /// The cursors as last read from or written to [`STATE_FILE`].
    saved: BTreeMap<String, i64>,
    /// Where the next poll starts, where it differs from `saved`.
    position: BTreeMap<String, i64>,
    /// Hubs whose position moved past what is saved.
    unsaved: BTreeSet<String>,
}

impl ClientCore {
    /// Reads `hubs.toml` and `state.json` from the client directory. Missing
    /// files mean nothing is paired yet.
    ///
    /// # Errors
    ///
    /// [`ClientError::Read`] or [`ClientError::Corrupt`] if a file exists but
    /// cannot be read or parsed.
    pub fn load(dirs: ClientDirs) -> Result<Self, ClientError> {
        let hubs_path = dirs.hubs_file();
        let hubs = read_file(&hubs_path, |text| {
            toml::from_str::<HubsFile>(text).map_err(|e| e.to_string())
        })?
        .unwrap_or_default()
        .hubs;
        let mut names = BTreeSet::new();
        for hub in &hubs {
            if !names.insert(hub.name.as_str()) {
                return Err(ClientError::Corrupt {
                    path: hubs_path,
                    message: format!("hub {:?} is listed twice", hub.name),
                });
            }
        }
        let state = read_state(&dirs.state_file())?;
        Ok(Self {
            dirs,
            hubs,
            injected: HashMap::new(),
            transient: BTreeSet::new(),
            saved: state.cursors,
            position: BTreeMap::new(),
            unsaved: BTreeSet::new(),
        })
    }

    /// Uses `api` for the hub `name` instead of its socket. If no hub of that
    /// name is paired, one is added for this process only. For tests.
    #[must_use]
    pub fn with_hub(mut self, name: &str, api: Arc<dyn HubApi>) -> Self {
        if !self.hubs.iter().any(|hub| hub.name == name) {
            self.hubs.push(PairedHub {
                name: name.to_owned(),
                socket: PathBuf::new(),
            });
            self.transient.insert(name.to_owned());
        }
        self.injected.insert(name.to_owned(), api);
        self
    }

    /// Pairs the hub listening on `socket` under `name` and saves the list.
    /// A directory means the hub's data directory: its `hub.sock` is used.
    /// A relative path is made absolute. The hub need not be running.
    ///
    /// # Errors
    ///
    /// [`ClientError::InvalidName`], [`ClientError::AlreadyPaired`], or an
    /// error writing the list.
    pub fn pair(&mut self, name: &str, socket: PathBuf) -> Result<(), ClientError> {
        validate_name(name)?;
        if self.hubs.iter().any(|hub| hub.name == name) {
            return Err(ClientError::AlreadyPaired(name.to_owned()));
        }
        let socket = std::path::absolute(&socket).map_err(|source| ClientError::Read {
            path: socket.clone(),
            source,
        })?;
        let socket = if socket.is_dir() {
            socket.join(SOCKET_FILE)
        } else {
            socket
        };
        let mut hubs = self.hubs.clone();
        hubs.push(PairedHub {
            name: name.to_owned(),
            socket,
        });
        self.save_hubs(&hubs)?;
        self.hubs = hubs;

        // A new pairing starts at the beginning of that hub's feed.
        self.position.remove(name);
        self.unsaved.remove(name);
        if self.saved.contains_key(name) {
            self.write_cursors(BTreeMap::from([(name.to_owned(), None)]))?;
        }
        Ok(())
    }

    /// Forgets the hub `name` and its feed position.
    ///
    /// # Errors
    ///
    /// [`ClientError::UnknownHub`], or an error writing the files.
    pub fn unpair(&mut self, name: &str) -> Result<(), ClientError> {
        let Some(index) = self.hubs.iter().position(|hub| hub.name == name) else {
            return Err(self.unknown(name));
        };
        let mut hubs = self.hubs.clone();
        hubs.remove(index);
        self.save_hubs(&hubs)?;
        self.hubs = hubs;
        self.injected.remove(name);
        self.transient.remove(name);
        self.position.remove(name);
        self.unsaved.remove(name);
        if self.saved.contains_key(name) {
            self.write_cursors(BTreeMap::from([(name.to_owned(), None)]))?;
        }
        Ok(())
    }

    /// The paired hubs, in the order they were paired.
    #[must_use]
    pub fn hubs(&self) -> &[PairedHub] {
        &self.hubs
    }

    /// Asks every paired hub about itself, all at once. A hub that does not
    /// answer is reported as unreachable; the others are unaffected.
    pub async fn status(&self) -> Vec<HubStatus> {
        let checks: Vec<_> =
            self.hubs
                .iter()
                .map(|hub| {
                    let api = self.api_for(hub);
                    let check = tokio::spawn(async move {
                        tokio::time::timeout(STATUS_TIMEOUT, api.info()).await
                    });
                    (hub.clone(), check)
                })
                .collect();
        let mut statuses = Vec::with_capacity(checks.len());
        for (hub, check) in checks {
            let info = match check.await {
                Ok(Ok(info)) => info,
                Ok(Err(_elapsed)) => Err(ApiError::Unreachable(format!(
                    "no answer within {} s",
                    STATUS_TIMEOUT.as_secs()
                ))),
                Err(error) => Err(ApiError::Internal(format!("status check failed: {error}"))),
            };
            statuses.push(HubStatus {
                name: hub.name,
                socket: hub.socket,
                info,
            });
        }
        statuses
    }

    /// The projects of one space. `None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn projects(&self, space: Option<&str>) -> Result<SpaceProjects, ClientError> {
        let (space, api) = self.target(space)?;
        let projects = api.projects().await.map_err(|e| api_error(&space, e))?;
        Ok(SpaceProjects { space, projects })
    }

    /// Creates a project in one space. `None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn create_project(
        &self,
        space: Option<&str>,
        name: &str,
        description: &str,
    ) -> Result<ProjectSummary, ClientError> {
        let request = OrgRequest::CreateProject {
            name: name.to_owned(),
            description: description.to_owned(),
        };
        match self.org(space, request).await? {
            (_, OrgReply::Project { project }) => Ok(project),
            (space, _) => Err(wrong_reply(&space, "create project")),
        }
    }

    /// Registers a repository in a project. `space: None` means the only
    /// paired hub; `project: None` means the General project, and
    /// `default_branch: None` the branch `main`.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error, for
    /// example a remote already registered in the space.
    pub async fn add_repository(
        &self,
        space: Option<&str>,
        project: Option<&str>,
        name: &str,
        remote_url: &str,
        default_branch: Option<&str>,
    ) -> Result<Repository, ClientError> {
        let request = OrgRequest::AddRepository {
            project: project.map(str::to_owned),
            name: name.to_owned(),
            remote_url: remote_url.to_owned(),
            default_branch: default_branch.map(str::to_owned),
        };
        match self.org(space, request).await? {
            (_, OrgReply::Repository { repository }) => Ok(repository),
            (space, _) => Err(wrong_reply(&space, "add repository")),
        }
    }

    /// The repositories of a project. `space: None` means the only paired
    /// hub; `project: None` means the General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn repositories(
        &self,
        space: Option<&str>,
        project: Option<&str>,
    ) -> Result<Vec<Repository>, ClientError> {
        let request = OrgRequest::Repositories {
            project: project.map(str::to_owned),
        };
        match self.org(space, request).await? {
            (_, OrgReply::Repositories { repositories }) => Ok(repositories),
            (space, _) => Err(wrong_reply(&space, "repositories")),
        }
    }

    /// The agents of one space. `None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn agents(&self, space: Option<&str>) -> Result<Vec<Agent>, ClientError> {
        match self.org(space, OrgRequest::Agents).await? {
            (_, OrgReply::Agents { agents }) => Ok(agents),
            (space, _) => Err(wrong_reply(&space, "agents")),
        }
    }

    /// Creates an agent in one space. `None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn create_agent(
        &self,
        space: Option<&str>,
        name: &str,
        role: &str,
        instructions: &str,
    ) -> Result<Agent, ClientError> {
        let request = OrgRequest::CreateAgent {
            name: name.to_owned(),
            role: role.to_owned(),
            instructions: instructions.to_owned(),
        };
        match self.org(space, request).await? {
            (_, OrgReply::Agent { agent }) => Ok(agent),
            (space, _) => Err(wrong_reply(&space, "create agent")),
        }
    }

    /// The bindings of a project. `space: None` means the only paired hub;
    /// `project: None` means the General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn bindings(
        &self,
        space: Option<&str>,
        project: Option<&str>,
    ) -> Result<Vec<BindingSummary>, ClientError> {
        let request = OrgRequest::Bindings {
            project: project.map(str::to_owned),
        };
        match self.org(space, request).await? {
            (_, OrgReply::Bindings { bindings }) => Ok(bindings),
            (space, _) => Err(wrong_reply(&space, "bindings")),
        }
    }

    /// Sets an agent's mode in a project, binding it there if needed.
    /// `space: None` means the only paired hub; `project: None` means the
    /// General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn set_mode(
        &self,
        space: Option<&str>,
        project: Option<&str>,
        agent: &str,
        mode: Mode,
    ) -> Result<BindingSummary, ClientError> {
        let request = OrgRequest::SetMode {
            project: project.map(str::to_owned),
            agent: agent.to_owned(),
            mode,
        };
        match self.org(space, request).await? {
            (_, OrgReply::Binding { binding }) => Ok(binding),
            (space, _) => Err(wrong_reply(&space, "set mode")),
        }
    }

    /// The decisions waiting on you in `space`, or in every paired hub when
    /// `None`. A hub that cannot list them is reported in its place; the
    /// others are unaffected.
    ///
    /// # Errors
    ///
    /// No such hub, or no hub paired.
    pub async fn decisions(&self, space: Option<&str>) -> Result<Vec<SpaceDecisions>, ClientError> {
        let hubs: Vec<&PairedHub> = match space {
            Some(name) => vec![
                self.hubs
                    .iter()
                    .find(|hub| hub.name == name)
                    .ok_or_else(|| self.unknown(name))?,
            ],
            None if self.hubs.is_empty() => return Err(ClientError::NoHubs),
            None => self.hubs.iter().collect(),
        };
        let mut listed = Vec::with_capacity(hubs.len());
        for hub in hubs {
            let decisions = match self.api_for(hub).decisions(DecisionRequest::Pending).await {
                Ok(DecisionReply::Pending { decisions }) => Ok(decisions),
                Ok(_) => Err(wrong_reply(&hub.name, "pending decisions")),
                Err(error) => Err(api_error(&hub.name, error)),
            };
            listed.push(SpaceDecisions {
                space: hub.name.clone(),
                decisions,
            });
        }
        Ok(listed)
    }

    /// Answers decision `decision` (`D-0012`) of one space with the option
    /// labeled `option`. `space: None` means the only paired hub. Answering
    /// again is harmless: the first answer stands.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, an id that is not a
    /// decision's, or the hub's error, for example an unknown option.
    pub async fn answer(
        &self,
        space: Option<&str>,
        decision: &str,
        option: &str,
        note: Option<&str>,
    ) -> Result<SpaceAnswer, ClientError> {
        let (space, api) = self.target(space)?;
        let id = DecisionId::parse(decision).ok_or_else(|| {
            api_error(
                &space,
                ApiError::Invalid(format!("{decision:?} is not a decision id like D-0012")),
            )
        })?;
        let request = DecisionRequest::Answer {
            decision: id,
            option: option.to_owned(),
            note: note.map(str::to_owned),
        };
        match api.decisions(request).await {
            Ok(DecisionReply::Answered { decision, recorded }) => Ok(SpaceAnswer {
                space,
                decision: *decision,
                recorded,
            }),
            Ok(_) => Err(wrong_reply(&space, "answer")),
            Err(error) => Err(api_error(&space, error)),
        }
    }

    async fn org(
        &self,
        space: Option<&str>,
        request: OrgRequest,
    ) -> Result<(String, OrgReply), ClientError> {
        let (space, api) = self.target(space)?;
        let reply = api.org(request).await.map_err(|e| api_error(&space, e))?;
        Ok((space, reply))
    }

    /// Sends `text` to a project's lead, with a fresh `client_message_id`.
    /// `space: None` means the only paired hub; `project: None` means the
    /// General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn send(
        &self,
        space: Option<&str>,
        project: Option<&str>,
        text: &str,
    ) -> Result<Sent, ClientError> {
        let (space, api) = self.target(space)?;
        let message = UserMessage {
            project: project.map(str::to_owned),
            text: text.to_owned(),
            client_message_id: uuid::Uuid::new_v4().to_string(),
        };
        let accepted = api
            .send_message(&message)
            .await
            .map_err(|e| api_error(&space, e))?;
        Ok(Sent {
            space,
            project: message.project,
            client_message_id: message.client_message_id,
            accepted,
        })
    }

    /// The feed items of one space after its cursor, oldest first, waiting up
    /// to `wait` if there are none. Moves the cursor past them; the move is
    /// saved by the next poll or by [`ClientCore::commit_feed`].
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, the hub's error, or an error
    /// saving the previous position.
    pub async fn poll_feed(
        &mut self,
        space: Option<&str>,
        wait: Duration,
    ) -> Result<Vec<SpaceFeedItem>, ClientError> {
        let (space, api) = self.target(space)?;
        // The caller has had everything returned so far.
        self.commit_feed()?;
        let after = self.next_after(&space);
        let items = api
            .feed(after, wait)
            .await
            .map_err(|e| api_error(&space, e))?;
        let items: Vec<_> = items
            .into_iter()
            .filter(|item| item.seq > after)
            .map(|item| SpaceFeedItem {
                space: space.clone(),
                item,
            })
            .collect();
        if let Some(last) = items.iter().map(|item| item.item.seq).max() {
            self.advance(&space, last);
        }
        Ok(items)
    }

    /// Saves the feed positions of the items already returned.
    ///
    /// # Errors
    ///
    /// An error reading or writing [`STATE_FILE`].
    pub fn commit_feed(&mut self) -> Result<(), ClientError> {
        if self.unsaved.is_empty() {
            return Ok(());
        }
        let updates = self
            .unsaved
            .iter()
            .filter_map(|name| {
                self.position
                    .get(name)
                    .map(|&seq| (name.clone(), Some(seq)))
            })
            .collect();
        self.write_cursors(updates)?;
        self.unsaved.clear();
        Ok(())
    }

    /// Makes the next poll of one space start at the beginning of its feed.
    /// Nothing is saved until items are returned again.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or an error saving the
    /// previous position.
    pub fn rewind_feed(&mut self, space: Option<&str>) -> Result<(), ClientError> {
        let (space, _api) = self.target(space)?;
        self.commit_feed()?;
        self.position.insert(space, 0);
        Ok(())
    }

    /// Follows the feed of one space until the first item of kind `message`
    /// arrives, handing every item up to and including it to `show`. Each
    /// item's position is saved once `show` returns; items after the message
    /// stay unread. Returns the message.
    ///
    /// # Errors
    ///
    /// [`ClientError::Timeout`] if no message arrives within `timeout`; no
    /// such hub, several hubs and none named, the hub's error, or an error
    /// saving the position.
    pub async fn wait_for_message(
        &mut self,
        space: Option<&str>,
        timeout: Duration,
        mut show: impl FnMut(&SpaceFeedItem),
    ) -> Result<SpaceFeedItem, ClientError> {
        let (space, api) = self.target(space)?;
        let deadline = Instant::now() + timeout;
        loop {
            self.commit_feed()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ClientError::Timeout { space, timeout });
            }
            let after = self.next_after(&space);
            let items = api
                .feed(after, remaining.min(LONGEST_POLL))
                .await
                .map_err(|e| api_error(&space, e))?;
            for item in items.into_iter().filter(|item| item.seq > after) {
                let item = SpaceFeedItem {
                    space: space.clone(),
                    item,
                };
                show(&item);
                self.advance(&space, item.item.seq);
                if item.kind() == OutboxKind::Message {
                    self.commit_feed()?;
                    return Ok(item);
                }
            }
        }
    }

    fn advance(&mut self, space: &str, seq: i64) {
        self.position.insert(space.to_owned(), seq);
        self.unsaved.insert(space.to_owned());
    }

    fn next_after(&self, space: &str) -> i64 {
        self.position
            .get(space)
            .or_else(|| self.saved.get(space))
            .copied()
            .unwrap_or(0)
    }

    fn names(&self) -> Vec<String> {
        self.hubs.iter().map(|hub| hub.name.clone()).collect()
    }

    fn unknown(&self, name: &str) -> ClientError {
        ClientError::UnknownHub {
            name: name.to_owned(),
            paired: self.names(),
        }
    }

    /// The hub a request goes to: the named one, or the only one.
    fn target(&self, space: Option<&str>) -> Result<(String, Arc<dyn HubApi>), ClientError> {
        let hub = match (space, self.hubs.as_slice()) {
            (Some(name), hubs) => hubs
                .iter()
                .find(|hub| hub.name == name)
                .ok_or_else(|| self.unknown(name))?,
            (None, []) => return Err(ClientError::NoHubs),
            (None, [only]) => only,
            (None, _) => return Err(ClientError::AmbiguousHub(self.names())),
        };
        Ok((hub.name.clone(), self.api_for(hub)))
    }

    fn api_for(&self, hub: &PairedHub) -> Arc<dyn HubApi> {
        match self.injected.get(&hub.name) {
            Some(api) => Arc::clone(api),
            None => Arc::new(LocalClient::new(&hub.socket)),
        }
    }

    fn save_hubs(&self, hubs: &[PairedHub]) -> Result<(), ClientError> {
        let path = self.dirs.hubs_file();
        let file = HubsFile {
            hubs: hubs
                .iter()
                .filter(|hub| !self.transient.contains(&hub.name))
                .cloned()
                .collect(),
        };
        let text = toml::to_string(&file).map_err(|e| ClientError::Corrupt {
            path: path.clone(),
            message: format!("cannot encode the hub list: {e}"),
        })?;
        write_atomic(&path, text.as_bytes())
    }

    /// Applies `updates` (a cursor, or `None` to drop it) to the state file
    /// as it is on disk now, so cursors written meanwhile by another process
    /// for other hubs are kept.
    fn write_cursors(&mut self, updates: BTreeMap<String, Option<i64>>) -> Result<(), ClientError> {
        let path = self.dirs.state_file();
        let mut state = read_state(&path)?;
        for (name, cursor) in updates {
            match cursor {
                Some(seq) => state.cursors.insert(name, seq),
                None => state.cursors.remove(&name),
            };
        }
        let text = serde_json::to_vec_pretty(&state).map_err(|e| ClientError::Corrupt {
            path: path.clone(),
            message: format!("cannot encode the state: {e}"),
        })?;
        write_atomic(&path, &text)?;
        self.saved = state.cursors;
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<(), ClientError> {
    let valid = (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(ClientError::InvalidName(name.to_owned()))
    }
}

fn read_state(path: &Path) -> Result<StateFile, ClientError> {
    Ok(read_file(path, |text| {
        serde_json::from_str::<StateFile>(text).map_err(|e| e.to_string())
    })?
    .unwrap_or_default())
}

/// Reads and parses `path`; `None` if it does not exist.
fn read_file<T>(
    path: &Path,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Result<Option<T>, ClientError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ClientError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    parse(&text)
        .map(Some)
        .map_err(|message| ClientError::Corrupt {
            path: path.to_path_buf(),
            message,
        })
}

/// Writes `contents` to a temporary file beside `path`, then renames it over
/// `path`, so a reader sees the old file or the new one and never a part.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), ClientError> {
    let failed = |source| ClientError::Write {
        path: path.to_path_buf(),
        source,
    };
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(failed)?;
    let file_name = path
        .file_name()
        .map_or_else(|| "file".into(), |name| name.to_string_lossy());
    let temp = dir.join(format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let written = File::create(&temp).and_then(|mut file| {
        file.write_all(contents)?;
        file.sync_all()
    });
    let result = written.and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ignored = fs::remove_file(&temp);
    }
    result.map_err(failed)
}

#[cfg(test)]
mod tests;
