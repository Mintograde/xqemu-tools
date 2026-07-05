use crate::runtime::{AppCommand, Health, RuntimeSnapshot, SharedRuntime};
use anyhow::Result;
use crossbeam_channel::Sender;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};
use std::io;
use std::thread;
use std::time::{Duration, Instant};

pub fn start_tui(
    runtime: SharedRuntime,
    command_tx: Sender<AppCommand>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if let Err(err) = run_tui(runtime.clone(), command_tx) {
            runtime.log("tui", format!("TUI stopped: {err:#}"));
        }
    })
}

fn run_tui(runtime: SharedRuntime, command_tx: Sender<AppCommand>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_tui_loop(&mut terminal, runtime, command_tx);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), Show, LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    runtime: SharedRuntime,
    command_tx: Sender<AppCommand>,
) -> Result<()> {
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    loop {
        if last_draw.elapsed() >= Duration::from_millis(250) {
            let snapshot = runtime.snapshot();
            terminal.draw(|frame| draw(frame, &snapshot))?;
            last_draw = Instant::now();
        }

        if event::poll(Duration::from_millis(50))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    let _ = command_tx.send(AppCommand::Shutdown);
                    break;
                }
                KeyCode::Char('r') => {
                    let _ = command_tx.send(AppCommand::ReconnectRelay);
                }
                KeyCode::Char('x') => {
                    let _ = command_tx.send(AppCommand::ReconnectXemu);
                }
                KeyCode::Char('m') => {
                    let _ = command_tx.send(AppCommand::ReconnectQmp);
                }
                KeyCode::Char('s') => {
                    let _ = command_tx.send(AppCommand::ToggleReplaySaving);
                }
                KeyCode::Char('t') => {
                    let _ = command_tx.send(AppCommand::ToggleSaveAllTicks);
                }
                KeyCode::Char('u') => {
                    let _ = command_tx.send(AppCommand::ToggleReplayUploads);
                }
                KeyCode::Char('c') => {
                    let _ = command_tx.send(AppCommand::ClearLogs);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn draw(frame: &mut Frame<'_>, snapshot: &RuntimeSnapshot) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(15),
            Constraint::Length(11),
            Constraint::Min(7),
            Constraint::Length(3),
        ])
        .split(frame.area());

    draw_header(frame, vertical[0], snapshot);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(36),
            Constraint::Percentage(34),
            Constraint::Percentage(30),
        ])
        .split(vertical[1]);
    draw_connections(frame, top[0], snapshot);
    draw_metrics(frame, top[1], snapshot);
    draw_config(frame, top[2], snapshot);

    let middle = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(vertical[2]);
    draw_replay(frame, middle[0], snapshot);
    draw_workers(frame, middle[1], snapshot);

    draw_logs(frame, vertical[3], snapshot);
    draw_shortcuts(frame, vertical[4]);
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let main = &snapshot.status.main;
    let text = vec![
        Line::from(vec![
            Span::styled(
                "xemu-tools-rs",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(health_label(main.health), health_style(main.health)),
            Span::raw(format!(
                "  uptime {}  tick {}  map {}",
                format_duration(snapshot.uptime),
                main.game_time,
                empty_dash(&main.map_name)
            )),
        ]),
        Line::from(vec![Span::raw(format!(
            "game {}  status {}  players {}  events {}",
            empty_dash(&main.game_id),
            empty_dash(&main.game_status),
            main.player_count,
            main.event_count
        ))]),
    ];
    frame.render_widget(Paragraph::new(text).block(block("Runtime")), area);
}

fn draw_connections(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let status = &snapshot.status;
    let rows = vec![
        kv_status_row(
            "xemu",
            status.xemu.health,
            format!(
                "{}{}",
                status.xemu.detail,
                status
                    .xemu
                    .pid
                    .map(|pid| format!(" ({pid}/{pid:#x})"))
                    .unwrap_or_default()
            ),
        ),
        kv_status_row("qmp", status.qmp.health, status.qmp.detail.clone()),
        kv_status_row(
            "local ws",
            status.local_ws.health,
            format!(
                "{} clients={} sent={}",
                empty_dash(&status.local_ws.bind_addr),
                status.local_ws.client_count,
                status.local_ws.messages_sent
            ),
        ),
        kv_status_row(
            "relay",
            status.relay.health,
            format!(
                "attempts={} reconnects={} pending_uploads={}",
                status.relay.attempts, status.relay.reconnects, status.relay.pending_uploads
            ),
        ),
        kv_row("relay uri", shorten(&status.relay.uri, area.width as usize)),
        kv_row(
            "last qmp",
            status
                .qmp
                .last_changed
                .map(elapsed)
                .unwrap_or_else(|| "-".to_string()),
        ),
    ];
    frame.render_widget(kv_table("Connections", rows), area);
}

