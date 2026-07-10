mod views;

use super::app::{Modal, TuiApp, View};
use crate::config::ConfigKey;
use crate::runtime::{
    CommandPhase, CommandRecord, Health, LogLevel, PipelineEdge, PipelineEdgeSnapshot,
    RuntimeSnapshot, UploadPhase,
};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table, Tabs, Wrap};
use std::time::{Duration, Instant};
use views::*;

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut TuiApp, snapshot: &RuntimeSnapshot) {
    let area = frame.area();
    if area.width < 52 || area.height < 16 {
        draw_compact(frame, area, snapshot);
        draw_modal(frame, area, app.modal);
        draw_setting_editor(frame, area, app);
        return;
    }

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(area);

    draw_header(frame, vertical[0], snapshot);
    draw_tabs(frame, vertical[1], app);
    match app.view {
        View::Overview => draw_overview(frame, vertical[2], snapshot),
        View::Pipeline => draw_pipeline(frame, vertical[2], app, snapshot),
        View::Game => draw_game(frame, vertical[2], app, snapshot),
        View::Connections => draw_connections(frame, vertical[2], app, snapshot),
        View::Replay => draw_replay(frame, vertical[2], app, snapshot),
        View::Metrics => draw_metrics(frame, vertical[2], app, snapshot),
        View::Logs => draw_logs(frame, vertical[2], app, snapshot),
        View::Settings => draw_settings(frame, vertical[2], app, snapshot),
    }
    draw_footer(frame, vertical[3], app, snapshot);
    draw_modal(frame, area, app.modal);
    draw_setting_editor(frame, area, app);
}

fn draw_compact(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let main = &snapshot.status.main;
    let pipeline_backlog = PipelineEdge::ALL
        .iter()
        .map(|edge| snapshot.pipeline.edge(*edge).queue_depth)
        .sum::<usize>();
    let text = vec![
        Line::from(vec![
            Span::styled(
                "xemu-tools-rs ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                health_label(overall_health(snapshot)),
                health_style(overall_health(snapshot)),
            ),
        ]),
        Line::from(format!(
            "uptime {}  tick {}  map {}",
            format_duration(snapshot.uptime),
            main.game_time,
            empty_dash(&main.map_name)
        )),
        Line::from(format!(
            "game {}  status {}  players {}  events {}",
            empty_dash(&main.game_id),
            empty_dash(&main.game_status),
            main.player_count,
            main.event_count
        )),
        Line::from(format!(
            "loop {:.2}ms  extract {:.2}ms  dropped {}  backlog {}",
            main.loop_ms, main.game_info_ms, main.dropped_ticks_total, pipeline_backlog
        )),
        Line::from("Terminal is too small for the full dashboard."),
        Line::from("Resize to at least 52x16.  q quit  ? help"),
    ];
    frame.render_widget(Paragraph::new(text).block(block("Runtime")), area);
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let main = &snapshot.status.main;
    let overall = overall_health(snapshot);
    let text = vec![
        Line::from(vec![
            Span::styled(
                "xemu-tools-rs",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(health_label(overall), health_style(overall)),
            Span::raw(format!(
                "  uptime {}  tick {}  map {}",
                format_duration(snapshot.uptime),
                main.game_time,
                empty_dash(&main.map_name)
            )),
        ]),
        Line::from(format!(
            "game {}  status {}  players {}  events {}  last tick {}",
            empty_dash(&main.game_id),
            empty_dash(&main.game_status),
            main.player_count,
            main.event_count,
            main.last_tick_at
                .map(elapsed)
                .unwrap_or_else(|| "-".to_string())
        )),
    ];
    frame.render_widget(Paragraph::new(text).block(block("Runtime")), area);
}

fn draw_tabs(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let compact = area.width < 110;
    let titles = View::ALL
        .iter()
        .enumerate()
        .map(|(index, view)| {
            let label = if compact {
                match view {
                    View::Overview => "Ovr",
                    View::Pipeline => "Pipe",
                    View::Game => "Game",
                    View::Connections => "Conn",
                    View::Replay => "Replay",
                    View::Metrics => "Metric",
                    View::Logs => "Logs",
                    View::Settings => "Set",
                }
            } else {
                view.label()
            };
            Line::from(format!("{} {label}", index + 1))
        })
        .collect::<Vec<_>>();
    let tabs = Tabs::new(titles)
        .block(block("Views"))
        .select(app.selected_tab())
        .divider(" | ")
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, area);
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &TuiApp, snapshot: &RuntimeSnapshot) {
    let mut spans = vec![
        key("<- ->"),
        Span::raw(" views  "),
        key("r"),
        Span::raw(" relay  "),
        key("x"),
        Span::raw(" xemu  "),
        key("m"),
        Span::raw(" QMP  "),
    ];
    match app.view {
        View::Replay => spans.extend([
            key("s"),
            Span::raw(" replay  "),
            key("t"),
            Span::raw(" ticks  "),
            key("u"),
            Span::raw(" uploads  "),
            key("D"),
            Span::raw(" discard  "),
        ]),
        View::Settings => spans.extend([
            key("Enter"),
            Span::raw(" edit  "),
            key("R"),
            Span::raw(" reload  "),
        ]),
        View::Connections => spans.extend([
            key("up/down"),
            Span::raw(" client  "),
            key("d"),
            Span::raw(" disconnect  "),
        ]),
        View::Game => spans.extend([
            key("up/down"),
            Span::raw(" player  "),
            key("g"),
            Span::raw(" JSON  "),
            key("f"),
            Span::raw(" refresh  "),
            key("/"),
            Span::raw(" filter  "),
        ]),
        View::Metrics => spans.extend([key("c"), Span::raw(" clear history  ")]),
        View::Logs => spans.extend([
            key("f"),
            Span::raw(" filter  "),
            key("up/down"),
            Span::raw(" scroll  "),
            key("End"),
            Span::raw(" follow  "),
            key("v"),
            Span::raw(" level  "),
        ]),
        _ => {}
    }
    spans.extend([key("q"), Span::raw(" quit  "), key("?"), Span::raw(" help")]);
    let latest = snapshot
        .commands
        .last()
        .map(command_summary)
        .unwrap_or_else(|| "no commands".to_string());
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(block(&format!(
            "Controls | {}",
            shorten(&latest, area.width.saturating_sub(20) as usize)
        ))),
        area,
    );
}

