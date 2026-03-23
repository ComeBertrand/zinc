use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, TableState,
};
use ratatui::Frame;
use zinc_proto::{AgentInfo, AgentState};

use super::app::{App, Mode, PickerState};

pub fn render(frame: &mut Frame, app: &mut App) {
    let show_filter = app.filter_active || !app.filter.is_empty();
    let filter_height = if show_filter { 1 } else { 0 };

    if app.peek.is_some() {
        let [header_area, filter_area, table_area, preview_area, footer_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(filter_height),
            Constraint::Percentage(35),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        render_header(frame, header_area, app);
        if show_filter {
            render_filter(frame, filter_area, app);
        }
        render_agents(frame, table_area, app);

        let agent_id = app.selected_agent().map(|a| a.id.as_str()).unwrap_or("—");
        let content = app.peek.as_deref().unwrap_or("");
        render_preview(frame, preview_area, agent_id, content);

        render_footer(frame, footer_area, app);
    } else {
        let [header_area, filter_area, table_area, footer_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(filter_height),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        render_header(frame, header_area, app);
        if show_filter {
            render_filter(frame, filter_area, app);
        }
        render_agents(frame, table_area, app);

        render_footer(frame, footer_area, app);
    }

    // Overlay popups
    match &app.mode {
        Mode::Normal => {}
        Mode::SpawnPickProject(picker) | Mode::SpawnPickSession { picker, .. } => {
            render_picker_popup(frame, picker);
        }
        Mode::SpawnEnterPath(path) => {
            render_enter_path_popup(frame, path);
        }
    }
}

fn render_filter(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(" /", Style::new().fg(Color::Cyan))];
    spans.push(Span::raw(&app.filter));
    if app.filter_active {
        spans.push(Span::styled("█", Style::new().fg(Color::DarkGray)));
    }
    frame.render_widget(Line::from(spans), area);
}

fn render_agents(frame: &mut Frame, area: Rect, app: &App) {
    let visible = app.visible_agents();
    if visible.is_empty() {
        if app.filter.is_empty() {
            render_empty(frame, area);
        } else {
            let text = Paragraph::new("No matching agents.")
                .alignment(Alignment::Center)
                .fg(Color::DarkGray);
            let y = area.y + area.height / 2;
            frame.render_widget(text, Rect::new(area.x, y, area.width, 1));
        }
    } else {
        render_table(frame, area, &visible, app.selected);
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let total = app.agents.len();
    let needs_input = app
        .agents
        .iter()
        .filter(|a| matches!(a.state, AgentState::Input | AgentState::Blocked))
        .count();

    let mut spans = vec![Span::styled(" zinc", Style::new().bold()), Span::raw(" — ")];

    if total == 0 {
        spans.push(Span::raw("no agents"));
    } else {
        spans.push(Span::raw(format!(
            "{total} agent{}",
            if total == 1 { "" } else { "s" }
        )));
        if needs_input > 0 {
            spans.push(Span::styled(
                format!(
                    " ({needs_input} need{} input)",
                    if needs_input == 1 { "s" } else { "" }
                ),
                Style::new().fg(Color::Yellow),
            ));
        }
    }

    frame.render_widget(Line::from(spans), area);
}

fn render_empty(frame: &mut Frame, area: Rect) {
    let text = Paragraph::new("No agents running. Press n to spawn one.")
        .alignment(Alignment::Center)
        .fg(Color::DarkGray);
    // Center vertically
    let y = area.y + area.height / 2;
    let centered = Rect::new(area.x, y, area.width, 1);
    frame.render_widget(text, centered);
}

fn render_table(frame: &mut Frame, area: Rect, agents: &[&AgentInfo], selected: usize) {
    let header = Row::new([
        Cell::from("STATE"),
        Cell::from("AGENT"),
        Cell::from("ID"),
        Cell::from("DIRECTORY"),
        Cell::from("UPTIME"),
        Cell::from("CTX"),
        Cell::from("VIEWERS"),
    ])
    .style(Style::new().fg(Color::DarkGray))
    .bottom_margin(0);

    let rows: Vec<Row> = agents
        .iter()
        .map(|agent| {
            let (icon, color) = state_display(&agent.state);
            let dir = shorten_home(&agent.dir.display().to_string());
            Row::new([
                Cell::from(Span::styled(icon, Style::new().fg(color))),
                Cell::from(agent.provider.as_str()),
                Cell::from(agent.id.as_str()),
                Cell::from(dir),
                Cell::from(format_uptime(agent.uptime_secs)),
                Cell::from(context_display(agent.context_percent)),
                Cell::from(format!("{}", agent.viewers)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Length(15),
        Constraint::Fill(1),
        Constraint::Length(8),
        Constraint::Length(6),
        Constraint::Length(7),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::NONE))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));

    let mut table_state = TableState::default();
    if !agents.is_empty() {
        table_state.select(Some(selected));
    }

    frame.render_stateful_widget(table, area, &mut table_state);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &mut App) {
    // Show status message if active, otherwise show keybinding hints
    if let Some(msg) = app.status_message() {
        let line = Line::from(format!(" {msg}")).fg(Color::Yellow);
        frame.render_widget(line, area);
    } else {
        let hints = match app.mode {
            Mode::Normal => {
                let mut hints = vec![
                    Span::styled(" enter", Style::new().bold()),
                    Span::raw(":attach  "),
                    Span::styled("n", Style::new().bold()),
                    Span::raw(":new  "),
                    Span::styled("/", Style::new().bold()),
                    Span::raw(":filter  "),
                    Span::styled("p", Style::new().bold()),
                    Span::raw(":peek  "),
                ];
                for cmd in &app.commands {
                    hints.push(Span::styled(cmd.key.clone(), Style::new().bold()));
                    hints.push(Span::raw(format!(":{} ", cmd.name)));
                }
                hints.extend([
                    Span::styled("d", Style::new().bold()),
                    Span::raw(":kill  "),
                    Span::styled("q", Style::new().bold()),
                    Span::raw(":quit"),
                ]);
                hints
            }
            _ => vec![
                Span::styled(" enter", Style::new().bold()),
                Span::raw(":select  "),
                Span::styled("esc", Style::new().bold()),
                Span::raw(":cancel"),
            ],
        };
        frame.render_widget(Line::from(hints).fg(Color::DarkGray), area);
    }
}

fn render_preview(frame: &mut Frame, area: Rect, agent_id: &str, raw_content: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::DarkGray))
        .title(format!(" {agent_id} "));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Feed raw content through vt100 to get a properly rendered screen
    let mut parser = vt100::Parser::new(inner.height, inner.width, 0);
    parser.process(raw_content.as_bytes());
    let screen = parser.screen();

    let lines: Vec<Line> = (0..inner.height)
        .map(|row| {
            let mut spans = Vec::new();
            for col in 0..inner.width {
                let cell = screen.cell(row, col).unwrap();
                if cell.is_wide_continuation() {
                    continue;
                }
                let contents = cell.contents();
                let style = vt100_style_to_ratatui(cell);
                if contents.is_empty() {
                    spans.push(Span::styled(" ", style));
                } else {
                    spans.push(Span::styled(contents.to_string(), style));
                }
            }
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Convert vt100 cell attributes to a ratatui Style.
fn vt100_style_to_ratatui(cell: &vt100::Cell) -> Style {
    let mut style = Style::new();
    style = style.fg(vt100_color_to_ratatui(cell.fgcolor()));
    style = style.bg(vt100_color_to_ratatui(cell.bgcolor()));
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

fn vt100_color_to_ratatui(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn render_picker_popup(frame: &mut Frame, picker: &PickerState) {
    let area = centered_rect(60, 60, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Cyan))
        .title(format!(" {} ", picker.title));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [filter_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);

    // Filter input
    let filter_line = Line::from(vec![
        Span::styled("> ", Style::new().fg(Color::Cyan)),
        Span::raw(&picker.filter),
        Span::styled("█", Style::new().fg(Color::DarkGray)),
    ]);
    frame.render_widget(filter_line, filter_area);

    // Filtered items list
    let filtered = picker.filtered_items();
    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let style = if i == picker.selected {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            ListItem::new(item.display.as_str()).style(style)
        })
        .collect();

    frame.render_widget(List::new(items), list_area);
}

fn render_enter_path_popup(frame: &mut Frame, path: &str) {
    let area = centered_rect(60, 20, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Cyan))
        .title(" Enter path ");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Center the input vertically in the popup
    let y = inner.y + inner.height / 2;
    let input_area = Rect::new(inner.x, y, inner.width, 1);

    let input_line = Line::from(vec![
        Span::styled("> ", Style::new().fg(Color::Cyan)),
        Span::raw(path),
        Span::styled("█", Style::new().fg(Color::DarkGray)),
    ]);
    frame.render_widget(input_line, input_area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [_, center_v, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .areas(area);
    let [_, center, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(center_v);
    center
}

fn state_display(state: &AgentState) -> (&'static str, Color) {
    match state {
        AgentState::Working => ("● work", Color::Blue),
        AgentState::Blocked => ("▲ block", Color::Red),
        AgentState::Input => ("▲ input", Color::Yellow),
        AgentState::Idle => ("○ idle", Color::DarkGray),
    }
}

fn context_display(pct: Option<u8>) -> Span<'static> {
    match pct {
        Some(p) => {
            let color = if p >= 90 {
                Color::Red
            } else if p >= 70 {
                Color::Yellow
            } else {
                Color::Green
            };
            Span::styled(format!("{p}%"), Style::new().fg(color))
        }
        None => Span::raw(""),
    }
}

pub fn shorten_home(path: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = path.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}
