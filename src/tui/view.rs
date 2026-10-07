//! Drawing the interactive mode: tabs, the query, the rows with their
//! state, the opener carousel.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::app::{App, Mode, Row};

pub const CLAUDE_WARNING: &str = "⚠ experimental: may break on any Claude Code update";
const HELP: &str = "←/→ mode   ↑/↓ select   enter accept   esc quit";
const OPENER_HELP: &str = "ctrl-←/→ opener";

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn tabs(app: &App) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, mode) in app.modes().iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" │ ", dim()));
        }
        let name = mode.name().to_uppercase();
        if *mode == app.mode() {
            spans.push(Span::styled(format!("[{name}]"), bold().fg(Color::Cyan)));
        } else {
            spans.push(Span::styled(format!(" {name} "), dim()));
        }
    }
    Line::from(spans)
}

fn carousel(app: &App) -> Line<'static> {
    let mut spans = vec![Span::styled("opener  ", dim())];
    for (index, opener) in app.openers().iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" | ", dim()));
        }
        if index == app.opener_index() {
            spans.push(Span::styled(format!("[{}]", opener.label), bold()));
        } else {
            spans.push(Span::raw(format!(" {} ", opener.label)));
        }
    }
    Line::from(spans)
}

fn state_style(state: &str) -> Style {
    if state.contains("stale") {
        Style::default().fg(Color::Red)
    } else if state.contains("dirty") {
        Style::default().fg(Color::Yellow)
    } else if state.contains("locked") {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default().fg(Color::Green)
    }
}

fn row_line(row: &Row, selected: bool, name_width: usize, detail_width: usize) -> Line<'static> {
    let marker = if selected { "> " } else { "  " };
    let name_style = if selected { bold() } else { Style::default() };
    let mut spans = vec![
        Span::styled(marker.to_string(), bold().fg(Color::Cyan)),
        Span::styled(format!("{:<name_width$}", row.name), name_style),
    ];
    if detail_width > 0 {
        spans.push(Span::styled(format!("  {:<detail_width$}", row.detail), dim()));
    }
    if !row.state.is_empty() {
        spans.push(Span::styled(format!("  {}", row.state), state_style(&row.state)));
    }
    Line::from(spans)
}

/// The rows that fit, scrolled so the selected one is on screen.
fn list(app: &App, height: usize) -> Vec<Line<'static>> {
    let rows: Vec<&Row> = app.visible().collect();
    if rows.is_empty() {
        let text = match (app.total(), app.mode()) {
            (0, Mode::Create) => "no branches to offer — type a name to create one",
            (0, _) => "nothing here",
            (_, Mode::Create) => "no match — enter creates this branch",
            _ => "no match",
        };
        return vec![Line::from(Span::styled(format!("  {text}"), dim()))];
    }
    let width = |text: &dyn Fn(&Row) -> usize| rows.iter().map(|row| text(row)).max().unwrap_or(0);
    let name_width = width(&|row| row.name.chars().count());
    let detail_width = width(&|row| row.detail.chars().count());
    let first = (app.selected() + 1).saturating_sub(height.max(1));
    rows.iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(index, row)| row_line(row, index == app.selected(), name_width, detail_width))
        .collect()
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let claude = app.mode() == Mode::Claude;
    let has_opener = app.mode().has_opener() && !app.openers().is_empty();
    let [tabs_area, warning_area, prompt_area, gap, list_area, opener_area, help_area] =
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(u16::from(claude)),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(u16::from(has_opener)),
            Constraint::Length(1),
        ])
        .areas(area);
    let _ = gap;

    frame.render_widget(Paragraph::new(tabs(app)), tabs_area);
    if claude {
        let warning = Span::styled(CLAUDE_WARNING, Style::default().fg(Color::Yellow));
        frame.render_widget(Paragraph::new(Line::from(warning)), warning_area);
    }
    let prompt = app.mode().prompt();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prompt, bold()),
            Span::raw(app.query().to_string()),
        ])),
        prompt_area,
    );
    frame.render_widget(Paragraph::new(list(app, list_area.height as usize)), list_area);
    if has_opener {
        frame.render_widget(Paragraph::new(carousel(app)), opener_area);
    }
    let help = if has_opener { format!("{HELP}   {OPENER_HELP}") } else { HELP.to_string() };
    frame.render_widget(Paragraph::new(Line::from(Span::styled(help, dim()))), help_area);

    let typed = (prompt.chars().count() + app.query().chars().count()) as u16;
    let cursor =
        Rect { x: prompt_area.x + typed.min(prompt_area.width.saturating_sub(1)), ..prompt_area };
    frame.set_cursor_position((cursor.x, cursor.y));
}