fn draw_modal(frame: &mut Frame<'_>, area: Rect, modal: Option<Modal>) {
    let Some(modal) = modal else {
        return;
    };
    let (modal_area, title, lines) = match modal {
        Modal::Help => (
            centered_rect(area, 72, 17),
            "Help",
            vec![
                Line::from("Left/Right, h/l, Tab     change view"),
                Line::from("1-8                      open a view"),
                Line::from("r / x / m                reconnect relay / xemu / QMP"),
                Line::from("s / t / u                toggle replay / ticks / uploads"),
                Line::from("f                        edit log filter"),
                Line::from("Up/Down, PgUp/PgDn       scroll logs"),
                Line::from("Home/End                 oldest / follow newest logs"),
                Line::from("v                        cycle log severity"),
                Line::from("Enter/e                  edit selected setting"),
                Line::from("p/d                      retry/cancel selected upload"),
                Line::from("c                        clear logs"),
                Line::from("q                        confirm shutdown"),
                Line::from("Ctrl+C                   immediate shutdown request"),
                Line::from("Esc, Enter, or ?         close this help"),
            ],
        ),
        Modal::Quit => (
            centered_rect(area, 52, 7),
            "Confirm Shutdown",
            vec![
                Line::from("Stop xemu-tools-rs and leave the terminal UI?"),
                Line::from(""),
                Line::from(vec![
                    key("y / Enter"),
                    Span::raw(" confirm    "),
                    key("n / Esc"),
                    Span::raw(" cancel"),
                ]),
            ],
        ),
        Modal::DiscardReplay => (
            centered_rect(area, 58, 7),
            "Discard In-progress Replay",
            vec![
                Line::from("Delete the active tick spool and reset recording state?"),
                Line::from(""),
                Line::from(vec![
                    key("y / Enter"),
                    Span::raw(" discard    "),
                    key("n / Esc"),
                    Span::raw(" cancel"),
                ]),
            ],
        ),
    };
    frame.render_widget(Clear, modal_area);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .block(block(title))
            .wrap(Wrap { trim: false }),
        modal_area,
    );
}

fn draw_setting_editor(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    if !app.editing_setting {
        return;
    }
    let key = ConfigKey::ALL[app.selected_setting.min(ConfigKey::ALL.len() - 1)];
    let editor_area = centered_rect(area, 70, 7);
    frame.render_widget(Clear, editor_area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(app.setting_buffer.clone()),
            Line::from(""),
            Line::from("Enter save and apply   Esc cancel"),
        ])
        .block(block(&format!("Edit {}", key.label()))),
        editor_area,
    );
}

