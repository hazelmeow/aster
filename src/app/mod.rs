pub(crate) mod event;
pub(crate) mod ui;

use crate::{
    app::event::{Event, EventHandler, app_send},
    profile::Profile,
    proto::Proto,
};
use anyhow::Context;
use iroh::{NodeAddr, protocol::Router};
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

    pub mode: AppMode,

    pub messages: Vec<String>,

    pub command_state: TextState<'a>,
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
        let router = iroh::protocol::Router::builder(endpoint)
            .accept(Proto::ALPN, Proto)
            .spawn()
            .await?;

        let events = EventHandler::new();

        Ok(Self {
            running: true,
            events,

            profile,
            router: Arc::new(router),
            proto: Proto,

            mode: AppMode::Default,

            messages: Vec::new(),

            command_state: TextState::default(),
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
    pub fn tick(&self) {}

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
            AppEvent::Log(s) => self.messages.push(s),
        }
        Ok(())
    }

    pub fn handle_command(&mut self, command: String) -> anyhow::Result<()> {
        let parts = command.split_whitespace().collect::<Vec<_>>();

        match parts[0] {
            "q" => self.events.send(AppEvent::Shutdown),

            "c" => {
                if parts.len() != 3 {
                    anyhow::bail!("expected 3 arguments");
                }

                app_log!("connecting to {}", parts[1]);

                let addr = NodeAddr::new(parts[1].parse().context("failed to parse node id")?);
                let router = self.router.clone();
                let message = parts[2].to_string();
                tokio::spawn(async move {
                    Self::connect_side(router, addr, &message).await.unwrap();
                });
            }

            _ => {
                anyhow::bail!("unknown command: {command}");
            }
        }
        Ok(())
    }

    async fn connect_side(
        router: Arc<Router>,
        addr: NodeAddr,
        message: &str,
    ) -> anyhow::Result<()> {
        app_log!("sending '{message}' to {addr:?}");

        // Open a connection to the accepting node
        let conn = router.endpoint().connect(addr, Proto::ALPN).await?;

        // Open a bidirectional QUIC stream
        let (mut send, mut recv) = conn.open_bi().await?;

        // Send some data to be echoed
        send.write_all(message.as_bytes()).await?;

        // Signal the end of data for this particular stream
        send.finish()?;

        // Receive the echo, but limit reading up to maximum 1000 bytes
        let response = recv.read_to_end(1000).await?;
        // assert_eq!(&response, b"Hello, world!");

        // Explicitly close the whole connection.
        conn.close(0u32.into(), b"bye!");

        app_log!("connection closed");

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
