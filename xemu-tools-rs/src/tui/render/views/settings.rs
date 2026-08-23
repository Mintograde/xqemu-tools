use super::super::*;

pub(in crate::tui::render) fn draw_settings(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &TuiApp,
    snapshot: &RuntimeSnapshot,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Percentage(50),
            Constraint::Min(6),
        ])
        .split(area);
    draw_update_status(frame, chunks[0], snapshot);
    let visible = chunks[1].height.saturating_sub(3) as usize;
    let start = app
        .selected_setting
        .saturating_sub(visible.saturating_sub(1))
        .min(ConfigKey::ALL.len().saturating_sub(visible));
    let rows = ConfigKey::ALL
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(index, key)| {
            let desired = snapshot.config.value(*key);
            let active = snapshot.active_config.value(*key);
            let pending = desired != active;
            let row = Row::new(vec![
                Cell::from(key.label()),
                Cell::from(desired),
                Cell::from(active),
                Cell::from(snapshot.config.origin(*key)),
                Cell::from(if pending {
                    "restart required"
                } else if key.requires_restart() {
                    "applied at startup"
                } else {
                    "hot applied"
                })
                .style(if pending {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Green)
                }),
            ]);
            if index == app.selected_setting {
                row.style(Style::default().fg(Color::Black).bg(Color::Cyan))
            } else {
                row
            }
        })
        .collect::<Vec<_>>();
    let source = snapshot
        .config
        .source_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "config.toml (new)".to_string());
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(20),
                Constraint::Percentage(30),
                Constraint::Percentage(24),
                Constraint::Length(12),
                Constraint::Min(16),
            ],
        )
        .header(table_header([
            "Setting",
            "Desired",
            "Active",
            "Source",
            "Application",
        ]))
        .block(block(&format!(
            "Settings - {} - Enter edit/toggle, R reload",
            shorten(&source, 45)
        )))
        .column_spacing(1),
        chunks[1],
    );
    draw_command_history(frame, chunks[2], snapshot);
}

fn draw_update_status(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let update = &snapshot.status.update;
    let latest = if update.latest_version.is_empty() {
        "-".to_string()
    } else {
        update.latest_version.clone()
    };
    let checked = update
        .last_checked
        .map(elapsed)
        .unwrap_or_else(|| "never".to_string());
    let rows = vec![
        Row::new(vec![
            Cell::from("Version"),
            Cell::from(format!("{} -> {latest}", update.current_version)),
            Cell::from("Signature"),
            Cell::from(if update.signature_configured {
                "configured"
            } else {
                "missing; install disabled"
            })
            .style(if update.signature_configured {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Yellow)
            }),
        ]),
        Row::new(vec![
            Cell::from("State"),
            Cell::from(update_phase_label(update.phase)).style(update_phase_style(update.phase)),
            Cell::from("Checked"),
            Cell::from(checked),
        ]),
        Row::new(vec![
            Cell::from("Detail"),
            Cell::from(update.detail.clone()),
            Cell::from("Error"),
            Cell::from(last_error(&update.last_error)).style(if update.last_error.is_some() {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::DarkGray)
            }),
        ]),
    ];
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(9),
                Constraint::Percentage(33),
                Constraint::Length(10),
                Constraint::Min(20),
            ],
        )
        .block(block("Application Update - U check/install"))
        .column_spacing(1),
        area,
    );
}

fn draw_command_history(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let visible = area.height.saturating_sub(3) as usize;
    let rows = snapshot
        .commands
        .iter()
        .rev()
        .take(visible)
        .map(command_row)
        .collect::<Vec<_>>();
    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(11),
            Constraint::Length(22),
            Constraint::Min(16),
            Constraint::Length(10),
        ],
    )
    .header(table_header([
        "ID", "State", "Command", "Detail", "Updated",
    ]))
    .block(block("Recent Commands"))
    .column_spacing(1);
    frame.render_widget(table, area);
}
