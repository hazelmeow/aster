pub(crate) mod event;
pub(crate) mod ui;

use crate::{
    app::event::{Event, EventHandler, app_send},
    profile::Profile,
    proto::{Protocol, ProtocolState, join::JoinProtocol},
};
use anyhow::Context;
use base64::{Engine, prelude::BASE64_STANDARD_NO_PAD};
use iroh::{NodeAddr, NodeId, protocol::Router};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
};
use std::{collections::HashSet, sync::Arc};
use tui_widgets::prompts::{State, Status, TextState};

/// Application.
#[derive(Debug)]
pub struct App<'a> {
    pub running: bool,
    pub events: EventHandler,

    pub profile: Profile,
    pub protocol: Arc<Protocol>,

    pub mode: AppMode,
    pub screen: AppScreen,

    pub messages: Vec<String>,

    pub command_state: TextState<'a>,
    pub protocol_state: ProtocolState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppScreen {
    #[default]
    Group,
    Library,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppMode {
    #[default]
    Default,
    Command,
}

/// Application events.
///
/// You can extend this enum with your own custom events.
#[derive(Clone, Debug)]
pub enum AppEvent {
    Log(String),

    Exit,

    CommandMode,
    ExitMode,

    Screen(AppScreen),

    PollProtocolState(ProtocolState),
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

        // TODO
        // save profile if named but not previously found
        // if profile_name.is_some() {
        // profile.save().await?;
        // }

        let protocol = Protocol::new(&profile).await?;

        let events = EventHandler::new();

        Ok(Self {
            running: true,
            events,

            profile,
            protocol,

            mode: AppMode::default(),
            screen: AppScreen::default(),

            messages: Vec::new(),

            command_state: TextState::default(),
            protocol_state: ProtocolState::default(),
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

        // shut down protocol
        self.protocol.shutdown().await?;

        Ok(())
    }

    /// Handles the key events and updates the state of [`App`].
    pub fn handle_key_events(&mut self, key_event: KeyEvent) -> anyhow::Result<()> {
        match (self.mode, key_event.code) {
            // default mode

            // : or / to enter command mode
            (AppMode::Default, KeyCode::Char(':') | KeyCode::Char('/')) => {
                self.events.send(AppEvent::CommandMode)
            }

            // change screens
            (AppMode::Default, KeyCode::Char('1')) => {
                self.events.send(AppEvent::Screen(AppScreen::Group))
            }
            (AppMode::Default, KeyCode::Char('2')) => {
                self.events.send(AppEvent::Screen(AppScreen::Library))
            }

            // esc or q to quit
            (AppMode::Default, KeyCode::Esc | KeyCode::Char('q')) => {
                self.events.send(AppEvent::Exit)
            }
            // ctrl+c to quit
            (AppMode::Default, KeyCode::Char('c' | 'C'))
                if key_event.modifiers == KeyModifiers::CONTROL =>
            {
                self.events.send(AppEvent::Exit)
            }

            // command mode
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
        let protocol = self.protocol.clone();
        tokio::spawn(async move {
            match protocol.poll_state().await {
                Ok(state) => {
                    app_send!(AppEvent::PollProtocolState(state));
                }
                Err(e) => {
                    app_log!("error polling protocol state: {e:#}");
                }
            }
        });
    }

    pub fn handle_app_events(&mut self, app_event: AppEvent) -> anyhow::Result<()> {
        match app_event {
            AppEvent::Log(s) => self.messages.push(s),

            AppEvent::Exit => self.exit(),

            AppEvent::CommandMode => {
                self.mode = AppMode::Command;
                self.command_state.focus();
            }
            AppEvent::ExitMode => {
                self.mode = AppMode::Default;
                self.command_state = TextState::default();
            }

            AppEvent::Screen(screen) => {
                self.screen = screen;
            }

            AppEvent::PollProtocolState(state) => self.protocol_state = state,
        }
        Ok(())
    }

    pub fn handle_command(&mut self, command: String) -> anyhow::Result<()> {
        let parts = command.split_whitespace().collect::<Vec<_>>();

        match parts[0] {
            "q" => self.events.send(AppEvent::Exit),

            // "c" => {
            //     if parts.len() != 2 {
            //         anyhow::bail!("expected 1 argument");
            //     }

            //     app_log!("connecting to {}", parts[1]);

            //     let addr = NodeAddr::new(parts[1].parse().context("failed to parse node id")?);
            //     let proto = self.proto.clone();
            //     let router = self.router.clone();

            //     tokio::spawn(async move {
            //         proto.connect(router.endpoint(), addr).await.unwrap();
            //     });
            // }

            // "b" => {
            //     if parts.len() != 2 {
            //         anyhow::bail!("expected 1 argument");
            //     }

            //     app_log!("broadcasting '{}'", parts[1]);

            //     let proto = self.proto.clone();
            //     let msg = parts[1].to_string();
            //     tokio::spawn(async move {
            //         let peers = proto.peers().clone();
            //         let peers = peers.lock().await;
            //         for (id, peer) in peers.iter() {
            //             peer.send(crate::proto::Message::Text(msg.clone()))
            //         }
            //     });
            // }
            "cg" => {
                app_log!("creating group");

                let protocol = self.protocol.clone();
                tokio::spawn(async move {
                    if let Err(e) = protocol.create_group().await {
                        app_log!("create group failed: {e:#}");
                    } else {
                        app_log!("create group success");
                    }
                });
            }

            "j" => {
                if parts.len() != 2 {
                    anyhow::bail!("expected 1 argument");
                }

                let code_bytes = BASE64_STANDARD_NO_PAD
                    .decode(parts[1])
                    .context("failed to decode join code")?;
                if code_bytes.len() != 40 {
                    anyhow::bail!("wrong length for join code");
                }

                let node_id = NodeId::from_bytes(code_bytes[0..32].try_into().unwrap())
                    .context("failed to parse join code")?;
                let node_addr = NodeAddr::new(node_id);

                let code = u64::from_be_bytes(code_bytes[32..40].try_into().unwrap());

                app_log!("joining {} with code {}", node_id, code);

                let protocol = self.protocol.clone();
                tokio::spawn(async move {
                    if let Err(e) = protocol.join_node(node_addr, code).await {
                        app_log!("join failed: {e:#}");
                    } else {
                        app_log!("join success");
                    }
                });
            }

            "r" | "remove" => {
                if parts.len() != 2 {
                    anyhow::bail!("expected 1 argument");
                }

                let node_id = parts[1].parse().context("failed to parse node id")?;

                app_log!("removing {node_id}");

                let protocol = self.protocol.clone();
                tokio::spawn(async move {
                    if let Err(e) = protocol.remove_group_member(node_id).await {
                        app_log!("remove failed: {e:#}");
                    } else {
                        app_log!("remove success");
                    }
                });
            }

            "d" => {
                // app_log!("app: {:?}", self);
                // app_log!("proto_state: {:?}", self.protocol_state);
                app_log!(
                    "protocol state group members: {:?}",
                    self.protocol_state.group.as_ref().map(|g| &g.members)
                );

                let protocol = self.protocol.clone();
                tokio::spawn(async move {
                    let group = protocol.group.lock().await;
                    let Some(group) = group.as_ref() else {
                        app_log!("not in group");
                        return;
                    };

                    app_log!("mutex members: {:?}", group.evaluate_members());
                });
            }

            "p" => {
                if parts.len() != 2 {
                    anyhow::bail!("expected 1 argument");
                }

                let file_hash = parts[1].parse().context("failed to parse file hash")?;

                app_log!("attempting to download {file_hash}");

                let protocol = self.protocol.clone();
                tokio::spawn(async move {
                    if let Err(e) = protocol.download_file(file_hash).await {
                        app_log!("download failed: {e:#}");
                    } else {
                        app_log!("download success");
                    }
                });
            }

            _ => {
                anyhow::bail!("unknown command: {command}");
            }
        }
        Ok(())
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
