use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use zinc_proto::AgentState;

use super::app::App;

pub fn render(frame: &mut Frame, app: &mut App) {
    let [header_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(frame, header_area, app);

    if app.agents.is_empty() {
        render_empty(frame, table_area);
    } else {
        render_table(frame, table_area, app);
    }

    render_footer(frame, footer_area, app);
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

fn render_table(frame: &mut Frame, area: Rect, app: &App) {
    let header = Row::new([
        Cell::from("STATE"),
        Cell::from("AGENT"),
        Cell::from("ID"),
        Cell::from("DIRECTORY"),
        Cell::from("UPTIME"),
        Cell::from("VIEWERS"),
    ])
    .style(Style::new().fg(Color::DarkGray))
    .bottom_margin(0);

    let rows: Vec<Row> = app
        .agents
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
        Constraint::Length(7),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::NONE))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));

    let mut table_state = TableState::default();
    if !app.agents.is_empty() {
        table_state.select(Some(app.selected));
    }

    frame.render_stateful_widget(table, area, &mut table_state);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &mut App) {
    // Show status message if active, otherwise show keybinding hints
    if let Some(msg) = app.status_message() {
        let line = Line::from(format!(" {msg}")).fg(Color::Yellow);
        frame.render_widget(line, area);
    } else {
        let line = Line::from(vec![
            Span::styled(" enter", Style::new().bold()),
            Span::raw(":attach  "),
            Span::styled("n", Style::new().bold()),
            Span::raw(":new  "),
            Span::styled("d", Style::new().bold()),
            Span::raw(":kill  "),
            Span::styled("q", Style::new().bold()),
            Span::raw(":quit"),
        ])
        .fg(Color::DarkGray);
        frame.render_widget(line, area);
    }
}

fn state_display(state: &AgentState) -> (&'static str, Color) {
    match state {
        AgentState::Working => ("● work", Color::Blue),
        AgentState::Blocked => ("▲ block", Color::Red),
        AgentState::Input => ("▲ input", Color::Yellow),
        AgentState::Idle => ("○ idle", Color::DarkGray),
    }
}

fn shorten_home(path: &str) -> String {
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