fn centered_rect(area: Rect, width_percent: u16, requested_height: u16) -> Rect {
    let width = area
        .width
        .saturating_mul(width_percent)
        .saturating_div(100)
        .max(20);
    let width = width.min(area.width.saturating_sub(2));
    let height = requested_height.min(area.height.saturating_sub(2)).max(3);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn component_row(key: &'static str, health: Health, detail: Option<String>) -> Row<'static> {
    Row::new(vec![
        Cell::from(key),
        Cell::from(health_label(health)).style(health_style(health)),
        Cell::from(detail.unwrap_or_else(|| "ok".to_string())),
    ])
}

fn connection_row(
    key: &'static str,
    health: Health,
    endpoint: String,
    detail: String,
    changed: Option<Instant>,
    error: &Option<String>,
) -> Row<'static> {
    Row::new(vec![
        Cell::from(key),
        Cell::from(health_label(health)).style(health_style(health)),
        Cell::from(endpoint),
        Cell::from(detail),
        Cell::from(changed.map(elapsed).unwrap_or_else(|| "-".to_string())),
        Cell::from(last_error(error)).style(if error.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::DarkGray)
        }),
    ])
}

fn command_row(command: &CommandRecord) -> Row<'static> {
    Row::new(vec![
        Cell::from(format!("#{}", command.id)),
        Cell::from(command_phase_label(command.phase)).style(command_phase_style(command.phase)),
        Cell::from(command.command.label()),
        Cell::from(command.detail.clone()),
        Cell::from(format!(
            "{} old",
            format_duration(command.requested_at.elapsed())
        )),
    ])
}

fn command_summary(command: &CommandRecord) -> String {
    format!(
        "#{} {} {}: {}",
        command.id,
        command_phase_label(command.phase),
        command.command.label(),
        command.detail
    )
}

fn pipeline_line(
    label: &'static str,
    metrics: PipelineEdgeSnapshot,
    target: &'static str,
) -> Line<'static> {
    Line::from(vec![
        Span::raw("  +-> "),
        Span::styled(format!("{label:<9}"), Style::default().fg(Color::Cyan)),
        Span::styled(
            format!("[queue {:>4}]", metrics.queue_depth),
            queue_style(metrics.queue_depth),
        ),
        Span::raw(format!(" -> {target}")),
    ])
}

fn node(value: &'static str) -> Span<'static> {
    Span::styled(
        format!("[{value}]"),
        Style::default().fg(Color::Black).bg(Color::Cyan),
    )
}

fn kv_table<'a>(title: &'a str, rows: Vec<Row<'a>>, key_width: u16) -> Table<'a> {
    Table::new(rows, [Constraint::Length(key_width), Constraint::Min(12)])
        .block(block(title))
        .column_spacing(1)
}

fn kv_row<'a>(key: &'static str, value: impl Into<String>) -> Row<'a> {
    Row::new(vec![
        Cell::from(key).style(Style::default().fg(Color::DarkGray)),
        Cell::from(value.into()),
    ])
}

fn kv_bool_row<'a>(key: &'static str, value: bool) -> Row<'a> {
    Row::new(vec![
        Cell::from(key).style(Style::default().fg(Color::DarkGray)),
        Cell::from(if value { "on" } else { "off" }).style(bool_style(value)),
    ])
}

fn kv_status_row<'a>(key: &'static str, health: Health, value: impl Into<String>) -> Row<'a> {
    Row::new(vec![
        Cell::from(key).style(Style::default().fg(Color::DarkGray)),
        Cell::from(format!("{} {}", health_label(health), value.into()))
            .style(health_style(health)),
    ])
}

