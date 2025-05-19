pub(crate) mod event;
pub(crate) mod ui;

use crate::{
    app::event::{Event, EventHandler, app_send},
    profile::Profile,
    proto::{Proto, join::JoinProtocol},
};
use anyhow::Context;
use iroh::{NodeAddr, NodeId, protocol::Router};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
};
use std::sync::Arc;
use tui_widgets::prompts::{State, Status, TextState};

/// Application.
#[derive(Debug)]
pub struct App<'a> {
    pub running: bool,
    pub events: EventHandler,

    pub profile: Profile,
    pub router: Arc<Router>,
    pub proto: Proto,
    pub join_protocol: JoinProtocol,

    pub mode: AppMode,

    pub messages: Vec<String>,

    pub command_state: TextState<'a>,
    pub proto_state: ProtoState,
}

/// View of protocol state for UI.
///
/// Polled periodically since we can't lock the state mutices during rendering.
#[derive(Debug, Clone, Default)]
pub struct ProtoState {
    peers: Vec<NodeId>,
    join_code: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Default,
    Command,
}

/// Application events.
///
/// You can extend this enum with your own custom events.
#[derive(Clone, Debug)]
pub enum AppEvent {
    Shutdown,
    Exit,
    CommandMode,
    ExitMode,
    PollProtoState(ProtoState),
    Log(String),
}

macro_rules! app_log {
    ($($arg:tt)*) => {
        let _ = crate::app::event::app_send!(crate::app::AppEvent::Log(format!($($arg)*)));
    };
}
pub(crate) use app_log;

impl<'a> App<'a> {
    /// Constructs a new instance of [`App`].
    pub async fn new(profile_name: Option<String>) -> anyhow::Result<Self> {
        // load profile if named
        let profile = if let Some(profile_name) = &profile_name {
            Profile::load_or_create(profile_name).await?
        } else {
            Profile::new(None)
        };

        // create iroh endpoint
        let endpoint = iroh::Endpoint::builder()
            .discovery_n0()
            .secret_key(profile.secret_key().clone())
            .bind()
            .await?;

        // save profile if named but not previously found
        if profile_name.is_some() {
            profile.save().await?;
        }

        // spawn router accepting protocol connections
        let proto = Proto::new();
        let join_protocol = JoinProtocol::new();
        let router = iroh::protocol::Router::builder(endpoint)
            .accept(Proto::ALPN, proto.clone())
            .accept(JoinProtocol::ALPN, join_protocol.clone())
            .spawn()
            .await?;

        let events = EventHandler::new();

        Ok(Self {
            running: true,
            events,

            profile,
            router: Arc::new(router),
            proto,
            join_protocol,

            mode: AppMode::Default,

            messages: Vec::new(),

            command_state: TextState::default(),
            proto_state: ProtoState::default(),
        })
    }

    /// Run the application's main loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> anyhow::Result<()> {
        while self.running {
            terminal.draw(|frame| self.render(frame))?;
            match self.events.next().await? {
                Event::Tick => self.tick(),
                Event::Crossterm(event) => match event {
                    crossterm::event::Event::Key(key_event) => self.handle_key_events(key_event)?,
                    _ => {}
                },
                Event::App(app_event) => self
                    .handle_app_events(app_event)
                    .context("handling app event failed")?,
            }
        }

        self.router.shutdown().await?;

