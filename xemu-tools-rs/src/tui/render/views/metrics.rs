use super::super::*;

pub(in crate::tui::render) fn draw_metrics(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &TuiApp,
    snapshot: &RuntimeSnapshot,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(area);
    let top = split_metric_row(rows[0]);
    let middle = split_metric_row(rows[1]);
    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(rows[2]);
    let game_info = app
        .metrics
        .iter()
        .map(|sample| sample.game_info_us)
        .collect::<Vec<_>>();
    let loop_time = app
        .metrics
        .iter()
        .map(|sample| sample.loop_us)
        .collect::<Vec<_>>();
    let post_time = app
        .metrics
        .iter()
        .map(|sample| sample.post_us)
        .collect::<Vec<_>>();
    let cpu = app
        .metrics
        .iter()
        .map(|sample| sample.cpu_hundredths)
        .collect::<Vec<_>>();
    let memory = app
        .metrics
        .iter()
        .map(|sample| sample.memory_tenths)
        .collect::<Vec<_>>();
    let reads = app
        .metrics
        .iter()
        .map(|sample| sample.reads)
        .collect::<Vec<_>>();
    let queues = app
        .metrics
        .iter()
        .map(|sample| sample.replay_queue + sample.local_queue + sample.relay_queue)
        .collect::<Vec<_>>();
    draw_sparkline(
        frame,
        top[0],
        &format!("Extraction {:.3} ms", snapshot.status.main.game_info_ms),
        &game_info,
        Color::Cyan,
    );
    draw_sparkline(
        frame,
        top[1],
        &format!("Loop {:.3} ms", snapshot.status.main.loop_ms),
        &loop_time,
        Color::Green,
    );
    draw_sparkline(
        frame,
        middle[0],
        &format!("Post {:.3} ms", snapshot.status.main.post_steps_ms),
        &post_time,
        Color::Yellow,
    );
    draw_sparkline(
        frame,
        middle[1],
        &format!("CPU {:.2}%", snapshot.status.main.app_cpu_percent),
        &cpu,
        Color::Magenta,
    );
    draw_sparkline(
        frame,
        bottom[0],
        &format!(
            "Working set {:.1} MiB",
            snapshot.status.main.app_working_set_mbytes
        ),
        &memory,
        Color::Blue,
    );
    draw_sparkline(
        frame,
        bottom[1],
        &format!("Reads/tick {}", snapshot.status.main.read_count),
        &reads,
        Color::White,
    );
    let queue_depth = PipelineEdge::ALL
        .iter()
        .map(|edge| snapshot.pipeline.edge(*edge).queue_depth)
        .sum::<usize>();
    draw_sparkline(
        frame,
        bottom[2],
        &format!("Total queue depth {queue_depth}"),
        &queues,
        Color::Red,
    );
}

fn split_metric_row(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area)
}

fn draw_sparkline(frame: &mut Frame<'_>, area: Rect, title: &str, values: &[u64], color: Color) {
    frame.render_widget(
        Sparkline::default()
            .block(block(title))
            .data(values)
            .style(Style::default().fg(color)),
        area,
    );
}
