//! The app TUI.

use crate::app::{App, AppMode, AppScreen};
use iroh::NodeId;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Stylize,
    symbols::border,
    text::{Line, Text},
    widgets::{Block, Paragraph, Tabs, Widget, Wrap},
};
use tui_widgets::prompts::{Prompt, TextPrompt};

impl<'a> App<'a> {
    // we're using this instead of Widget::render because we also need the
    // frame to use TextPrompt
    pub fn render(&mut self, frame: &mut Frame) {
        use Constraint::{Length, Min};

        let [header_area, inner_area] = {
            if self.mode == AppMode::Command {
                let vertical = Layout::vertical([Length(1), Min(0), Length(3)]);
                let [header_area, inner_area, command_area] = vertical.areas(frame.area());

                self.render_command(frame, command_area);

                [header_area, inner_area]
            } else {
                let vertical = Layout::vertical([Length(1), Min(0)]);
                let [header_area, inner_area] = vertical.areas(frame.area());

                [header_area, inner_area]
            }
        };

        let horizontal = Layout::horizontal([Min(0), Length(10), Length(10)]);
        let [tabs_area, id_area, title_area] = horizontal.areas(header_area);

        // tabs
        let selected_tab_index = match self.screen {
            AppScreen::Group => 0,
            AppScreen::Library => 1,
        };
        let titles = ["Group", "Library"]
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                if i == selected_tab_index {
                    Line::from(vec![
                        "[".blue().bold(),
                        (i + 1).to_string().blue().bold(),
                        "] ".blue().bold(),
                        s.into(),
                    ])
                } else {
                    Line::from(vec![
                        "<".blue().bold(),
                        (i + 1).to_string().blue().bold(),
                        "> ".blue().bold(),
                        s.into(),
                    ])
                }
            })
            .collect::<Vec<_>>();
        Tabs::new(titles)
            .select(None)
            .padding("", "")
            .divider(" ")
            .render(tabs_area, frame.buffer_mut());

        // id
        shorten_id(self.protocol.router.endpoint().node_id())
            .yellow()
            .render(id_area, frame.buffer_mut());

        // title
        "Aster ⁂".bold().render(title_area, frame.buffer_mut());

        match self.screen {
            AppScreen::Group => {
                self.render_group_screen(frame, inner_area);
            }
            AppScreen::Library => {
                self.render_library_screen(frame, inner_area);
            }
        }
    }

    fn render_command(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().border_set(border::THICK);

        TextPrompt::from("Command")
            .with_block(block)
            .draw(frame, area, &mut self.command_state);
    }

    fn render_group_screen(&mut self, frame: &mut Frame, area: Rect) {
        let vertical = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]);
        let [status_area, log_area] = vertical.areas(area);

        self.render_status(frame, status_area);
        self.render_log(frame, log_area);
    }

    fn render_status(&self, frame: &mut Frame, area: Rect) {
        let title = Line::from(" Status ".bold());
        let instructions = Line::from(vec![
            " Command ".into(),
            "<:>".blue().bold(),
            " Quit ".into(),
            "<Q> ".blue().bold(),
        ]);
        let block = Block::bordered()
            .title(title.centered())
            .title_bottom(instructions.left_aligned())
            .border_set(border::THICK);

        let home_relay = self
            .protocol
            .router
            .endpoint()
            .home_relay()
            .get()
            .map_or("...".into(), |r| r.map_or("None".into(), |r| r.to_string()));

        let peers = self
            .protocol_state
            .peers
            .iter()
            .copied()
            .map(shorten_id)
            .collect::<Vec<_>>()
            .join(", ");

        let mut lines = vec![
            Line::from(vec![
                "Profile: ".into(),
                self.profile.name().unwrap_or("None").yellow(),
            ]),
            Line::from(vec![
                "Node ID: ".into(),
                self.protocol
                    .router
                    .endpoint()
                    .node_id()
                    .to_string()
                    .yellow(),
            ]),
            Line::from(vec!["Home Relay: ".into(), home_relay.yellow()]),
            Line::from(""),
            Line::from(vec!["Connected Peers: ".into(), peers.yellow()]),
        ];

        if let Some(group) = &self.protocol_state.group {
            let mut group_members = group
                .members
                .iter()
                .copied()
                .map(shorten_id)
                .collect::<Vec<_>>();
            group_members.sort();
            let group_members = group_members.join(", ");

            lines.extend_from_slice(&[
                Line::from(""),
                Line::from(vec!["Group ID: ".into(), group.id.to_string().yellow()]),
                Line::from(vec!["Group Members: ".into(), group_members.yellow()]),
                Line::from(""),
                Line::from(vec!["Join Code: ".into(), group.join_code.clone().yellow()]),
            ]);
        }

        let status_text = Text::from(lines);

        Paragraph::new(status_text)
            .block(block)
            .render(area, frame.buffer_mut());
    }

    fn render_log(&mut self, frame: &mut Frame, area: Rect) {
        let title = Line::from(" Log ".bold());
        let block = Block::bordered()
            .title(title.centered())
            // .title_bottom(instructions.right_aligned())
            .border_set(border::THICK);

        // let log_text = Text::from(vec![Line::from(vec![
        //     "Counter: ".into(),
        //     self.counter.to_string().yellow(),
        // ])]);
        let log_text = Text::from(
            self.messages
                .iter()
                .map(|s| Line::from(s.as_str()))
                .collect::<Vec<_>>(),
        );

        Paragraph::new(log_text)
            .wrap(Wrap { trim: false })
            .block(block)
            .render(area, frame.buffer_mut());
    }

    fn render_library_screen(&mut self, frame: &mut Frame, area: Rect) {
        let title = Line::from(" Library ".bold());
        let instructions = Line::from(vec![
            " Command ".into(),
            "<:>".blue().bold(),
            " Quit ".into(),
            "<Q> ".blue().bold(),
        ]);
        let block = Block::bordered()
            .title(title.centered())
            .title_bottom(instructions.left_aligned())
            .border_set(border::THICK);

        let lines = std::iter::once(Line::from(vec![
            "Local Files (".into(),
            self.protocol_state.local_files.len().to_string().into(),
            "):".into(),
        ]))
        .chain(
            self.protocol_state
                .local_files
                .iter()
                .map(|file_hash| Line::from(vec![" - ".into(), file_hash.to_string().into()])),
        )
        .chain(std::iter::once("".into()))
        .chain(std::iter::once(Line::from(vec![
            "Remote Files (".into(),
            self.protocol_state.remote_files.len().to_string().into(),
            "):".into(),
        ])))
        .chain(
            self.protocol_state
                .remote_files
                .iter()
                .map(|(file_hash, nodes)| {
                    let nodes = nodes
                        .iter()
                        .copied()
                        .map(shorten_id)
                        .collect::<Vec<_>>()
                        .join(", ");

                    Line::from(vec![
                        " - ".into(),
                        file_hash.to_string().into(),
                        " on ".into(),
                        nodes.into(),
                    ])
                }),
        )
        .collect::<Vec<_>>();

        Paragraph::new(lines)
            .block(block)
            .render(area, frame.buffer_mut());
    }
}

fn shorten_id(node_id: NodeId) -> String {
    let mut s = node_id.to_string();
    s.truncate(6);
    s.push_str("..");
    s
}
