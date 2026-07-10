use super::super::*;

pub(in crate::tui::render) fn draw_game(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &TuiApp,
    snapshot: &RuntimeSnapshot,
) {
    if app.show_raw_game {
        let filter = app.raw_game_filter.to_ascii_lowercase();
        let filtered = app
            .raw_game_lines
            .iter()
            .filter(|line| filter.is_empty() || line.to_ascii_lowercase().contains(&filter))
            .collect::<Vec<_>>();
        let visible = area.height.saturating_sub(2) as usize;
        let max_scroll = filtered.len().saturating_sub(visible);
        let start = app.raw_game_scroll.min(max_scroll);
        let end = (start + visible).min(filtered.len());
        let lines = filtered[start..end]
            .iter()
            .map(|line| Line::from((*line).clone()))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines).block(block(&format!(
                "Raw game_info - lines {}-{} of {} - filter={}{} - g summary",
                start.saturating_add(1),
                end,
                filtered.len(),
                if app.raw_game_filter.is_empty() {
                    "<none>"
                } else {
                    &app.raw_game_filter
                },
                if app.editing_raw_game_filter {
                    " (editing)"
                } else {
                    ""
                }
            ))),
            area,
        );
        return;
    }
    let game = &snapshot.status.game;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(6)])
        .split(area);
    let summary = Line::from(vec![
        Span::styled("type ", Style::default().fg(Color::DarkGray)),
        Span::raw(empty_dash(&game.game_type)),
        Span::raw("  "),
        Span::styled("variant ", Style::default().fg(Color::DarkGray)),
        Span::raw(empty_dash(&game.variant)),
        Span::raw("  "),
        Span::styled("stage ", Style::default().fg(Color::DarkGray)),
        Span::raw(empty_dash(&game.stage)),
    ]);
    let counts = Line::from(format!(
        "teams {}  local players {}  objects {}  items {}  spawns {}",
        if game.has_teams { "on" } else { "off" },
        game.local_player_count,
        game.object_count,
        game.item_count,
        game.spawn_count
    ));
    frame.render_widget(
        Paragraph::new(vec![summary, counts]).block(block("Match")),
        chunks[0],
    );

    let body = Layout::default()
        .direction(if area.width >= 100 {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(chunks[1]);
    let visible_players = body[0].height.saturating_sub(3) as usize;
    let player_start = app
        .selected_player
        .saturating_sub(visible_players.saturating_sub(1))
        .min(game.players.len().saturating_sub(visible_players));
    let rows = game
        .players
        .iter()
        .enumerate()
        .skip(player_start)
        .take(visible_players)
        .map(|(index, player)| {
            let accuracy = if player.shots_fired > 0 {
                player.shots_hit as f64 / player.shots_fired as f64 * 100.0
            } else {
                0.0
            };
            let row = Row::new(vec![
                Cell::from(player.index.to_string()),
                Cell::from(player.name.clone()),
                Cell::from(player.team.to_string()),
                Cell::from(player.score.to_string()),
                Cell::from(player.kills.to_string()),
                Cell::from(player.deaths.to_string()),
                Cell::from(player.assists.to_string()),
                Cell::from(format!("{accuracy:.1}%")),
                Cell::from(if player.quit { "quit" } else { "active" }),
            ]);
            if index == app.selected_player {
                row.style(Style::default().fg(Color::Black).bg(Color::Cyan))
            } else {
                row
            }
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Min(12),
                Constraint::Length(5),
                Constraint::Length(7),
                Constraint::Length(5),
                Constraint::Length(5),
                Constraint::Length(5),
                Constraint::Length(8),
                Constraint::Length(7),
            ],
        )
        .header(table_header([
            "ID", "Player", "Team", "Score", "K", "D", "A", "Accuracy", "State",
        ]))
        .block(block("Players - Up/Down to inspect"))
        .column_spacing(1),
        body[0],
    );

    let mut lines = Vec::new();
    if let Some(player) = game.players.get(app.selected_player) {
        lines.push(Line::from(vec![
            Span::styled(
                player.name.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  health {:.2} shields {:.2}  pos {:.1}, {:.1}, {:.1}",
                player.health,
                player.shields,
                player.position[0],
                player.position[1],
                player.position[2]
            )),
        ]));
        lines.push(Line::from(format!(
            "powerups: camo={} overshield={}",
            player.has_camo, player.has_overshield
        )));
    }
    lines.push(Line::from(""));
    lines.extend(
        game.recent_events
            .iter()
            .rev()
            .take(body[1].height.saturating_sub(5) as usize)
            .rev()
            .cloned()
            .map(Line::from),
    );
    frame.render_widget(
        Paragraph::new(lines).block(block("Player Detail / Recent Events")),
        body[1],
    );
}
