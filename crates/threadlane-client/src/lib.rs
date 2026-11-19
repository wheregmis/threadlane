//! The client core (Plan.md §11.3, §18.2): connections to paired hubs and the
//! merging of what they report, with no user interface in it.
//!
//! [`ClientCore`] keeps the list of paired hubs in `hubs.toml` and the feed
//! position per hub in `state.json`, both in the client directory. A hub is
//! reached over its Unix socket on this computer, or over iroh with a key of
//! this client's own for that hub alone, kept in `keys/<name>.key` (Plan.md
//! §3.5 "one key per joining"). Every item it hands out is labeled with its
//! space, which is the name the hub was paired under. See
//! `docs/milestone-1.md` §7.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, DirBuilder, File};
use std::future::Future;
use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use threadlane_core::admission::AdmittedKey;
use threadlane_core::client::{
    Accepted, AgentTemplate, ApiError, BindingSummary, ConversationEntry, DecisionReply,
    DecisionRequest, FeedItem, HubApi, HubInfo, LeadRun, MemoryReply, MemoryRequest, OrgReply,
    OrgRequest, OverviewReply, OverviewRequest, PairingReply, PairingRequest, ProjectSummary,
    TaskDetail, TaskSummary, UserMessage,
};
use threadlane_core::ids::{DecisionId, TaskId};
use threadlane_core::model::{
    Account, Agent, BackendKind, Decision, Machine, Mode, OutboxKind, Repository,
};
use threadlane_core::time::Timestamp;
use threadlane_transport_iroh::{EndpointAddr, EndpointId, IrohClient, JoinCode, TransportAddr};
use threadlane_transport_local::{LocalClient, SOCKET_FILE};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// The file in the client directory that lists the paired hubs.
pub const HUBS_FILE: &str = "hubs.toml";

/// The file in the client directory that holds the feed position per hub.
pub const STATE_FILE: &str = "state.json";

/// The directory in the client directory that holds one key per hub paired
/// over iroh.
pub const KEYS_DIR: &str = "keys";

/// The pause before polling a socket hub's feed again after an error.
const POLL_BACKOFF: Duration = Duration::from_secs(1);

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

    /// Suggested directory for a new local space, beneath this client's spaces root.
    ///
    /// # Errors
    ///
    /// Returns an error if the name cannot be used as a single directory name.
    pub fn space_dir(&self, name: &str) -> Result<PathBuf, ClientError> {
        validate_name(name)?;
        Ok(self.root.join("spaces").join(name))
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

    /// The key this client uses for the hub paired as `name` over iroh.
    #[must_use]
    pub fn key_file(&self, name: &str) -> PathBuf {
        self.root.join(KEYS_DIR).join(format!("{name}.key"))
    }
}

/// A hub this client is paired with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedHub {
    /// The name the hub was paired under; it labels everything from this hub.
    pub name: String,
    /// How to reach it.
    #[serde(flatten)]
    pub reach: Reach,
}

/// How a paired hub is reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Reach {
    /// Its Unix socket, on this computer.
    Socket { socket: PathBuf },
    /// Over iroh: its id and the addresses its join code gave. Local
    /// discovery finds it if those change.
    Iroh {
        hub: EndpointId,
        addrs: Vec<SocketAddr>,
    },
}

impl fmt::Display for Reach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socket { socket } => write!(f, "{}", socket.display()),
            Self::Iroh { hub, .. } => write!(f, "iroh {}", hub.fmt_short()),
        }
    }
}

/// One hub as [`ClientCore::status`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubStatus {
    /// The name the hub was paired under.
    pub name: String,
    /// How it is reached. An empty socket for a hub injected with
    /// [`ClientCore::with_hub`].
    pub reach: Reach,
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
    /// The project named when sending; `None` means the General project,
    /// or none, for a message to a task.
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

/// What one space listed, or why its hub could not (Plan.md §11.3: each
/// space is asked on its own, and one that is away hides nothing else).
#[derive(Debug)]
pub struct SpaceListing<T> {
    /// The space it belongs to.
    pub space: String,
    /// What the hub listed, or why it could not.
    pub listed: Result<T, ClientError>,
}

/// The machines that joined one space, and its model accounts.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceMachines {
    /// Every machine membership, revoked ones included.
    pub machines: Vec<Machine>,
    /// The model accounts its machines run on.
    pub accounts: Vec<Account>,
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

/// Items from the feed of one hub or several, each labeled with its space.
/// An error is one failed attempt to reach a hub; the stream goes on.
pub type FeedStream = mpsc::Receiver<Result<SpaceFeedItem, ClientError>>;