fn draw_metrics(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let main = &snapshot.status.main;
    let rows = vec![
        kv_row("game info", format!("{:.3} ms", main.game_info_ms)),
        kv_row("loop", format!("{:.3} ms", main.loop_ms)),
        kv_row("post", format!("{:.3} ms", main.post_steps_ms)),
        kv_row("loops/tick", format!("{:.1}", main.loops_per_tick)),
        kv_row("dropped", main.dropped_ticks_total.to_string()),
        kv_row("app cpu", format!("{:.2}%", main.app_cpu_percent)),
        kv_row("app cores", format!("{:.3}", main.app_cpu_cores)),
        kv_row("app ws", format!("{:.1} MB", main.app_working_set_mbytes)),
        kv_row("app private", format!("{:.1} MB", main.app_private_mbytes)),
        kv_row("app pagefile", format!("{:.1} MB", main.app_pagefile_mbytes)),
        kv_row("reads/tick", main.read_count.to_string()),
        kv_row(
            "last tick",
            main.last_tick_at
                .map(elapsed)
                .unwrap_or_else(|| "-".to_string()),
        ),
    ];
    frame.render_widget(kv_table("Performance", rows), area);
}

fn draw_config(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let config = &snapshot.config;
    let controls = snapshot.controls;
    let rows = vec![
        kv_row(
            "local ws",
            format!("{}:{}", config.websocket_host, config.websocket_port),
        ),
        kv_row("qmp", format!("{}:{}", config.qmp_host, config.qmp_port)),
        kv_row("replay dir", shorten(&config.replay_directory.display().to_string(), 38)),
        kv_bool_row("relay enabled", config.ws_relay_enabled),
        kv_row("relay room", config.ws_relay_room.clone()),
        kv_bool_row("spawn hash", config.compute_spawn_parameters_hash),
        kv_bool_row("save replays", controls.save_replays),
        kv_bool_row("save ticks", controls.save_all_ticks),
        kv_bool_row("uploads", controls.replay_uploads),
    ];
    frame.render_widget(kv_table("Config", rows), area);
}

fn draw_replay(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let replay = &snapshot.status.replay;
    let rows = vec![
        kv_status_row("worker", replay.health, worker_detail(replay.recording)),
        kv_row("game id", empty_dash(&replay.current_game_id)),
        kv_row("ticks", replay.ticks_recorded.to_string()),
        kv_row("buffered", replay.ticks_buffered.to_string()),
        kv_row("queue", replay.queue_depth.to_string()),
        kv_row("saved", replay.saved_replays.to_string()),
        kv_row("upload req", replay.upload_requests.to_string()),
        kv_row("last bytes", replay.last_save_bytes.to_string()),
        kv_row("last file", shorten(&replay.last_saved_file, area.width as usize)),
    ];
    frame.render_widget(kv_table("Replay", rows), area);
}

fn draw_workers(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let status = &snapshot.status;
    let rows = vec![
        kv_status_row("main loop", status.main.health, last_error(&status.main.last_error)),
        kv_status_row("replay", status.replay.health, last_error(&status.replay.last_error)),
        kv_status_row(
            "local ws",
            status.local_ws.health,
            last_error(&status.local_ws.last_error),
        ),
        kv_status_row("relay", status.relay.health, last_error(&status.relay.last_error)),
        kv_row("relay msgs", status.relay.messages_sent.to_string()),
        kv_row("live status", status.relay.live_status_sent.to_string()),
        kv_row("tick payloads", status.relay.compressed_ticks_sent.to_string()),
        kv_row("stale dropped", status.relay.dropped_stale_messages.to_string()),
        kv_row("qmp reconnects", status.qmp.reconnects.to_string()),
    ];
    frame.render_widget(kv_table("Workers", rows), area);
}

fn draw_logs(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let visible = area.height.saturating_sub(2) as usize;
    let mut lines = snapshot
        .status
        .logs
        .iter()
        .rev()
        .take(visible)
        .map(|line| {
            Line::from(vec![
                Span::styled(line.when.clone(), Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(
                    format!("{:<8}", line.source),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(" "),
                Span::raw(line.message.clone()),
            ])
        })
        .collect::<Vec<_>>();
    lines.reverse();
    frame.render_widget(
        Paragraph::new(lines)
            .block(block("Logs"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_shortcuts(frame: &mut Frame<'_>, area: Rect) {
    let shortcuts = Line::from(vec![
        key("q"),
        Span::raw(" quit  "),
        key("r"),
        Span::raw(" relay reconnect  "),
        key("x"),
        Span::raw(" reconnect xemu  "),
        key("m"),
        Span::raw(" reconnect qmp  "),
        key("s"),
        Span::raw(" save replay  "),
        key("t"),
        Span::raw(" save ticks  "),
        key("u"),
        Span::raw(" uploads  "),
        key("c"),
        Span::raw(" clear logs"),
    ]);
    frame.render_widget(Paragraph::new(shortcuts).block(block("Shortcuts")), area);
}

fn kv_table<'a>(title: &'a str, rows: Vec<Row<'a>>) -> Table<'a> {
    Table::new(rows, [Constraint::Length(15), Constraint::Min(12)])
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

fn bool_style(value: bool) -> Style {
    Style::default().fg(if value { Color::Green } else { Color::Yellow })
}

fn worker_detail(recording: bool) -> &'static str {
    if recording { "recording" } else { "idle" }
}

fn last_error(error: &Option<String>) -> String {
    error.clone().unwrap_or_else(|| "ok".to_string())
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

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

fn shorten(value: &str, width: usize) -> String {
    let max = width.saturating_sub(20).max(16);
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
