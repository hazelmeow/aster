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
            let show_audio = self.audio_state.is_some();
            let show_command = self.mode == AppMode::Command;

            let mut constraints = vec![Length(1), Min(0)];
            if show_audio {
                constraints.push(Length(4));
            }
            if show_command {
                constraints.push(Length(3))
            }

            let vertical = Layout::vertical(constraints);
            let areas = vertical.split(frame.area());

            let header_area = areas[0];
            let inner_area = areas[1];

            let mut next_area = 2;
            if show_audio {
                let audio_area = areas[next_area];
                next_area += 1;

                self.render_audio(frame, audio_area);
            }
            if show_command {
                let command_area = areas[next_area];

                self.render_command(frame, command_area);
            }

            [header_area, inner_area]
        };

        let horizontal = Layout::horizontal([Min(0), Length(10), Length(10)]);
        let [tabs_area, id_area, title_area] = horizontal.areas(header_area);

        // tabs
        let selected_tab_index = match self.screen {
            AppScreen::Group => 0,
            AppScreen::Library => 1,
            AppScreen::Help => 2,
        };
        let titles = ["Group", "Library", "Help"]
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let key = if s == "Help" {
                    "?".blue().bold()
                } else {
                    (i + 1).to_string().blue().bold()
                };

                if i == selected_tab_index {
                    Line::from(vec!["[".blue().bold(), key, "] ".blue().bold(), s.into()])
                } else {
                    Line::from(vec!["<".blue().bold(), key, "> ".blue().bold(), s.into()])
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
            AppScreen::Help => {
                self.render_help_screen(frame, inner_area);
            }
        }
    }

    fn render_audio(&mut self, frame: &mut Frame, area: Rect) {
        let state = self.audio_state.as_ref().unwrap();

        let instructions = Line::from(vec![
            " Pause ".into(),
            "<p>".blue().bold(),
            " Stop ".into(),
            "<s>".blue().bold(),
            " Seek ".into(),
            "<shift + ←/→>".blue().bold(),
        ]);
        let block = Block::bordered()
            .title_top(instructions.right_aligned())
            .border_set(border::THICK);

        let status_line = {
            let icon = if state.paused { '⏸' } else { '⏵' };

            let elapsed_mins = (state.elapsed / 60.0).floor() as usize;
            let elapsed_secs = (state.elapsed % 60.0).floor() as usize;
            let elapsed = format!("{elapsed_mins}:{elapsed_secs:02}");

            let duration_mins = (state.duration / 60.0).floor() as usize;
            let duration_secs = (state.duration % 60.0).floor() as usize;
            let duration = format!("{duration_mins}:{duration_secs:02}");

            let source = if state.peer == self.protocol.router.endpoint().node_id() {
                String::from("local library")
            } else {
                format!("streaming from {}", shorten_id(state.peer))
            };

            Line::from(vec![
                " ".into(),
                icon.to_string().blue(),
                " ".into(),
                elapsed.into(),
                " / ".green(),
                duration.into(),
                " ".into(),
                state.label.clone().into(),
                " (".green(),
                source.green(),
                ")".green(),
            ])
        };

        let progress_line = {
            let width = area.width as usize - 6;
            let percent = state.elapsed / state.duration;

            let left_cap = String::from('┣').blue();

            let left_n = ((width as f64 * percent).floor() as usize).min(width);
            let left = std::iter::repeat_n('━', left_n).collect::<String>().blue();

            let right_n = width - left_n;
            let right = std::iter::repeat_n('━', right_n)
                .collect::<String>()
                .white();

            let right_cap = if left_n == width {
                String::from('┫').blue()
            } else {
                String::from('┫').white()
            };

            Line::from(vec![" ".into(), left_cap, left, right, right_cap])
        };

        Paragraph::new(vec![status_line, progress_line])
            .block(block)
            .render(area, frame.buffer_mut());
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
            "<q> ".blue().bold(),
        ]);
        let block = Block::bordered()
            .title(title.centered())
            .title_top(instructions.right_aligned())
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
            .border_set(border::THICK);

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
        let top_instructions = Line::from(vec![
            " Command ".into(),
            "<:>".blue().bold(),
            " Quit ".into(),
            "<q> ".blue().bold(),
        ]);
        let bottom_instructions = Line::from(vec![
            " Navigate ".into(),
            "<↑ ↓ space>".blue().bold(),
            //
        ]);
        let block = Block::bordered()
            .title(title.centered())
            .title_top(top_instructions.right_aligned())
            .title_bottom(bottom_instructions.right_aligned())
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
                    .track_symbol(None)
                    .style(Color::Blue),
            ))
            .highlight_style(Style::new().fg(Color::Black).bg(Color::LightBlue))
            .node_closed_symbol("⏵ ")
            .node_open_symbol("⏷ ")
            .block(block);

        frame.render_stateful_widget(tree, area, &mut self.library_tree_state);
    }

    fn render_help_screen(&mut self, frame: &mut Frame, area: Rect) {
        let title = Line::from(" Help ".bold());
        let instructions = Line::from(vec![
            " Command ".into(),
            "<:>".blue().bold(),
            " Quit ".into(),
            "<q> ".blue().bold(),
        ]);
        let block = Block::bordered()
            .title(title.centered())
            .title_top(instructions.right_aligned())
            .border_set(border::THICK);

        let lines = vec![
            Line::from("Navigation".bold()),
            Line::from(vec![
                " - ".into(),
                "<1>".blue(),
                " and ".into(),
                "<2>".blue(),
                " to change screens.".into(),
            ]),
            Line::from(vec![
                " - ".into(),
                "<:>".blue(),
                " to open the command prompt.".into(),
            ]),
            Line::from(vec![
                " - ".into(),
                "<q>".blue(),
                " or ".into(),
                "<ctrl + c>".blue(),
                " to quit.".into(),
            ]),
            Line::from(""),
            Line::from("Group Management".bold()),
            Line::from(vec![
                " - ".into(),
                "`:cg`".blue(),
                " to create a group.".into(),
            ]),
            Line::from(vec![
                " - ".into(),
                "`:join <join code>`".blue(),
                " to join a group.".into(),
            ]),
            Line::from(""),
            Line::from("Library Screen".bold()),
            Line::from(vec![
                " - ".into(),
                "<↑> <↓> <PgUp> <PgDn> <Home> <End> <space>".blue(),
                " to navigate.".into(),
            ]),
            Line::from(""),
            Line::from("Playback".bold()),
            Line::from(vec![" - ".into(), "<p>".blue(), " to pause.".into()]),
            Line::from(vec![" - ".into(), "<s>".blue(), " to stop.".into()]),
            Line::from(vec![
                " - ".into(),
                "<shift + ←> <shift + →>".blue(),
                " to seek.".into(),
            ]),
        ];

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