        Ok(())
    }

    /// Handles the key events and updates the state of [`App`].
    pub fn handle_key_events(&mut self, key_event: KeyEvent) -> anyhow::Result<()> {
        match (self.mode, key_event.code) {
            // default mode

            // : to enter command mode
            (AppMode::Default, KeyCode::Char(':')) => self.events.send(AppEvent::CommandMode),

            // esc or q to quit
            (AppMode::Default, KeyCode::Esc | KeyCode::Char('q')) => {
                self.events.send(AppEvent::Shutdown)
            }
            // ctrl+c to quit
            (AppMode::Default, KeyCode::Char('c' | 'C'))
                if key_event.modifiers == KeyModifiers::CONTROL =>
            {
                self.events.send(AppEvent::Shutdown)
            }

            (AppMode::Command, _) => {
                self.command_state.handle_key_event(key_event);

                match self.command_state.status() {
                    Status::Done => {
                        let command = self.command_state.value().to_string();

                        if let Err(e) = self.handle_command(command) {
                            app_log!("Error: {e:#}");
                        }

                        self.events.send(AppEvent::ExitMode);
                    }
                    Status::Aborted => self.events.send(AppEvent::ExitMode),
                    Status::Pending => {}
                }
            }

            _ => {}
        }
        Ok(())
    }

    /// Handles the tick event of the terminal.
    ///
    /// The tick event is where you can update the state of your application with any logic that
    /// needs to be updated at a fixed frame rate. E.g. polling a server, updating an animation.
    pub fn tick(&self) {
        let proto = self.proto.clone();
        let join_protocol = self.join_protocol.clone();
        tokio::spawn(async move {
            let peers = {
                let peers = proto.peers().lock().await;
                peers.keys().copied().collect::<Vec<_>>()
            };

            let join_code = join_protocol.get_code().await;

            let proto_state = ProtoState { peers, join_code };

            app_send!(AppEvent::PollProtoState(proto_state));
        });
    }

    pub fn handle_app_events(&mut self, app_event: AppEvent) -> anyhow::Result<()> {
        match app_event {
            AppEvent::Shutdown => self.shutdown(),
            AppEvent::Exit => self.exit(),
            AppEvent::CommandMode => {
                self.mode = AppMode::Command;
                self.command_state.focus();
            }
            AppEvent::ExitMode => {
                self.mode = AppMode::Default;
                self.command_state = TextState::default();
            }
            AppEvent::PollProtoState(state) => self.proto_state = state,
            AppEvent::Log(s) => self.messages.push(s),
        }
        Ok(())
    }

    pub fn handle_command(&mut self, command: String) -> anyhow::Result<()> {
        let parts = command.split_whitespace().collect::<Vec<_>>();

        match parts[0] {
            "q" => self.events.send(AppEvent::Shutdown),

            "c" => {
                if parts.len() != 2 {
                    anyhow::bail!("expected 1 argument");
                }

                app_log!("connecting to {}", parts[1]);

                let addr = NodeAddr::new(parts[1].parse().context("failed to parse node id")?);
                let proto = self.proto.clone();
                let router = self.router.clone();

                tokio::spawn(async move {
                    proto.connect(router.endpoint(), addr).await.unwrap();
                });
            }

            "b" => {
                if parts.len() != 2 {
                    anyhow::bail!("expected 1 argument");
                }

                app_log!("broadcasting '{}'", parts[1]);

                let proto = self.proto.clone();
                let msg = parts[1].to_string();
                tokio::spawn(async move {
                    let peers = proto.peers().clone();
                    let peers = peers.lock().await;
                    for (id, peer) in peers.iter() {
                        peer.send(crate::proto::Message::Text(msg.clone()))
                    }
                });
            }

            "j" => {
                if parts.len() != 3 {
                    anyhow::bail!("expected 2 arguments");
                }

                app_log!("joining {} with code {}", parts[1], parts[2]);

                let addr = NodeAddr::new(parts[1].parse().context("failed to parse node id")?);
                let code = parts[2].to_string();

                let join_protocol = self.join_protocol.clone();
                let router = self.router.clone();
                tokio::spawn(async move {
                    if let Err(e) = join_protocol.join(router.endpoint(), addr, code).await {
                        app_log!("join failed: {e:#}");
                    } else {
                        app_log!("join success");
                    }
                });
            }

            "d" => {
                // app_log!("app: {:?}", self);
                app_log!("proto_state: {:?}", self.proto_state);
            }

            _ => {
                anyhow::bail!("unknown command: {command}");
            }
        }
        Ok(())
    }

    /// Shut down and then exit the app.
    fn shutdown(&mut self) {
        let router = self.router.clone();
        tokio::spawn(async move {
            // shutdown router
            router.shutdown().await.unwrap();

            // send actual quit message
            app_send!(AppEvent::Exit)
        });
    }

    /// Exit the app.
    fn exit(&mut self) {
        self.running = false;
    }
}

/// Logger implementation that logs to the TUI.
pub struct AppLogger {
    filter: env_filter::Filter,
}

impl AppLogger {
    pub fn new() -> Self {
        let mut filter_builder = env_filter::Builder::new();
        if let Ok(filter) = &std::env::var("RUST_LOG") {
            filter_builder.parse(filter);
        }
        Self {
            filter: filter_builder.build(),
        }
    }
}

impl log::Log for AppLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if self.filter.matches(record) {
            let s = record.args().to_string();
            app_send!(AppEvent::Log(s));
        }
    }

    fn flush(&self) {}
}
