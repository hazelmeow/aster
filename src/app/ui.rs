//! The app TUI.

use crate::app::{App, AppMode};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Stylize,
    symbols::border,
    text::{Line, Text},
    widgets::{Block, Paragraph, Widget},
};
use tui_widgets::prompts::{Prompt, TextPrompt};

impl<'a> App<'a> {
    // we're using this instead of Widget::render because we also need the
    // frame to use TextPrompt
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        let mut layout_contraints = vec![Constraint::Percentage(50), Constraint::Percentage(50)];

        if self.mode == AppMode::Command {
            layout_contraints.push(Constraint::Length(3));
        }

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(layout_contraints)
            .split(area);

        self.render_status(frame, layout[0]);
        self.render_log(frame, layout[1]);

        if self.mode == AppMode::Command {
            self.render_command(frame, layout[2]);
        }
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
            .map(|k| k.to_string())
            .collect::<Vec<_>>()
            .join(",");

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
                .map(|k| k.to_string())
                .collect::<Vec<_>>();
            group_members.sort();
            let group_members = group_members.join(",");

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
            .block(block)
            .render(area, frame.buffer_mut());
    }

    fn render_command(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().border_set(border::THICK);

        TextPrompt::from("Command")
            .with_block(block)
            .draw(frame, area, &mut self.command_state);
    }
}
