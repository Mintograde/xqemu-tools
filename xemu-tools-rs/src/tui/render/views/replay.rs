use super::super::*;

pub(in crate::tui::render) fn draw_replay(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &TuiApp,
    snapshot: &RuntimeSnapshot,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);
    draw_replay_summary(frame, chunks[0], snapshot);
    if area.width >= 105 {
        let lower = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(chunks[1]);
        draw_uploads(frame, lower[0], app, snapshot);
        draw_recent_replays(frame, lower[1], snapshot);
    } else {
        draw_uploads(frame, chunks[1], app, snapshot);
    }
}

fn draw_recent_replays(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let rows = snapshot
        .status
        .replay
        .recent_files
        .iter()
        .rev()
        .take(area.height.saturating_sub(3) as usize)
        .map(|file| {
            Row::new(vec![
                Cell::from(shorten(&file.path, 24)),
                Cell::from(format_bytes(file.bytes)),
                Cell::from(file.ticks.to_string()),
                Cell::from(format_duration(file.duration)),
                Cell::from(elapsed(file.saved_at)),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Min(16),
                Constraint::Length(9),
                Constraint::Length(7),
                Constraint::Length(9),
                Constraint::Length(10),
            ],
        )
        .header(table_header(["File", "Size", "Ticks", "Time", "Saved"]))
        .block(block("Recent Replays"))
        .column_spacing(1),
        area,
    );
}

fn draw_replay_summary(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let horizontal = area.width >= 92;
    let chunks = Layout::default()
        .direction(if horizontal {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(54), Constraint::Percentage(46)])
        .split(area);
    let replay = &snapshot.status.replay;
    let replay_edge = snapshot.pipeline.edge(PipelineEdge::Replay);
    let state_rows = vec![
        kv_status_row(
            "worker",
            replay.health,
            if replay.recording {
                "recording"
            } else {
                "idle"
            },
        ),
        kv_row("game id", empty_dash(&replay.current_game_id)),
        kv_row("ticks", replay.ticks_recorded.to_string()),
        kv_row("buffered", replay.ticks_buffered.to_string()),
        kv_row("queue", replay_edge.queue_depth.to_string()),
        kv_row("queue peak", replay_edge.high_water.to_string()),
        kv_row("saved", replay.saved_replays.to_string()),
        kv_row("upload requests", replay.upload_requests.to_string()),
        kv_row("last size", format_bytes(replay.last_save_bytes)),
        kv_row("JSON size", format_bytes(replay.last_uncompressed_bytes)),
        kv_row(
            "compression",
            if replay.last_save_bytes > 0 {
                format!(
                    "{:.2}:1",
                    replay.last_uncompressed_bytes as f64 / replay.last_save_bytes as f64
                )
            } else {
                "-".to_string()
            },
        ),
        kv_row("save time", format_duration(replay.last_save_duration)),
        kv_row(
            "recording age",
            replay
                .started_at
                .map(|started| format_duration(started.elapsed()))
                .unwrap_or_else(|| "-".to_string()),
        ),
        kv_row("spool size", format_bytes(replay.spool_bytes)),
        kv_row(
            "spool path",
            shorten(&replay.spool_path, chunks[0].width as usize),
        ),
        kv_row(
            "last file",
            shorten(&replay.last_saved_file, chunks[0].width as usize),
        ),
        kv_row("last error", last_error(&replay.last_error)),
    ];
    frame.render_widget(kv_table("Recorder", state_rows, 16), chunks[0]);

    let relay = &snapshot.status.relay;
    let controls = snapshot.controls;
    let control_rows = vec![
        kv_bool_row("save replays", controls.save_replays),
        kv_bool_row("save all ticks", controls.save_all_ticks),
        kv_bool_row("uploads", controls.replay_uploads),
        kv_row("pending uploads", relay.pending_uploads.to_string()),
        kv_row("relay messages", relay.messages_sent.to_string()),
        kv_row("live status", relay.live_status_sent.to_string()),
        kv_row("tick payloads", relay.compressed_ticks_sent.to_string()),
        kv_row("stale dropped", relay.dropped_stale_messages.to_string()),
        kv_row(
            "replay dir",
            shorten(
                &snapshot.config.replay_directory.display().to_string(),
                chunks[1].width as usize,
            ),
        ),
    ];
    frame.render_widget(kv_table("Capture Controls", control_rows, 17), chunks[1]);
}

fn draw_uploads(frame: &mut Frame<'_>, area: Rect, app: &TuiApp, snapshot: &RuntimeSnapshot) {
    let rows = snapshot
        .status
        .relay
        .uploads
        .iter()
        .enumerate()
        .rev()
        .map(|(index, upload)| {
            let row = Row::new(vec![
                Cell::from(shorten(&upload.request_id, 12)),
                Cell::from(upload.file_name.clone()),
                Cell::from(format_bytes(upload.size_bytes)),
                Cell::from(upload_phase_label(upload.phase))
                    .style(upload_phase_style(upload.phase)),
                Cell::from(upload.attempts.to_string()),
                Cell::from(upload.detail.clone()),
                Cell::from(elapsed(upload.updated_at)),
            ]);
            if index == app.selected_upload {
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
                Constraint::Length(12),
                Constraint::Min(18),
                Constraint::Length(10),
                Constraint::Length(12),
                Constraint::Length(8),
                Constraint::Percentage(30),
                Constraint::Length(11),
            ],
        )
        .header(table_header([
            "Request", "File", "Size", "State", "Attempts", "Detail", "Updated",
        ]))
        .block(block("Replay Uploads - Up/Down select, p retry, d cancel"))
        .column_spacing(1),
        area,
    );
}
