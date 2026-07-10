use super::super::*;

pub(in crate::tui::render) fn draw_overview(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
) {
    let horizontal = area.width >= 100;
    let chunks = Layout::default()
        .direction(if horizontal {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);
    draw_component_summary(frame, chunks[0], snapshot);
    draw_performance(frame, chunks[1], snapshot);
}

fn draw_component_summary(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let status = &snapshot.status;
    let rows = vec![
        component_row(
            "main loop",
            status.main.health,
            status.main.last_error.clone(),
        ),
        component_row("xemu", status.xemu.health, Some(status.xemu.detail.clone())),
        component_row("QMP", status.qmp.health, Some(status.qmp.detail.clone())),
        component_row(
            "local WS",
            status.local_ws.health,
            Some(format!(
                "{} clients, {} sent",
                status.local_ws.client_count, status.local_ws.messages_sent
            )),
        ),
        component_row(
            "relay",
            status.relay.health,
            Some(format!(
                "{} sent, {} pending uploads",
                status.relay.messages_sent, status.relay.pending_uploads
            )),
        ),
        component_row(
            "replay",
            status.replay.health,
            Some(if status.replay.recording {
                format!("recording {} ticks", status.replay.ticks_recorded)
            } else {
                "idle".to_string()
            }),
        ),
    ];
    let table = Table::new(
        rows,
        [
            Constraint::Length(13),
            Constraint::Length(13),
            Constraint::Min(12),
        ],
    )
    .header(table_header(["Component", "State", "Detail"]))
    .block(block("Components"))
    .column_spacing(1);
    frame.render_widget(table, area);
}

fn draw_performance(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let main = &snapshot.status.main;
    let replay_queue = snapshot.pipeline.edge(PipelineEdge::Replay).queue_depth;
    let local_queue = snapshot
        .pipeline
        .edge(PipelineEdge::LocalWebSocket)
        .queue_depth;
    let relay_queue = snapshot.pipeline.edge(PipelineEdge::Relay).queue_depth;
    let latest_command = snapshot
        .commands
        .last()
        .map(command_summary)
        .unwrap_or_else(|| "-".to_string());
    let rows = vec![
        kv_row("game info", format!("{:.3} ms", main.game_info_ms)),
        kv_row("loop", format!("{:.3} ms", main.loop_ms)),
        kv_row("post", format!("{:.3} ms", main.post_steps_ms)),
        kv_row("loops/tick", format!("{:.1}", main.loops_per_tick)),
        kv_row("dropped", main.dropped_ticks_total.to_string()),
        kv_row("reads/tick", main.read_count.to_string()),
        kv_row(
            "app CPU",
            format!(
                "{:.2}% / {:.3} cores",
                main.app_cpu_percent, main.app_cpu_cores
            ),
        ),
        kv_row("working set", format_bytes_mb(main.app_working_set_mbytes)),
        kv_row("private", format_bytes_mb(main.app_private_mbytes)),
        kv_row(
            "queues",
            format!("replay={replay_queue} local={local_queue} relay={relay_queue}"),
        ),
        kv_row(
            "last command",
            shorten(&latest_command, area.width as usize),
        ),
    ];
    frame.render_widget(kv_table("Performance", rows, 14), area);
}