fn table_header<const N: usize>(values: [&'static str; N]) -> Row<'static> {
    Row::new(values.map(Cell::from)).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

fn block(title: &str) -> Block<'_> {
    Block::default().title(title).borders(Borders::ALL)
}

fn key(value: &'static str) -> Span<'static> {
    Span::styled(
        value,
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

fn overall_health(snapshot: &RuntimeSnapshot) -> Health {
    let status = &snapshot.status;
    let health = [
        status.main.health,
        status.xemu.health,
        status.qmp.health,
        status.local_ws.health,
        status.relay.health,
        status.replay.health,
    ];
    if health.contains(&Health::Error) {
        Health::Error
    } else if health.contains(&Health::Starting) {
        Health::Starting
    } else if status.main.health == Health::Running {
        Health::Running
    } else if health.contains(&Health::Disconnected) {
        Health::Disconnected
    } else {
        Health::Unknown
    }
}

fn edge_health(edge: PipelineEdge, snapshot: &RuntimeSnapshot) -> Health {
    match edge {
        PipelineEdge::Replay => snapshot.status.replay.health,
        PipelineEdge::LocalWebSocket => snapshot.status.local_ws.health,
        PipelineEdge::Relay => snapshot.status.relay.health,
    }
}

fn health_label(health: Health) -> &'static str {
    match health {
        Health::Unknown => "unknown",
        Health::Starting => "starting",
        Health::Running => "running",
        Health::Connected => "connected",
        Health::Disconnected => "disconnected",
        Health::Disabled => "disabled",
        Health::Error => "error",
    }
}

fn health_style(health: Health) -> Style {
    let color = match health {
        Health::Connected | Health::Running => Color::Green,
        Health::Starting => Color::Yellow,
        Health::Disconnected | Health::Unknown | Health::Disabled => Color::DarkGray,
        Health::Error => Color::Red,
    };
    Style::default().fg(color)
}

fn command_phase_label(phase: CommandPhase) -> &'static str {
    match phase {
        CommandPhase::Queued => "queued",
        CommandPhase::Running => "running",
        CommandPhase::Succeeded => "succeeded",
        CommandPhase::Failed => "failed",
    }
}

fn command_phase_style(phase: CommandPhase) -> Style {
    let color = match phase {
        CommandPhase::Queued => Color::DarkGray,
        CommandPhase::Running => Color::Yellow,
        CommandPhase::Succeeded => Color::Green,
        CommandPhase::Failed => Color::Red,
    };
    Style::default().fg(color)
}

fn upload_phase_label(phase: UploadPhase) -> &'static str {
    match phase {
        UploadPhase::WaitingForUrl => "waiting URL",
        UploadPhase::Uploading => "uploading",
        UploadPhase::Retrying => "retrying",
        UploadPhase::Uploaded => "uploaded",
        UploadPhase::Failed => "failed",
        UploadPhase::Cancelled => "cancelled",
    }
}

fn upload_phase_style(phase: UploadPhase) -> Style {
    let color = match phase {
        UploadPhase::WaitingForUrl => Color::DarkGray,
        UploadPhase::Uploading | UploadPhase::Retrying => Color::Yellow,
        UploadPhase::Uploaded => Color::Green,
        UploadPhase::Failed => Color::Red,
        UploadPhase::Cancelled => Color::DarkGray,
    };
    Style::default().fg(color)
}

fn log_level_label(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "info",
        LogLevel::Warning => "warn",
        LogLevel::Error => "error",
    }
}

fn log_level_style(level: LogLevel) -> Style {
    Style::default().fg(match level {
        LogLevel::Info => Color::DarkGray,
        LogLevel::Warning => Color::Yellow,
        LogLevel::Error => Color::Red,
    })
}

fn bool_style(value: bool) -> Style {
    Style::default().fg(if value { Color::Green } else { Color::Yellow })
}

fn queue_style(depth: usize) -> Style {
    Style::default().fg(if depth == 0 {
        Color::Green
    } else if depth < 10 {
        Color::Yellow
    } else {
        Color::Red
    })
}

fn count_style(count: u64) -> Style {
    Style::default().fg(if count == 0 {
        Color::DarkGray
    } else {
        Color::Red
    })
}

fn source_color(source: &str) -> Color {
    match source {
        "main" | "metrics" | "perf" => Color::Cyan,
        "xemu" | "qmp" => Color::Yellow,
        "relay" => Color::Magenta,
        "replay" => Color::Green,
        _ => Color::White,
    }
}

fn last_error(error: &Option<String>) -> String {
    error.clone().unwrap_or_else(|| "-".to_string())
}

fn empty_dash(value: &str) -> String {
    if value.trim().is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

fn elapsed(instant: Instant) -> String {
    format!("{} ago", format_duration(instant.elapsed()))
}

fn until(instant: Instant) -> String {
    if instant <= Instant::now() {
        "now".to_string()
    } else {
        format!(
            "in {}",
            format_duration(instant.duration_since(Instant::now()))
        )
    }
}

fn format_optional_age(age: Option<Duration>) -> String {
    age.map(format_duration).unwrap_or_else(|| "-".to_string())
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else if duration < Duration::from_secs(1) {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{seconds}s")
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / 1024.0 / 1024.0)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn format_bytes_mb(megabytes: f64) -> String {
    format!("{megabytes:.1} MiB")
}

fn shorten(value: &str, width: usize) -> String {
    let max = width.max(8);
    if value.chars().count() <= max {
        return value.to_string();
    }
    let tail = value
        .chars()
        .rev()
        .take(max.saturating_sub(3))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("...{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::runtime::RuntimeState;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn renders_normal_and_compact_terminal_sizes() -> anyhow::Result<()> {
        let runtime = RuntimeState::new(Config::default());
        for view in View::ALL {
            for (width, height) in [(120, 40), (80, 24), (48, 12)] {
                let backend = TestBackend::new(width, height);
                let mut terminal = Terminal::new(backend)?;
                let mut app = TuiApp::default();
                app.view = view;
                let snapshot = runtime.snapshot();
                terminal.draw(|frame| draw(frame, &mut app, &snapshot))?;
            }
        }
        Ok(())
    }
}