#[cfg(test)]
mod tests {
    use super::super::app::tests::{app, key, openers};
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;

    /// What the screen shows, line by line, trailing blanks dropped.
    fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                let line: String =
                    (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect();
                line.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn open_mode_shows_tabs_rows_with_state_and_the_carousel() {
        let app = app(Mode::Open);
        assert_eq!(
            screen(&app, 70, 9),
            [
                " CREATE  │ [OPEN] │  CHECKOUT  │  DELETE",
                "Open:",
                "",
                "> feat       feature/feat  clean",
                "  fix-login  fix/login     dirty",
                "  docs       docs          stale locked",
                "",
                "opener  [edit] |  shell",
                "←/→ mode   ↑/↓ select   enter accept   esc quit   ctrl-←/→ opener",
            ]
        );
    }

    #[test]
    fn the_query_the_selection_and_the_opener_follow_the_keys() {
        let mut app = app(Mode::Open);
        for ch in "fi".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(ratatui::crossterm::event::KeyEvent::new(
            KeyCode::Right,
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        ));
        let lines = screen(&app, 70, 9);
        assert_eq!(lines[1], "Open: fi");
        assert_eq!(lines[3], "> fix-login  fix/login  dirty");
        assert_eq!(lines[4], "");
        assert_eq!(lines[7], "opener   edit  | [shell]");
    }

    #[test]
    fn modes_without_an_opener_have_no_carousel() {
        let app = app(Mode::Delete);
        let lines = screen(&app, 70, 8);
        assert_eq!(lines[0], " CREATE  │  OPEN  │  CHECKOUT  │ [DELETE]");
        assert_eq!(lines[1], "Delete:");
        assert_eq!(lines[7], "←/→ mode   ↑/↓ select   enter accept   esc quit");
        assert!(lines.iter().all(|line| !line.starts_with("opener")));
    }

    #[test]
    fn claude_mode_carries_its_warning() {
        let mut modes = Mode::BASE.to_vec();
        modes.push(Mode::Claude);
        let mut app = App::new(modes, Mode::Claude, openers());
        app.set_rows(vec![Row::new("abc-123", "fix the login bug", "")]);
        let lines = screen(&app, 80, 8);
        assert!(lines[0].ends_with("│ [CLAUDE]"), "{}", lines[0]);
        assert_eq!(lines[1], CLAUDE_WARNING);
        assert_eq!(lines[2], "Copy session:");
        assert_eq!(lines[4], "> abc-123  fix the login bug");
    }

    #[test]
    fn empty_lists_say_what_enter_would_do() {
        let message = |mode: Mode, rows: Vec<Row>, query: &str| {
            let mut app = App::new(Mode::BASE.to_vec(), mode, openers());
            app.set_rows(rows);
            for ch in query.chars() {
                app.handle_key(key(KeyCode::Char(ch)));
            }
            screen(&app, 70, 8)[3].clone()
        };
        let one = || vec![Row::new("feat", "local", "")];
        assert_eq!(
            message(Mode::Create, vec![], ""),
            "  no branches to offer — type a name to create one"
        );
        assert_eq!(message(Mode::Create, one(), "zzz"), "  no match — enter creates this branch");
        assert_eq!(message(Mode::Open, vec![], ""), "  nothing here");
        assert_eq!(message(Mode::Open, one(), "zzz"), "  no match");
    }

    #[test]
    fn a_long_list_scrolls_to_keep_the_selection_on_screen() {
        let mut app = App::new(Mode::BASE.to_vec(), Mode::Delete, openers());
        app.set_rows(
            (0..30).map(|index| Row::new(&format!("wt{index:02}"), "", "clean")).collect(),
        );
        for _ in 0..12 {
            app.handle_key(key(KeyCode::Down));
        }
        let lines = screen(&app, 40, 9); // five rows fit
        assert_eq!(lines[3], "  wt08  clean");
        assert_eq!(lines[7], "> wt12  clean");
    }

    #[test]
    fn every_state_has_a_color() {
        let colors: Vec<Option<Color>> = ["clean", "dirty", "stale locked", "clean locked"]
            .iter()
            .map(|state| state_style(state).fg)
            .collect();
        assert_eq!(
            colors,
            [Some(Color::Green), Some(Color::Yellow), Some(Color::Red), Some(Color::Magenta)]
        );
    }
}