/// A new join code from a hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedCode {
    /// The space it joins.
    pub space: String,
    /// The code, for `threadlane hub join`.
    pub code: String,
    /// When it stops working; it also stops after one use.
    pub expires_at: Timestamp,
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
    /// A suggested directory is already in use.
    #[error("{} already exists. Choose it explicitly to pair it, or use another directory.", .0.display())]
    DirectoryTaken(PathBuf),
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
    /// The text given is not a join code.
    #[error("invalid join code: {0}")]
    InvalidJoinCode(String),
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
/// Whether a hub predates a particular overview request.
pub fn unknown_overview(error: &ClientError, op: &str) -> bool {
    matches!(error, ClientError::Api { source: ApiError::Invalid(message), .. }
        if message.contains(&format!("unknown variant `{op}`")))
}

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
    /// One iroh client per hub paired over iroh, made on first use.
    iroh: Mutex<HashMap<String, Arc<IrohClient>>>,
    /// Whether iroh looks for hubs on the local network.
    local_discovery: bool,
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
            iroh: Mutex::default(),
            local_discovery: true,
        })
    }

    /// Whether to look for hubs on the local network when reaching them over
    /// iroh (on by default). Without it, only the addresses from a hub's join
    /// code are tried.
    #[must_use]
    pub fn with_local_discovery(mut self, on: bool) -> Self {
        self.local_discovery = on;
        self
    }

    /// Uses `api` for the hub `name` instead of its socket. If no hub of that
    /// name is paired, one is added for this process only. For tests.
    #[must_use]
    pub fn with_hub(mut self, name: &str, api: Arc<dyn HubApi>) -> Self {
        if !self.hubs.iter().any(|hub| hub.name == name) {
            self.hubs.push(PairedHub {
                name: name.to_owned(),
                reach: Reach::Socket {
                    socket: PathBuf::new(),
                },
            });
            self.transient.insert(name.to_owned());
        }
        self.injected.insert(name.to_owned(), api);
        self
    }

    fn check_new_name(&self, name: &str) -> Result<(), ClientError> {
        validate_name(name)?;
        if self.hubs.iter().any(|hub| hub.name == name) {
            return Err(ClientError::AlreadyPaired(name.to_owned()));
        }
        Ok(())
    }

    /// Pairs a local data directory, creating it before registering its socket.
    /// Only an explicitly selected directory may already exist. An explicit path
    /// pointing to a `hub.sock` file (not an existing directory) pairs its parent
    /// directory instead of creating a directory named `hub.sock`.
    ///
    /// # Errors
    ///
    /// Returns a name, directory creation, or persistence error.
    pub fn pair_directory(
        &mut self,
        name: &str,
        custom: Option<PathBuf>,
    ) -> Result<(), ClientError> {
        self.check_new_name(name)?;
        let explicit = custom.is_some();
        let dir = match custom {
            Some(dir) => dir,
            None => self.dirs.space_dir(name)?,
        };
        let dir =
            std::path::absolute(&dir).map_err(|source| ClientError::Read { path: dir, source })?;
        let dir = if explicit
            && dir.file_name() == Some(std::ffi::OsStr::new(SOCKET_FILE))
            && !dir.is_dir()
        {
            dir.parent().map(Path::to_path_buf).unwrap_or(dir)
        } else {
            dir
        };
        let socket = dir.join(SOCKET_FILE);
        std::os::unix::net::SocketAddr::from_pathname(&socket).map_err(|source| {
            ClientError::Write {
                path: socket.clone(),
                source,
            }
        })?;
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        if let Some(parent) = dir.parent() {
            builder
                .recursive(true)
                .create(parent)
                .map_err(|source| ClientError::Write {
                    path: parent.to_owned(),
                    source,
                })?;
        }
        let created = match builder.recursive(false).create(&dir) {
            Ok(()) => true,
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists && explicit && dir.is_dir() =>
            {
                false
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && !explicit => {
                return Err(ClientError::DirectoryTaken(dir));
            }
            Err(source) => return Err(ClientError::Write { path: dir, source }),
        };
        let paired = self.add_hub(PairedHub {
            name: name.to_owned(),
            reach: Reach::Socket { socket },
        });
        if created && paired.is_err() {
            // Never remove files another process may have put in the directory.
            let _ignored = fs::remove_dir(&dir);
        }
        paired
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
        self.check_new_name(name)?;
        let socket = std::path::absolute(&socket).map_err(|source| ClientError::Read {
            path: socket.clone(),
            source,
        })?;
        let socket = if socket.is_dir() {
            socket.join(SOCKET_FILE)
        } else {
            socket
        };
        self.add_hub(PairedHub {
            name: name.to_owned(),
            reach: Reach::Socket { socket },
        })
    }

    /// Pairs the hub of a join code under `name` over iroh: makes a new key
    /// for this hub alone, presents the code's one-time secret, and saves the
    /// pairing once the hub has admitted the key.
    ///
    /// # Errors
    ///
    /// [`ClientError::InvalidName`], [`ClientError::AlreadyPaired`],
    /// [`ClientError::InvalidJoinCode`], the hub unreachable or refusing the
    /// code, or an error writing the key or the list.
    pub async fn join(&mut self, name: &str, code: &str) -> Result<(), ClientError> {
        self.check_new_name(name)?;
        let code: JoinCode = code.parse().map_err(ClientError::InvalidJoinCode)?;
        let key_file = self.dirs.key_file(name);
        let failed = |source| ClientError::Write {
            path: key_file.clone(),
            source,
        };
        if let Some(dir) = key_file.parent() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(failed)?;
        }
        // Left over from a pairing of this name that is gone; never reused.
        match fs::remove_file(&key_file) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(failed(error)),
            _ => {}
        }
        let secret = threadlane_transport_iroh::create_key(&key_file).map_err(failed)?;
        if let Err(error) =
            threadlane_transport_iroh::join(secret, &code, self.local_discovery).await
        {
            let _ignored = fs::remove_file(&key_file);
            return Err(api_error(name, error));
        }
        let added = self.add_hub(PairedHub {
            name: name.to_owned(),
            reach: Reach::Iroh {
                hub: code.hub.id,
                addrs: code.hub.ip_addrs().copied().collect(),
            },
        });
        if added.is_err() {
            let _ignored = fs::remove_file(&key_file);
        }
        added
    }

    /// Saves the list with `hub` added; its feed starts at the beginning.
    fn add_hub(&mut self, hub: PairedHub) -> Result<(), ClientError> {
        let name = hub.name.clone();
        let name = name.as_str();
        let mut hubs = self.hubs.clone();
        hubs.push(hub);
        // Clear an orphaned cursor before committing the new pairing.
        if self.saved.contains_key(name) {
            self.write_cursors(BTreeMap::from([(name.to_owned(), None)]))?;
        }
        self.save_hubs(&hubs)?;
        self.hubs = hubs;
        self.position.remove(name);
        self.unsaved.remove(name);
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
        self.iroh.lock().expect("iroh clients lock").remove(name);
        let key_file = self.dirs.key_file(name);
        match fs::remove_file(&key_file) {
            Err(source) if source.kind() != io::ErrorKind::NotFound => {
                return Err(ClientError::Write {
                    path: key_file,
                    source,
                });
            }
            _ => {}
        }
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
        let checks: Vec<_> = self
            .hubs
            .iter()
            .map(|hub| {
                let api = self.api_for(hub);
                let check = tokio::spawn(async move {
                    let api = api.map_err(|error| ApiError::Unreachable(error.to_string()))?;
                    tokio::time::timeout(STATUS_TIMEOUT, api.info())
                        .await
                        .unwrap_or_else(|_elapsed| {
                            Err(ApiError::Unreachable(format!(
                                "no answer within {} s",
                                STATUS_TIMEOUT.as_secs()
                            )))
                        })
                });
                (hub.clone(), check)
            })
            .collect();
        let mut statuses = Vec::with_capacity(checks.len());
        for (hub, check) in checks {
            let info = check.await.unwrap_or_else(|error| {
                Err(ApiError::Internal(format!("status check failed: {error}")))
            });
            statuses.push(HubStatus {
                name: hub.name,
                reach: hub.reach,
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
        self.project_list(space, false).await
    }

    /// Lists the space's projects, including archived ones when requested.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn project_list(
        &self,
        space: Option<&str>,
        include_archived: bool,
    ) -> Result<SpaceProjects, ClientError> {
        let (space, api) = self.target(space)?;
        let projects = api
            .project_list(include_archived)
            .await
            .map_err(|e| api_error(&space, e))?;
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

    /// The starter agents a space's hub offers. `None` means the only paired
    /// hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn agent_templates(
        &self,
        space: Option<&str>,
    ) -> Result<Vec<AgentTemplate>, ClientError> {
        match self.org(space, OrgRequest::Templates).await? {
            (_, OrgReply::Templates { templates }) => Ok(templates),
            (space, _) => Err(wrong_reply(&space, "agent templates")),
        }
    }

    /// Lists personal skill names on the selected hub, with the backends
    /// that could not list theirs.
    ///
    /// # Errors
    /// Returns pairing, discovery or hub errors.
    pub async fn installed_skills(
        &self,
        space: Option<&str>,
    ) -> Result<(Vec<threadlane_core::model::SkillBinding>, Vec<BackendKind>), ClientError> {
        match self.org(space, OrgRequest::InstalledSkills).await? {
            (
                _,
                OrgReply::InstalledSkills {
                    skills,
                    unavailable,
                },
            ) => Ok((skills, unavailable)),
            (space, _) => Err(wrong_reply(&space, "installed skills")),
        }
    }

    /// Creates an agent in one space, with the backends it may use in order
    /// of preference (empty for either). `None` means the only paired hub.
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
        backends: &[BackendKind],
    ) -> Result<Agent, ClientError> {
        self.create_agent_with_models(space, name, role, instructions, backends, BTreeMap::new())
            .await
    }

    /// Creates an agent with model overrides keyed by backend name.
    ///
    /// # Errors
    ///
    /// Returns the same pairing and hub errors as [`Self::create_agent`].
    pub async fn create_agent_with_models(
        &self,
        space: Option<&str>,
        name: &str,
        role: &str,
        instructions: &str,
        backends: &[BackendKind],
        models: BTreeMap<String, String>,
    ) -> Result<Agent, ClientError> {
        self.create_agent_profile(
            space,
            name,
            role,
            instructions,
            backends,
            models,
            Vec::new(),
        )
        .await
    }

    /// Creates a profile with per-provider models and personal skill references.
    ///
    /// # Errors
    /// Returns pairing or hub validation errors.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the existing profile creation API"
    )]
    pub async fn create_agent_profile(
        &self,
        space: Option<&str>,
        name: &str,
        role: &str,
        instructions: &str,
        backends: &[BackendKind],
        models: BTreeMap<String, String>,
        skills: Vec<threadlane_core::model::SkillBinding>,
    ) -> Result<Agent, ClientError> {
        let name = name.to_owned();
        let role = role.to_owned();
        let instructions = instructions.to_owned();
        let backends = backends.to_vec();
        let request = if !skills.is_empty() {
            OrgRequest::CreateAgentWithSkills {
                name,
                role,
                instructions,
                backends,
                models,
                skills,
            }
        } else if models.is_empty() {
            OrgRequest::CreateAgent {
                name,
                role,
                instructions,
                backends,
            }
        } else {
            OrgRequest::CreateAgentWithModels {
                name,
                role,
                instructions,
                backends,
                models,
            }
        };
        match self.org(space, request).await? {
            (_, OrgReply::Agent { agent }) => Ok(agent),
            (space, _) => Err(wrong_reply(&space, "create agent")),
        }
    }

    /// Changes an agent; each `None` keeps that field. The request is
    /// [`OrgRequest::UpdateAgent`] as built by the caller.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn update_agent(
        &self,
        space: Option<&str>,
        request: OrgRequest,
    ) -> Result<Agent, ClientError> {
        match self.org(space, request).await? {
            (_, OrgReply::Agent { agent }) => Ok(agent),
            (space, _) => Err(wrong_reply(&space, "update agent")),
        }
    }

    /// Deletes an agent whose exact name was confirmed.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn delete_agent(
        &self,
        space: Option<&str>,
        agent: &str,
        confirm_name: &str,
    ) -> Result<String, ClientError> {
        let request = OrgRequest::DeleteAgent {
            agent: agent.to_owned(),
            confirm_name: confirm_name.to_owned(),
        };
        match self.org(space, request).await? {
            (_, OrgReply::AgentDeleted { agent }) => Ok(agent),
            (space, _) => Err(wrong_reply(&space, "delete agent")),
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
        let hubs = self.selected(space)?;
        let mut listed = Vec::with_capacity(hubs.len());
        for hub in hubs {
            let decisions = match self.api_for(hub) {
                Ok(api) => match api.decisions(DecisionRequest::Pending).await {
                    Ok(DecisionReply::Pending { decisions }) => Ok(decisions),
                    Ok(_) => Err(wrong_reply(&hub.name, "pending decisions")),
                    Err(error) => Err(api_error(&hub.name, error)),
                },
                Err(error) => Err(error),
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
        self.answer_as(space, decision, option, note, false).await
    }

    /// [`ClientCore::answer`], and with `always` also "always allow this
    /// here" ([`DecisionRequest::AnswerAlways`]).
    pub(crate) async fn answer_as(
        &self,
        space: Option<&str>,
        decision: &str,
        option: &str,
        note: Option<&str>,
        always: bool,
    ) -> Result<SpaceAnswer, ClientError> {
        let (space, api) = self.target(space)?;
        let id = DecisionId::parse(decision).ok_or_else(|| {
            api_error(
                &space,
                ApiError::Invalid(format!("{decision:?} is not a decision id like D-0012")),
            )
        })?;
        let (option, note) = (option.to_owned(), note.map(str::to_owned));
        let request = if always {
            DecisionRequest::AnswerAlways {
                decision: id,
                option,
                note,
            }
        } else {
            DecisionRequest::Answer {
                decision: id,
                option,
                note,
            }
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

    /// The named hub, or every paired hub when `None`.
    fn selected(&self, space: Option<&str>) -> Result<Vec<&PairedHub>, ClientError> {
        match space {
            Some(name) => Ok(vec![
                self.hubs
                    .iter()
                    .find(|hub| hub.name == name)
                    .ok_or_else(|| self.unknown(name))?,
            ]),
            None if self.hubs.is_empty() => Err(ClientError::NoHubs),
            None => Ok(self.hubs.iter().collect()),
        }
    }

    /// Asks the named hub, or every paired hub when `None`, the same
    /// overview request, and keeps what `take` makes of each answer. A hub
    /// that cannot answer is reported in its place; the others are
    /// unaffected.
    async fn overview_each<T>(
        &self,
        space: Option<&str>,
        request: &OverviewRequest,
        what: &str,
        take: impl Fn(&str, OverviewReply) -> Option<T>,
    ) -> Result<Vec<SpaceListing<T>>, ClientError> {
        let hubs = self.selected(space)?;
        let mut listed = Vec::with_capacity(hubs.len());
        for hub in hubs {
            let answer = match self.api_for(hub) {
                Ok(api) => match api.overview(request.clone()).await {
                    Ok(reply) => take(&hub.name, reply).ok_or_else(|| wrong_reply(&hub.name, what)),
                    Err(error) => Err(api_error(&hub.name, error)),
                },
                Err(error) => Err(error),
            };
            listed.push(SpaceListing {
                space: hub.name.clone(),
                listed: answer,
            });
        }
        Ok(listed)
    }

    /// The running tasks of `space`, or of every paired hub when `None`,
    /// oldest first.
    ///
    /// # Errors
    ///
    /// No such hub, or no hub paired.
    pub async fn running(
        &self,
        space: Option<&str>,
    ) -> Result<Vec<SpaceListing<Vec<TaskSummary>>>, ClientError> {
        self.overview_each(
            space,
            &OverviewRequest::Running,
            "running",
            |_, reply| match reply {
                OverviewReply::Tasks { tasks } => Some(tasks),
                _ => None,
            },
        )
        .await
    }

    /// The newest `limit` delivered items of `space`, or of every paired hub
    /// when `None`, oldest first and labeled with their space.
    ///
    /// # Errors
    ///
    /// No such hub, or no hub paired.
    pub async fn recent(
        &self,
        space: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SpaceListing<Vec<SpaceFeedItem>>>, ClientError> {
        let request = OverviewRequest::Recent { limit };
        self.overview_each(space, &request, "recent", |space, reply| match reply {
            OverviewReply::Recent { items } => Some(
                items
                    .into_iter()
                    .map(|item| SpaceFeedItem {
                        space: space.to_owned(),
                        item,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .await
    }

    /// The machines and accounts of `space`, or of every paired hub when
    /// `None`.
    ///
    /// # Errors
    ///
    /// No such hub, or no hub paired.
    pub async fn machines(
        &self,
        space: Option<&str>,
    ) -> Result<Vec<SpaceListing<SpaceMachines>>, ClientError> {
        self.overview_each(
            space,
            &OverviewRequest::Machines,
            "machines",
            |_, reply| match reply {
                OverviewReply::Machines { machines, accounts } => {
                    Some(SpaceMachines { machines, accounts })
                }
                _ => None,
            },
        )
        .await
    }

    /// The tasks of a project, oldest first. `space: None` means the only
    /// paired hub; `project: None` means the General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn tasks(
        &self,
        space: Option<&str>,
        project: Option<&str>,
    ) -> Result<Vec<TaskSummary>, ClientError> {
        let request = OverviewRequest::Tasks {
            project: project.map(str::to_owned),
        };
        match self.overview(space, request).await? {
            (_, OverviewReply::Tasks { tasks }) => Ok(tasks),
            (space, _) => Err(wrong_reply(&space, "tasks")),
        }
    }

    /// Batches the active project tasks and lead status for one hub.
    pub async fn work(
        &self,
        space: &str,
    ) -> Result<threadlane_core::client::WorkOverview, ClientError> {
        match self.overview(Some(space), OverviewRequest::Work).await? {
            (_, OverviewReply::Work { overview }) => Ok(overview),
            (space, _) => Err(wrong_reply(&space, "work")),
        }
    }

    /// Task details without historical activity, with a fallback for older hubs.
    pub async fn task_overview(
        &self,
        space: &str,
        task: TaskId,
    ) -> Result<TaskDetail, ClientError> {
        match self
            .overview(Some(space), OverviewRequest::TaskOverview { task })
            .await
        {
            Ok((_, OverviewReply::Task { task })) => Ok(*task),
            Ok((space, _)) => Err(wrong_reply(&space, "task")),
            Err(error) if unknown_overview(&error, "task_overview") => {
                self.task(Some(space), &task.to_string()).await
            }
            Err(error) => Err(error),
        }
    }

    /// A newest-first page of activity, or fabricated pages from older hubs' full detail.
    pub async fn task_activity(
        &self,
        space: &str,
        task: TaskId,
        before: Option<threadlane_core::client::ActivityCursor>,
    ) -> Result<
        (
            Vec<threadlane_core::client::TaskActivityEntry>,
            Option<threadlane_core::client::ActivityCursor>,
        ),
        ClientError,
    > {
        use threadlane_core::client::{ActivityCursor, CONVERSATION_PAGE, TaskActivityEntry};
        match self
            .overview(Some(space), OverviewRequest::TaskActivity { task, before })
            .await
        {
            Ok((_, OverviewReply::TaskActivity { entries, next })) => Ok((entries, next)),
            Ok((space, _)) => Err(wrong_reply(&space, "task activity")),
            Err(error) if unknown_overview(&error, "task_activity") => {
                let detail = self.task(Some(space), &task.to_string()).await?;
                let mut entries = detail
                    .runs
                    .into_iter()
                    .rev()
                    .flat_map(|summary| {
                        let run = summary.run.id;
                        summary
                            .steps
                            .into_iter()
                            .enumerate()
                            .rev()
                            .map(move |(index, step)| TaskActivityEntry {
                                position: ActivityCursor {
                                    run,
                                    seq: index as i64 + 1,
                                },
                                step,
                            })
                    })
                    .filter(|entry| {
                        before.is_none_or(|before| {
                            (entry.position.run, entry.position.seq) < (before.run, before.seq)
                        })
                    });
                let size = CONVERSATION_PAGE as usize;
                let page: Vec<_> = entries.by_ref().take(size).collect();
                let next = entries
                    .next()
                    .and_then(|_| page.last().map(|entry| entry.position));
                Ok((page, next))
            }
            Err(error) => Err(error),
        }
    }

    /// Task `task` (`T-0142`) of one space in full. `None` means the only
    /// paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, an id that is not a
    /// task's, or the hub's error.
    pub async fn task(&self, space: Option<&str>, task: &str) -> Result<TaskDetail, ClientError> {
        let (name, _api) = self.target(space)?;
        let id = TaskId::parse(task).ok_or_else(|| {
            api_error(
                &name,
                ApiError::Invalid(format!("{task:?} is not a task id like T-0142")),
            )
        })?;
        match self
            .overview(space, OverviewRequest::Task { task: id })
            .await?
        {
            (_, OverviewReply::Task { task }) => Ok(*task),
            (space, _) => Err(wrong_reply(&space, "task")),
        }
    }

    /// The newest `limit` entries of the conversation with a project's lead
    /// before `before` (the newest of all when `None`), oldest first.
    /// `space: None` means the only paired hub; `project: None` means the
    /// General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn conversation(
        &self,
        space: Option<&str>,
        project: Option<&str>,
        before: Option<Timestamp>,
        limit: u32,
    ) -> Result<Vec<ConversationEntry>, ClientError> {
        let request = OverviewRequest::Conversation {
            project: project.map(str::to_owned),
            before,
            limit,
        };
        match self.overview(space, request).await? {
            (_, OverviewReply::Conversation { entries }) => Ok(entries),
            (space, _) => Err(wrong_reply(&space, "conversation")),
        }
    }

    /// When the run in progress on a project's lead lane started, or `None`
    /// while the lead is idle. `space: None` means the only paired hub;
    /// `project: None` means the General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn lead(
        &self,
        space: Option<&str>,
        project: Option<&str>,
    ) -> Result<Option<Timestamp>, ClientError> {
        self.lead_status(space, project)
            .await
            .map(|(since, _)| since)
    }

    /// Reads when the lead started its current run and whether its lane is stopped.
    ///
    /// # Errors
    ///
    /// No such hub or project, several hubs and none named, or the hub's error.
    pub async fn lead_status(
        &self,
        space: Option<&str>,
        project: Option<&str>,
    ) -> Result<(Option<Timestamp>, bool), ClientError> {
        let request = OverviewRequest::Lead {
            project: project.map(str::to_owned),
        };
        match self.overview(space, request).await? {
            (
                _,
                OverviewReply::Lead {
                    running_since,
                    stopped,
                    ..
                },
            ) => Ok((running_since, stopped)),
            (space, _) => Err(wrong_reply(&space, "lead")),
        }
    }

    /// Lists a project's lead runs, their message markers, and recorded rationale.
    /// `space: None` means the only paired hub and `project: None` means General.
    ///
    /// # Errors
    ///
    /// No such hub or project, several hubs and none named, or the hub's error.
    pub async fn lead_runs(
        &self,
        space: Option<&str>,
        project: Option<&str>,
    ) -> Result<Vec<LeadRun>, ClientError> {
        let request = OverviewRequest::LeadRuns {
            project: project.map(str::to_owned),
        };
        match self.overview(space, request).await? {
            (_, OverviewReply::LeadRuns { runs }) => Ok(runs),
            (space, _) => Err(wrong_reply(&space, "lead runs")),
        }
    }

    async fn overview(
        &self,
        space: Option<&str>,
        request: OverviewRequest,
    ) -> Result<(String, OverviewReply), ClientError> {
        let (space, api) = self.target(space)?;
        let reply = api
            .overview(request)
            .await
            .map_err(|e| api_error(&space, e))?;
        Ok((space, reply))
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

    /// Checks for a hub update or installs it and waits for the new hub to report its version.
    pub fn update_hub(
        &self,
        space: &str,
        request: threadlane_core::client::HubUpdate,
    ) -> impl Future<Output = Result<Option<String>, ClientError>> + use<> {
        use threadlane_core::client::HubUpdate;

        let target = self.target(Some(space));
        async move {
            let (space, api) = target?;
            let result = api.update_hub(request.clone()).await;
            let HubUpdate::Install { version } = request else {
                return result.map_err(|error| api_error(&space, error));
            };
            // An update can close the connection before its acknowledgment arrives.
            if let Err(error) = result
                && !matches!(&error, ApiError::Unreachable(_))
            {
                return Err(api_error(&space, error));
            }
            tokio::time::timeout(Duration::from_secs(90), async {
                loop {
                    if let Ok(info) = api.info().await
                        && info.update_version.as_deref() == Some(version.as_str())
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            })
            .await
            .map_err(|_| api_error(&space, ApiError::Unreachable(format!(
                "Could not confirm the hub restarted on version {version}. Check its connection and try again."
            ))))?;
            Ok(Some(version))
        }
    }

    /// Asks one hub for a new join code. `None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn pair_code(&self, space: Option<&str>) -> Result<IssuedCode, ClientError> {
        match self.pairing(space, PairingRequest::IssueJoinCode).await? {
            (space, PairingReply::JoinCode { code, expires_at }) => Ok(IssuedCode {
                space,
                code,
                expires_at,
            }),
            (space, _) => Err(wrong_reply(&space, "join code")),
        }
    }

    /// The keys one hub has admitted, revoked ones included. `None` means the
    /// only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn clients(&self, space: Option<&str>) -> Result<Vec<AdmittedKey>, ClientError> {
        match self.pairing(space, PairingRequest::Clients).await? {
            (_, PairingReply::Clients { clients }) => Ok(clients),
            (space, _) => Err(wrong_reply(&space, "clients")),
        }
    }

    /// Revokes a client at one hub, which closes its connections at once.
    /// `None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn revoke(
        &self,
        space: Option<&str>,
        name: &str,
    ) -> Result<AdmittedKey, ClientError> {
        let request = PairingRequest::Revoke {
            name: name.to_owned(),
        };
        match self.pairing(space, request).await? {
            (_, PairingReply::Revoked { client }) => Ok(client),
            (space, _) => Err(wrong_reply(&space, "revoke")),
        }
    }

    async fn pairing(
        &self,
        space: Option<&str>,
        request: PairingRequest,
    ) -> Result<(String, PairingReply), ClientError> {
        let (space, api) = self.target(space)?;
        let reply = api
            .pairing(request)
            .await
            .map_err(|e| api_error(&space, e))?;
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
        self.send_with_images(space, project, text, Vec::new())
            .await
    }

    /// Sends text and images to a project's lead.
    ///
    /// # Errors
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn send_with_images(
        &self,
        space: Option<&str>,
        project: Option<&str>,
        text: &str,
        images: Vec<threadlane_core::attachment::ImageAttachment>,
    ) -> Result<Sent, ClientError> {
        let (space, api) = self.target(space)?;
        let message = UserMessage {
            project: project.map(str::to_owned),
            text: text.to_owned(),
            images,
            client_message_id: uuid::Uuid::new_v4().to_string(),
            task: None,
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

    /// Sends `text` to task `task` (`T-0142`) of one space, with a fresh
    /// `client_message_id`: it goes to the task's lane, and declines the
    /// decision open there with `text` as the note (Plan.md §9.4). `None`
    /// means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, an id that is not a
    /// task's, or the hub's error.
    pub async fn send_to_task(
        &self,
        space: Option<&str>,
        task: &str,
        text: &str,
    ) -> Result<Sent, ClientError> {
        let (space, api) = self.target(space)?;
        let id = TaskId::parse(task).ok_or_else(|| {
            api_error(
                &space,
                ApiError::Invalid(format!("{task:?} is not a task id like T-0142")),
            )
        })?;
        let message = UserMessage {
            project: None,
            text: text.to_owned(),
            images: Vec::new(),
            client_message_id: uuid::Uuid::new_v4().to_string(),
            task: Some(id),
        };
        let accepted = api
            .send_message(&message)
            .await
            .map_err(|e| api_error(&space, e))?;
        Ok(Sent {
            space,
            project: None,
            client_message_id: message.client_message_id,
            accepted,
        })
    }

    /// Reads a project's memory in one space, or changes it as you
    /// (Plan.md §8.3). `None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error: a path
    /// outside the project, or a change the store refuses, with its reason.
    pub async fn memory(
        &self,
        space: Option<&str>,
        request: MemoryRequest,
    ) -> Result<MemoryReply, ClientError> {
        let (space, api) = self.target(space)?;
        api.memory(request).await.map_err(|e| api_error(&space, e))
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

    /// The feed of one space from its cursor on, as a stream that stays
    /// open: pushed by the hub over iroh, long-polled over a socket. `None`
    /// means the only paired hub. Pass each item shown to
    /// [`ClientCore::acknowledge`]. Must be called inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or this client's key for
    /// the hub cannot be read.
    pub fn follow(&self, space: Option<&str>) -> Result<FeedStream, ClientError> {
        let (name, _api) = self.target(space)?;
        let (items, stream) = mpsc::channel(64);
        for hub in self.hubs.iter().filter(|hub| hub.name == name) {
            self.follow_hub(hub, items.clone())?;
        }
        Ok(stream)
    }

    /// The feeds of every paired hub merged into one stream, each item
    /// labeled with its space (Plan.md §11.3). A hub that cannot be reached
    /// shows up as errors; the others are unaffected. Must be called inside a
    /// Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`ClientError::NoHubs`], or a key that cannot be read.
    pub fn follow_all(&self) -> Result<FeedStream, ClientError> {
        if self.hubs.is_empty() {
            return Err(ClientError::NoHubs);
        }
        let (items, stream) = mpsc::channel(64);
        for hub in &self.hubs {
            self.follow_hub(hub, items.clone())?;
        }
        Ok(stream)
    }

    /// Feeds one hub's items into `items` from a task of their own, until
    /// the receiver is dropped.
    fn follow_hub(
        &self,
        hub: &PairedHub,
        items: mpsc::Sender<Result<SpaceFeedItem, ClientError>>,
    ) -> Result<(), ClientError> {
        let space = hub.name.clone();
        let mut after = self.next_after(&space);
        let label = |result: Result<FeedItem, ApiError>, space: &str| {
            result
                .map(|item| SpaceFeedItem {
                    space: space.to_owned(),
                    item,
                })
                .map_err(|error| api_error(space, error))
        };
        if !self.injected.contains_key(&space) && matches!(hub.reach, Reach::Iroh { .. }) {
            let mut pushed = self.iroh_client(hub)?.follow(after);
            tokio::spawn(async move {
                while let Some(result) = pushed.recv().await {
                    if items.send(label(result, &space)).await.is_err() {
                        return;
                    }
                }
            });
            return Ok(());
        }
        let api = self.api_for(hub)?;
        tokio::spawn(async move {
            loop {
                let polled = tokio::select! {
                    polled = api.feed(after, LONGEST_POLL) => polled,
                    () = items.closed() => return,
                };
                let results: Vec<_> = match polled {
                    Ok(polled) => polled
                        .into_iter()
                        .filter(|item| item.seq > after)
                        .map(Ok)
                        .collect(),
                    Err(error) => vec![Err(error)],
                };
                let failed = results.iter().any(Result::is_err);
                for result in results {
                    if let Ok(item) = &result {
                        after = item.seq;
                    }
                    if items.send(label(result, &space)).await.is_err() {
                        return;
                    }
                }
                if failed {
                    tokio::time::sleep(POLL_BACKOFF).await;
                }
            }
        });
        Ok(())
    }

    /// Records that `item`, from a feed stream, was shown, and saves its
    /// position at once.
    ///
    /// # Errors
    ///
    /// An error reading or writing [`STATE_FILE`].
    pub fn acknowledge(&mut self, item: &SpaceFeedItem) -> Result<(), ClientError> {
        if item.item.seq > self.next_after(&item.space) {
            self.advance(&item.space, item.item.seq);
        }
        self.commit_feed()
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
        Ok((hub.name.clone(), self.api_for(hub)?))
    }

    fn api_for(&self, hub: &PairedHub) -> Result<Arc<dyn HubApi>, ClientError> {
        if let Some(api) = self.injected.get(&hub.name) {
            return Ok(Arc::clone(api));
        }
        Ok(match &hub.reach {
            Reach::Socket { socket } => Arc::new(LocalClient::new(socket)),
            Reach::Iroh { .. } => self.iroh_client(hub)?,
        })
    }

    /// The one iroh client of a hub paired over iroh, made on first use.
    fn iroh_client(&self, hub: &PairedHub) -> Result<Arc<IrohClient>, ClientError> {
        let Reach::Iroh { hub: id, addrs } = &hub.reach else {
            return Err(self.unknown(&hub.name));
        };
        let mut clients = self.iroh.lock().expect("iroh clients lock");
        if let Some(client) = clients.get(&hub.name) {
            return Ok(Arc::clone(client));
        }
        let key_file = self.dirs.key_file(&hub.name);
        let secret =
            threadlane_transport_iroh::load_key(&key_file).map_err(|source| ClientError::Read {
                path: key_file,
                source,
            })?;
        let addr = EndpointAddr::from_parts(*id, addrs.iter().copied().map(TransportAddr::Ip));
        let client = Arc::new(IrohClient::new(secret, addr, self.local_discovery));
        clients.insert(hub.name.clone(), Arc::clone(&client));
        Ok(client)
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
    let valid = !matches!(name, "." | "..")
        && (1..=64).contains(&name.len())
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

mod tasks;
mod timers;

mod permissions;
pub use tasks::{DRAFT_PR_GOAL, NewTaskSpec};

#[cfg(test)]
mod tests;

impl ClientCore {
    /// Reads the current pacing backlog from each selected hub.
    ///
    /// # Errors
    ///
    /// No hub paired, or the named hub does not exist.
    pub async fn held_results(
        &self,
        space: Option<&str>,
    ) -> Result<Vec<SpaceListing<Option<threadlane_core::client::HeldResults>>>, ClientError> {
        self.overview_each(
            space,
            &OverviewRequest::HeldResults,
            "held results",
            |_, reply| match reply {
                OverviewReply::HeldResults { held } => Some(held),
                _ => None,
            },
        )
        .await
    }

    /// Reads the space's delivery pacing settings.
    pub async fn pacing(
        &self,
        space: Option<&str>,
    ) -> Result<threadlane_core::model::Pacing, ClientError> {
        self.pacing_request(space, OrgRequest::Pacing).await
    }

    /// Replaces the space's delivery pacing settings.
    pub async fn set_pacing(
        &self,
        space: Option<&str>,
        pacing: threadlane_core::model::Pacing,
    ) -> Result<threadlane_core::model::Pacing, ClientError> {
        self.pacing_request(space, OrgRequest::SetPacing { pacing })
            .await
    }

    async fn pacing_request(
        &self,
        space: Option<&str>,
        request: OrgRequest,
    ) -> Result<threadlane_core::model::Pacing, ClientError> {
        match self.org(space, request).await? {
            (_, OrgReply::Pacing { pacing }) => Ok(pacing),
            (space, _) => Err(wrong_reply(&space, "pacing")),
        }
    }

    /// Stops a lead lane or task, retaining its pending work.
    pub async fn stop(
        &self,
        space: Option<&str>,
        target: threadlane_core::client::LaneTarget,
    ) -> Result<(), ClientError> {
        self.lane_request(
            space,
            OrgRequest::Stop {
                target,
                client_request_id: uuid::Uuid::new_v4().to_string(),
            },
        )
        .await
    }

    /// Resumes a lead lane or task that was stopped.
    pub async fn resume_lane(
        &self,
        space: Option<&str>,
        target: threadlane_core::client::LaneTarget,
    ) -> Result<(), ClientError> {
        self.lane_request(space, OrgRequest::ResumeLane { target })
            .await
    }

    async fn lane_request(
        &self,
        space: Option<&str>,
        request: OrgRequest,
    ) -> Result<(), ClientError> {
        match self.org(space, request).await? {
            (_, OrgReply::LaneUpdated) => Ok(()),
            (space, _) => Err(wrong_reply(&space, "lane update")),
        }
    }
}

impl ClientCore {
    /// Reads operational history or performs an owner repair on one paired hub.
    ///
    /// # Errors
    ///
    /// The hub could not be selected or the operation failed.
    pub async fn operations(
        &self,
        space: Option<&str>,
        request: threadlane_core::client::OperationsRequest,
    ) -> Result<threadlane_core::client::OperationsReply, ClientError> {
        let (space, api) = self.target(space)?;
        api.operations(request)
            .await
            .map_err(|error| api_error(&space, error))
    }
}

/// Parses a dreaming setting as `off`, `daily HH:MM`, or `every Nh`.
/// The named `zone` applies to daily schedules and is validated by the hub.
pub fn parse_dreaming_cadence(
    input: &str,
    zone: &str,
) -> Result<Option<threadlane_core::model::Recurrence>, String> {
    use threadlane_core::model::{Recurrence, Rule};

    let mut parts = input.split_whitespace();
    let rule = match (parts.next(), parts.next(), parts.next()) {
        (Some("off"), None, None) => return Ok(None),
        (Some("daily"), Some(time), None) => {
            let bad_time = || "Enter a daily time in HH:MM.".to_owned();
            let [h1, h2, b':', m1, m2] = time.as_bytes() else {
                return Err(bad_time());
            };
            if ![h1, h2, m1, m2].into_iter().all(u8::is_ascii_digit) {
                return Err(bad_time());
            }
            let hour = (*h1 - b'0') * 10 + (*h2 - b'0');
            let minute = (*m1 - b'0') * 10 + (*m2 - b'0');
            if hour > 23 || minute > 59 {
                return Err(bad_time());
            }
            Rule::Daily { hour, minute }
        }
        (Some("every"), Some(hours), None) => {
            let minutes = hours
                .strip_suffix('h')
                .filter(|hours| hours.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|hours| hours.parse::<u32>().ok())
                .filter(|hours| *hours > 0)
                .and_then(|hours| hours.checked_mul(60))
                .ok_or_else(|| "Enter a positive whole number of hours, such as 6h.".to_owned())?;
            return Ok(Some(Recurrence {
                rule: Rule::Interval { minutes },
                zone: "UTC".to_owned(),
            }));
        }
        _ => return Err("Enter off, daily HH:MM, or every Nh.".to_owned()),
    };
    Ok(Some(Recurrence {
        rule,
        zone: zone.to_owned(),
    }))
}
