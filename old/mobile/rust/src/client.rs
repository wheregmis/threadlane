//! iOS executor and GPUI event-pump adapter for the shared daemon client.
use std::sync::{Arc, OnceLock};
use threadlane_client::{ConnectionState, DaemonClient, RemoteDaemon};
use threadlane_protocol::daemon::{CommandResponse, SessionCommand, SessionEvent};
use tokio::sync::mpsc;

pub enum MobileEvent {
    Event(SessionEvent),
    Connecting,
    Reconnecting,
    Connected,
    Fatal(String),
    CommandResult {
        command: SessionCommand,
        result: Result<CommandResponse, String>,
    },
}
pub struct MobileDaemon {
    client: Arc<RemoteDaemon>,
    events: Option<mpsc::UnboundedReceiver<MobileEvent>>,
    event_tx: mpsc::UnboundedSender<MobileEvent>,
    pump: tokio::task::JoinHandle<()>,
}
impl MobileDaemon {
    pub fn connect(url: String, token: Option<String>) -> Result<Self, String> {
        Self::connect_named(url, token, None)
    }

    pub fn connect_named(
        url: String,
        token: Option<String>,
        device_name: Option<String>,
    ) -> Result<Self, String> {
        let client = RemoteDaemon::connect_pairing_named(
            url,
            token.unwrap_or_default(),
            device_name,
            runtime().handle().clone(),
        )?;
        let mut events = client.subscribe();
        let mut connection = client.subscribe_connection();
        let (event_tx, rx) = mpsc::unbounded_channel();
        let tx = event_tx.clone();
        let pump = runtime().spawn(async move {
            let initial = match connection.borrow_and_update().clone() {
                ConnectionState::Connected => MobileEvent::Connected,
                ConnectionState::Failed(error) => MobileEvent::Fatal(error),
                ConnectionState::Reconnecting => MobileEvent::Reconnecting,
                ConnectionState::Connecting => MobileEvent::Connecting,
            };
            if tx.send(initial).is_err() {
                return;
            }

            loop {
                tokio::select! {
                    event = events.recv() => match event {
                        Some(event) => { if tx.send(MobileEvent::Event(event)).is_err() { break; } }
                        None => break,
                    },
                    changed = connection.changed() => {
                        if changed.is_err() { break; }
                        let event = match connection.borrow_and_update().clone() {
                            ConnectionState::Connected => MobileEvent::Connected,
                            ConnectionState::Failed(error) => MobileEvent::Fatal(error),
                            ConnectionState::Reconnecting => MobileEvent::Reconnecting,
                            ConnectionState::Connecting => MobileEvent::Connecting,
                        };
                        if tx.send(event).is_err() { break; }
                    }
                }
            }
        });
        Ok(Self {
            client,
            events: Some(rx),
            event_tx,
            pump,
        })
    }
    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }
    pub fn request_reconnect(&self) {
        self.client.request_reconnect();
    }
    pub fn paired_device_id(&self) -> Option<String> {
        self.client.paired_device_id()
    }
    pub fn send(&self, command: SessionCommand) {
        if let Err(message) = self.client.send(command) {
            let _ = self
                .event_tx
                .send(MobileEvent::Event(SessionEvent::DaemonError {
                    session_id: None,
                    message,
                }));
        }
    }
    pub fn request(&self, command: SessionCommand) {
        let client = self.client.clone();
        let tx = self.event_tx.clone();
        runtime().spawn(async move {
            let result = if matches!(command, SessionCommand::BeginSession { .. } | SessionCommand::GetComposerOptions { .. }) && !client.supports_composer_options() {
                Err("Update desktop to use new chats and composer options".into())
            } else if matches!(command, SessionCommand::GitRequest { .. }) && !client.supports_project_io() {
                Err("Update desktop to use the Git panel".into())
            } else if matches!(command, SessionCommand::GitHubRequest { .. } | SessionCommand::AutomationRequest { .. }) && !client.supports_github_automation() {
                Err("Update desktop to use issues, pull requests, and automations".into())
            } else if client.supports_command_requests() { client.request(command.clone()).await }
                else { Err("This desktop version cannot acknowledge commands. Update desktop before sending.".into()) };
            let _ = tx.send(MobileEvent::CommandResult { command, result });
        });
    }
    pub fn take_events(&mut self) -> Option<mpsc::UnboundedReceiver<MobileEvent>> {
        self.events.take()
    }
}
impl Drop for MobileDaemon {
    fn drop(&mut self) {
        self.pump.abort();
    }
}
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("mobile client runtime")
    })
}
