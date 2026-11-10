use crate::calculator::InterfaceRate;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

pub fn draw(f: &mut Frame, rates: &[InterfaceRate]) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(f.area());

    // Header
    let header = Paragraph::new("Rust Linux Network Monitor (Press 'q' to quit)")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL).title("Dashboard"));
    f.render_widget(header, chunks[0]);

    // Table Headers
    let header_cells = [
        "Interface",
        "RX Rate",
        "TX Rate",
        "Total RX",
        "Total TX",
        "Pkt RX/TX",
        "Errors / Drops",
    ]
    .iter()
    .map(|h| {
        Cell::from(*h).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    });
    let header_row = Row::new(header_cells).height(1).bottom_margin(1);

    // Table Rows
    let rows = rates.iter().map(|r| {
        let cells = vec![
            Cell::from(r.name.clone()),
            Cell::from(format_rate(r.rx_rate)).style(Style::default().fg(Color::Green)),
            Cell::from(format_rate(r.tx_rate)).style(Style::default().fg(Color::Blue)),
            Cell::from(format_bytes(r.rx_bytes)),
            Cell::from(format_bytes(r.tx_bytes)),
            Cell::from(format!("{:.0}/{:.0}", r.rx_pps, r.tx_pps)),
            Cell::from(format!(
                "{}/{}",
                r.rx_errors + r.tx_errors,
                r.rx_dropped + r.tx_dropped
            )),
        ];
        Row::new(cells)
    });

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(18),
            Constraint::Percentage(14),
            Constraint::Percentage(14),
            Constraint::Percentage(13),
            Constraint::Percentage(13),
            Constraint::Percentage(14),
            Constraint::Percentage(14),
        ],
    )
    .header(header_row)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Network Interfaces"),
    );

    f.render_widget(table, chunks[1]);
}

fn format_rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 1024.0 {
        format!("{:.2} B/s", bytes_per_sec)
    } else if bytes_per_sec < 1024.0 * 1024.0 {
        format!("{:.2} KiB/s", bytes_per_sec / 1024.0)
    } else if bytes_per_sec < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} MiB/s", bytes_per_sec / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GiB/s", bytes_per_sec / (1024.0 * 1024.0 * 1024.0))
    }
}

fn format_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    if b < 1024.0 {
        format!("{:.2} B", b)
    } else if b < 1024.0 * 1024.0 {
        format!("{:.2} KiB", b / 1024.0)
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} MiB", b / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GiB", b / (1024.0 * 1024.0 * 1024.0))
    }
}
