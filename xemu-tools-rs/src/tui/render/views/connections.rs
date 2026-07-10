use super::super::*;

pub(in crate::tui::render) fn draw_connections(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &TuiApp,
    snapshot: &RuntimeSnapshot,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(5)])
        .split(area);
    draw_connection_summary(frame, chunks[0], snapshot);
    draw_local_clients(frame, chunks[1], app, snapshot);
}

fn draw_connection_summary(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let status = &snapshot.status;
    if area.width < 105 {
        let rows = vec![
            compact_connection_row(
                "xemu",
                status.xemu.health,
                format!(
                    "{}; {}{}",
                    status.xemu.detail,
                    status
                        .xemu
                        .pid
                        .map(|pid| format!("pid {pid}"))
                        .unwrap_or_else(|| "no pid".to_string()),
                    error_suffix(&status.xemu.last_error)
                ),
            ),
            compact_connection_row(
                "QMP",
                status.qmp.health,
                format!(
                    "{}; {}; reconnects={}{}",
                    empty_dash(&status.qmp.endpoint),
                    status.qmp.detail,
                    status.qmp.reconnects,
                    error_suffix(&status.qmp.last_error)
                ),
            ),
            compact_connection_row(
                "local WS",
                status.local_ws.health,
                format!(
                    "{}; clients={} sent={}{}",
                    empty_dash(&status.local_ws.bind_addr),
                    status.local_ws.client_count,
                    status.local_ws.messages_sent,
                    error_suffix(&status.local_ws.last_error)
                ),
            ),
            compact_connection_row(
                "relay",
                status.relay.health,
                format!(
                    "{}; attempts={} reconnects={} sent={}{}",
                    empty_dash(&status.relay.uri),
                    status.relay.attempts,
                    status.relay.reconnects,
                    status.relay.messages_sent,
                    error_suffix(&status.relay.last_error)
                ),
            ),
        ];
        let table = Table::new(
            rows,
            [
                Constraint::Length(12),
                Constraint::Length(13),
                Constraint::Min(16),
            ],
        )
        .header(table_header(["Connection", "State", "Detail"]))
        .block(block("Connections"))
        .column_spacing(1);
        frame.render_widget(table, area);
        return;
    }
    let rows = vec![
        connection_row(
            "xemu",
            status.xemu.health,
            status
                .xemu
                .pid
                .map(|pid| format!("pid {pid} ({pid:#x})"))
                .unwrap_or_else(|| "-".to_string()),
            status.xemu.detail.clone(),
            status.xemu.last_changed,
            &status.xemu.last_error,
        ),
        connection_row(
            "QMP",
            status.qmp.health,
            empty_dash(&status.qmp.endpoint),
            format!(
                "{}; reconnects={}",
                status.qmp.detail, status.qmp.reconnects
            ),
            status.qmp.last_changed,
            &status.qmp.last_error,
        ),
        connection_row(
            "local WS",
            status.local_ws.health,
            empty_dash(&status.local_ws.bind_addr),
            format!(
                "clients={} sent={}",
                status.local_ws.client_count, status.local_ws.messages_sent
            ),
            status.local_ws.last_changed,
            &status.local_ws.last_error,
        ),
        connection_row(
            "relay",
            status.relay.health,
            empty_dash(&status.relay.uri),
            format!(
                "attempts={} reconnects={} sent={}",
                status.relay.attempts, status.relay.reconnects, status.relay.messages_sent
            ),
            status.relay.last_changed,
            &status.relay.last_error,
        ),
    ];
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(13),
            Constraint::Percentage(27),
            Constraint::Percentage(32),
            Constraint::Length(12),
            Constraint::Min(12),
        ],
    )
    .header(table_header([
        "Connection",
        "State",
        "Endpoint",
        "Session",
        "Changed",
        "Last error",
    ]))
    .block(block("Connections"))
    .column_spacing(1);
    frame.render_widget(table, area);
}

fn compact_connection_row(key: &'static str, health: Health, detail: String) -> Row<'static> {
    Row::new(vec![
        Cell::from(key),
        Cell::from(health_label(health)).style(health_style(health)),
        Cell::from(detail),
    ])
}

fn error_suffix(error: &Option<String>) -> String {
    error
        .as_ref()
        .map(|error| format!("; error={error}"))
        .unwrap_or_default()
}

fn draw_local_clients(frame: &mut Frame<'_>, area: Rect, app: &TuiApp, snapshot: &RuntimeSnapshot) {
    let chunks = Layout::default()
        .direction(if area.width >= 96 {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(area);
    let rows = snapshot
        .status
        .local_ws
        .clients
        .iter()
        .enumerate()
        .map(|(index, client)| {
            let row = Row::new(vec![
                Cell::from(client.address.clone()),
                Cell::from(format_duration(client.connected_at.elapsed())),
                Cell::from(client.messages_sent.to_string()),
                Cell::from(format_bytes(client.bytes_sent)),
                Cell::from(client.lagged_messages.to_string())
                    .style(count_style(client.lagged_messages)),
                Cell::from(
                    client
                        .last_sent_at
                        .map(elapsed)
                        .unwrap_or_else(|| "-".to_string()),
                ),
            ]);
            if index == app.selected_client {
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
                Constraint::Min(18),
                Constraint::Length(9),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(8),
                Constraint::Length(12),
            ],
        )
        .header(table_header([
            "Client",
            "Age",
            "Messages",
            "Bytes",
            "Lagged",
            "Last send",
        ]))
        .block(block("Local WebSocket Clients"))
        .column_spacing(1),
        chunks[0],
    );

    let relay = &snapshot.status.relay;
    let rows = vec![
        kv_row(
            "transport",
            if relay.uri.starts_with("wss://") {
                "WebSocket over TLS"
            } else {
                "WebSocket"
            },
        ),
        kv_row("room", snapshot.active_config.ws_relay_room.clone()),
        kv_bool_row("producer key", relay.producer_key_present),
        kv_row("key expires", empty_dash(&relay.producer_key_expires_at)),
        kv_bool_row("key required", relay.require_key),
        kv_row("received", relay.messages_received.to_string()),
        kv_row(
            "last received",
            relay
                .last_received_at
                .map(elapsed)
                .unwrap_or_else(|| "-".to_string()),
        ),
        kv_row("backoff", format!("{}s", relay.reconnect_backoff_secs)),
        kv_row(
            "next retry",
            relay
                .next_reconnect_at
                .map(until)
                .unwrap_or_else(|| "-".to_string()),
        ),
    ];
    frame.render_widget(kv_table("Relay Session", rows, 15), chunks[1]);
}
