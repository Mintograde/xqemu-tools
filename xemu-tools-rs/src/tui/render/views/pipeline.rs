use super::super::*;

pub(in crate::tui::render) fn draw_pipeline(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &TuiApp,
    snapshot: &RuntimeSnapshot,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(5)])
        .split(area);
    let replay = snapshot.pipeline.edge(PipelineEdge::Replay);
    let local = snapshot.pipeline.edge(PipelineEdge::LocalWebSocket);
    let relay = snapshot.pipeline.edge(PipelineEdge::Relay);
    let topology = vec![
        Line::from(vec![
            node("xemu memory"),
            Span::raw(" -> "),
            node("extractor"),
            Span::raw(" -> fanout"),
        ]),
        pipeline_line("replay", replay, "recorder -> disk -> upload"),
        pipeline_line("local ws", local, "serializer -> connected clients"),
        pipeline_line("relay", relay, "strip -> zstd -> remote relay"),
        Line::from(vec![
            node("QMP"),
            Span::raw(" -> address translation and reconnect control"),
        ]),
        Line::from("queue policy: unbounded delivery; failures mean consumer disconnected"),
    ];
    frame.render_widget(
        Paragraph::new(topology).block(block("Data Flow")),
        chunks[0],
    );

    let full = area.width >= 105;
    let rows = PipelineEdge::ALL
        .iter()
        .map(|edge| pipeline_row(*edge, app, snapshot, full))
        .collect::<Vec<_>>();
    let table = if full {
        Table::new(
            rows,
            [
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(7),
                Constraint::Length(7),
                Constraint::Length(11),
                Constraint::Length(11),
                Constraint::Length(9),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Min(10),
            ],
        )
        .header(table_header([
            "Edge", "State", "Queue", "Peak", "Input/s", "Output/s", "Failures", "Dropped",
            "Bytes", "Activity",
        ]))
    } else {
        Table::new(
            rows,
            [
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(9),
                Constraint::Length(14),
                Constraint::Min(10),
            ],
        )
        .header(table_header([
            "Edge", "State", "Queue", "In / out", "Activity",
        ]))
    };
    frame.render_widget(table.block(block("Edges")).column_spacing(1), chunks[1]);
}

fn pipeline_row(
    edge: PipelineEdge,
    app: &TuiApp,
    snapshot: &RuntimeSnapshot,
    full: bool,
) -> Row<'static> {
    let metrics = snapshot.pipeline.edge(edge);
    let health = edge_health(edge, snapshot);
    let rate = app.rate(edge);
    let activity = format!(
        "in {} / out {}",
        format_optional_age(metrics.last_enqueue_age),
        format_optional_age(metrics.last_dequeue_age)
    );
    let mut cells = vec![
        Cell::from(edge.label()),
        Cell::from(health_label(health)).style(health_style(health)),
        Cell::from(metrics.queue_depth.to_string()).style(queue_style(metrics.queue_depth)),
    ];
    if full {
        cells.extend([
            Cell::from(metrics.high_water.to_string()),
            Cell::from(format!("{:.1}", rate.enqueued)),
            Cell::from(format!("{:.1}", rate.dequeued)),
            Cell::from(metrics.send_failures.to_string()).style(count_style(metrics.send_failures)),
            Cell::from(metrics.dropped.to_string()).style(count_style(metrics.dropped)),
            Cell::from(format_bytes(metrics.processed_bytes)),
            Cell::from(activity),
        ]);
    } else {
        cells.extend([
            Cell::from(format!("{:.1} / {:.1}", rate.enqueued, rate.dequeued)),
            Cell::from(activity),
        ]);
    }
    Row::new(cells)
}
