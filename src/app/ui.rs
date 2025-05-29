//! The app TUI.

use crate::app::{App, AppMode, AppScreen};
use iroh::NodeId;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    symbols::border,
    text::{Line, Text},
    widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, Tabs, Widget, Wrap},
};
use tui_tree_widget::{Tree, TreeItem};
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

        let mut tree_items = Vec::new();

        // local files
        {
            let local_id = self.protocol.router.endpoint().node_id();

            let mut root_item = TreeItem::new_leaf((local_id, String::new(), false), "Local Files");

            for (path_root, files) in &self.protocol_state.local_files {
                for file in files {
                    let mut parts = std::iter::once(path_root.as_str())
                        .chain(file.split('/').filter(|p| !p.is_empty()))
                        .peekable();

                    let mut current_item = &mut root_item;
                    let mut current_path = String::new();
                    while let Some(part) = parts.next() {
                        // separate parts with /
                        if !current_path.is_empty() {
                            current_path.push('/');
                        }
                        // accumulate full path
                        current_path.push_str(part);

                        // find existing child with matching path
                        let next_idx = current_item.children().iter().position(|child| {
                            child.identifier().0 == local_id && child.identifier().1 == current_path
                        });

                        if let Some(index) = next_idx {
                            // item exists, recurse
                            current_item = current_item.child_mut(index).unwrap();
                        } else {
                            // item does not exist, create a new item
                            let is_leaf = parts.peek().is_none();
                            let new_item =
                                TreeItem::new_leaf((local_id, current_path.clone(), is_leaf), part);

                            // recurse
                            current_item.add_child(new_item).unwrap();
                            current_item = current_item
                                .child_mut(current_item.children().len() - 1)
                                .unwrap();
                        }
                    }
                }
            }

            tree_items.push(root_item);
        }

        // remote files
        for (remote_id, remote_roots) in self.protocol_state.remote_files.iter() {
            let mut root_item = TreeItem::new_leaf(
                (*remote_id, String::new(), false),
                format!("Remote Files on {remote_id}"),
            );

            for (path_root, files) in remote_roots {
                for file in files {
                    let mut parts = std::iter::once(path_root.as_str())
                        .chain(file.split('/').filter(|p| !p.is_empty()))
                        .peekable();

                    let mut current_item = &mut root_item;
                    let mut current_path = String::new();
                    while let Some(part) = parts.next() {
                        // separate parts with /
                        if !current_path.is_empty() {
                            current_path.push('/');
                        }
                        // accumulate full path
                        current_path.push_str(part);

                        // find existing child with matching path
                        let next_idx = current_item.children().iter().position(|child| {
                            child.identifier().0 == *remote_id
                                && child.identifier().1 == current_path
                        });

                        if let Some(index) = next_idx {
                            // item exists, recurse
                            current_item = current_item.child_mut(index).unwrap();
                        } else {
                            // item does not exist, create a new item
                            let is_leaf = parts.peek().is_none();
                            let new_item = TreeItem::new_leaf(
                                (*remote_id, current_path.clone(), is_leaf),
                                part,
                            );

                            // recurse
                            current_item.add_child(new_item).unwrap();
                            current_item = current_item
                                .child_mut(current_item.children().len() - 1)
                                .unwrap();
                        }
                    }
                }
            }

            tree_items.push(root_item);
        }

        // tree widget
        let tree = Tree::new(&tree_items)
            .unwrap()
            .experimental_scrollbar(Some(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_symbol(None),
            ))
            .highlight_style(Style::new().fg(Color::Black).bg(Color::LightBlue))
            .block(block);

        frame.render_stateful_widget(tree, area, &mut self.library_tree_state);
    }
}

fn shorten_id(node_id: NodeId) -> String {
    let mut s = node_id.to_string();
    s.truncate(6);
    s.push_str("..");
    s
}
