use super::super::*;

pub(in crate::tui::render) fn draw_logs(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut TuiApp,
    snapshot: &RuntimeSnapshot,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3)])
        .split(area);
    let filter_style = if app.editing_filter {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let filter_text = if app.log_filter.is_empty() {
        "<all sources and messages>".to_string()
    } else {
        app.log_filter.clone()
    };
    let mode = if app.log_follow {
        "following"
    } else {
        "paused"
    };
    let level = app.log_level.map(log_level_label).unwrap_or("all levels");
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("filter "),
            Span::styled(filter_text, filter_style),
            Span::raw(format!("  {level}  {mode}  offset {}", app.log_offset)),
        ]))
        .block(block(if app.editing_filter {
            "Log Filter - type, Enter to apply"
        } else {
            "Log Filter"
        })),
        chunks[0],
    );

    let filtered = snapshot
        .status
        .logs
        .iter()
        .filter(|line| app.log_matches(line))
        .collect::<Vec<_>>();
    let log_areas = if chunks[1].height >= 10 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(5)])
            .split(chunks[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(0)])
            .split(chunks[1])
    };
    let visible = log_areas[0].height.saturating_sub(2) as usize;
    let (start, end) = app.log_window(filtered.len(), visible);
    let lines = filtered[start..end]
        .iter()
        .map(|line| {
            Line::from(vec![
                Span::styled(
                    format!("#{:<5}", line.sequence),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(line.when.clone(), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(" {:>6}", format_duration(line.at.elapsed())),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<5}", log_level_label(line.level)),
                    log_level_style(line.level),
                ),
                Span::styled(
                    format!("{:<9}", line.source),
                    Style::default().fg(source_color(&line.source)),
                ),
                Span::raw(" "),
                Span::raw(line.message.clone()),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines).block(block("Logs")), log_areas[0]);
    if log_areas[1].height > 0 {
        let detail = filtered
            .get(end.saturating_sub(1))
            .map(|line| {
                format!(
                    "#{} {} {} {}: {}",
                    line.sequence,
                    line.when,
                    log_level_label(line.level),
                    line.source,
                    line.message
                )
            })
            .unwrap_or_else(|| "No matching log entry".to_string());
        frame.render_widget(
            Paragraph::new(detail)
                .block(block("Full Entry"))
                .wrap(Wrap { trim: false }),
            log_areas[1],
        );
    }
}
