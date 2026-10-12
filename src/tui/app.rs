//! The interactive mode's state, with no terminal in sight: what is
//! listed, what is typed, what a key does.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Create,
    Open,
    Checkout,
    Delete,
    Claude,
}

impl Mode {
    pub const BASE: [Mode; 4] = [Mode::Create, Mode::Open, Mode::Checkout, Mode::Delete];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Create => "create",
            Mode::Open => "open",
            Mode::Checkout => "checkout",
            Mode::Delete => "delete",
            Mode::Claude => "claude",
        }
    }

    pub fn from_name(name: &str) -> Option<Mode> {
        [Mode::Create, Mode::Open, Mode::Checkout, Mode::Delete, Mode::Claude]
            .into_iter()
            .find(|mode| mode.name() == name)
    }

    pub fn prompt(self) -> &'static str {
        match self {
            Mode::Create => "Branch/New: ",
            Mode::Open => "Open: ",
            Mode::Checkout => "Checkout: ",
            Mode::Delete => "Delete: ",
            Mode::Claude => "Copy session: ",
        }
    }

    /// Whether what is picked is then opened — and so with which opener.
    pub fn has_opener(self) -> bool {
        matches!(self, Mode::Create | Mode::Open)
    }
}

/// One thing to pick: its name (what the command gets), what it is, and
/// the state it is in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub name: String,
    /// A branch's location, a worktree's branch, a session's description.
    pub detail: String,
    /// `clean`, `dirty`, `stale`, `locked` — worktrees only.
    pub state: String,
}

impl Row {
    pub fn new(name: &str, detail: &str, state: &str) -> Self {
        Self { name: name.into(), detail: detail.into(), state: state.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opener {
    pub label: String,
    /// What `create`/`open` get as the opener; None = the default.
    pub arg: Option<String>,
}

/// What a key came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Stay,
    /// The mode changed: its rows have to be loaded.
    Reload,
    Quit,
    /// A row's name — or, in create mode, what was typed when nothing
    /// matches: a new branch.
    Accept(String),
}

pub struct App {
    modes: Vec<Mode>,
    mode: usize,
    openers: Vec<Opener>,
    opener: usize,
    query: String,
    rows: Vec<Row>,
    /// Indices into `rows` that match the query, best first.
    matches: Vec<usize>,
    /// Index into `matches`.
    selected: usize,
    matcher: Matcher,
}

pub fn cycle(length: usize, index: usize, forward: bool) -> usize {
    if length == 0 {
        return 0;
    }
    if forward { (index + 1) % length } else { (index + length - 1) % length }
}

impl App {
    pub fn new(modes: Vec<Mode>, mode: Mode, openers: Vec<Opener>) -> Self {
        let mode = modes.iter().position(|known| *known == mode).unwrap_or(0);
        Self {
            modes,
            mode,
            openers,
            opener: 0,
            query: String::new(),
            rows: Vec::new(),
            matches: Vec::new(),
            selected: 0,
            matcher: Matcher::new(MatcherConfig::DEFAULT),
        }
    }

    pub fn modes(&self) -> &[Mode] {
        &self.modes
    }

    pub fn mode(&self) -> Mode {
        self.modes[self.mode]
    }

    pub fn openers(&self) -> &[Opener] {
        &self.openers
    }

    pub fn opener_index(&self) -> usize {
        self.opener
    }

    /// The opener the carousel is on, for the modes that open.
    pub fn opener_arg(&self) -> Option<String> {
        if !self.mode().has_opener() {
            return None;
        }
        self.openers.get(self.opener).and_then(|opener| opener.arg.clone())
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The rows that match, best first.
    pub fn visible(&self) -> impl Iterator<Item = &Row> {
        self.matches.iter().map(|index| &self.rows[*index])
    }

    pub fn total(&self) -> usize {
        self.rows.len()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The current mode's rows, freshly loaded: the query starts over.
    pub fn set_rows(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        self.query.clear();
        self.refilter();
    }

    fn refilter(&mut self) {
        self.selected = 0;
        if self.query.trim().is_empty() {
            self.matches = (0..self.rows.len()).collect();
            return;
        }
        let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let mut buffer = Vec::new();
        let mut scored: Vec<(u32, usize)> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let haystack = format!("{} {}", row.name, row.detail);
                let score =
                    pattern.score(Utf32Str::new(&haystack, &mut buffer), &mut self.matcher)?;
                Some((score, index))
            })
            .collect();
        // best score first; equal scores keep the listing's own order
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.matches = scored.into_iter().map(|(_, index)| index).collect();
    }

    fn switch_mode(&mut self, forward: bool) -> Step {
        self.mode = cycle(self.modes.len(), self.mode, forward);
        Step::Reload
    }

    fn accept(&self) -> Step {
        if let Some(row) = self.matches.get(self.selected).map(|index| &self.rows[*index]) {
            return Step::Accept(row.name.clone());
        }
        let typed = self.query.trim();
        if self.mode() == Mode::Create && !typed.is_empty() {
            return Step::Accept(typed.to_string());
        }
        Step::Stay
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Step {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return Step::Quit,
            KeyCode::Char('c') if ctrl => return Step::Quit,
            KeyCode::Enter => return self.accept(),
            KeyCode::Left | KeyCode::Right if ctrl && self.mode().has_opener() => {
                self.opener = cycle(self.openers.len(), self.opener, key.code == KeyCode::Right);
            }
            // no carousel in this mode: and not a mode switch either
            KeyCode::Left | KeyCode::Right if ctrl => {}
            KeyCode::Left => return self.switch_mode(false),
            KeyCode::Right => return self.switch_mode(true),
            KeyCode::Char('h') if alt => return self.switch_mode(false),
            KeyCode::Char('l') if alt => return self.switch_mode(true),
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.move_down(),
            KeyCode::Char('k') if alt => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('j') if alt => self.move_down(),
            KeyCode::Char('p') if ctrl => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('n') if ctrl => self.move_down(),
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
            }
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.refilter();
            }
            KeyCode::Char('w') if ctrl => {
                let kept = self.query.trim_end().rfind(' ').map_or(0, |space| space + 1);
                self.query.truncate(kept);
                self.refilter();
            }
            KeyCode::Char(ch) if !ctrl && !alt => {
                self.query.push(ch);
                self.refilter();
            }
            _ => {}
        }
        Step::Stay
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.matches.len() {
            self.selected += 1;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    pub fn with(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    pub fn openers() -> Vec<Opener> {
        vec![
            Opener { label: "edit".into(), arg: None },
            Opener { label: "shell".into(), arg: Some("/bin/sh".into()) },
        ]
    }

    pub fn app(mode: Mode) -> App {
        let mut app = App::new(Mode::BASE.to_vec(), mode, openers());
        app.set_rows(vec![
            Row::new("feat", "feature/feat", "clean"),
            Row::new("fix-login", "fix/login", "dirty"),
            Row::new("docs", "docs", "stale locked"),
        ]);
        app
    }

    fn type_text(app: &mut App, text: &str) {
        for ch in text.chars() {
            assert_eq!(app.handle_key(key(KeyCode::Char(ch))), Step::Stay);
        }
    }

    fn names(app: &App) -> Vec<&str> {
        app.visible().map(|row| row.name.as_str()).collect()
    }

    #[test]
    fn cycle_wraps_both_directions() {
        assert_eq!(cycle(3, 0, false), 2);
        assert_eq!(cycle(3, 2, true), 0);
        assert_eq!(cycle(3, 1, true), 2);
        assert_eq!(cycle(0, 0, true), 0);
    }

    #[test]
    fn modes_know_their_names_prompts_and_openers() {
        for mode in Mode::BASE.into_iter().chain([Mode::Claude]) {
            assert_eq!(Mode::from_name(mode.name()), Some(mode));
            assert!(mode.prompt().ends_with(": "));
        }
        assert_eq!(Mode::from_name("nope"), None);
        assert!(Mode::Create.has_opener() && Mode::Open.has_opener());
        assert!(
            !Mode::Delete.has_opener()
                && !Mode::Checkout.has_opener()
                && !Mode::Claude.has_opener()
        );
    }

    #[test]
    fn arrows_and_alt_keys_switch_modes_and_ask_for_a_reload() {
        let mut app = app(Mode::Open);
        assert_eq!(app.handle_key(key(KeyCode::Right)), Step::Reload);
        assert_eq!(app.mode(), Mode::Checkout);
        assert_eq!(app.handle_key(with(KeyCode::Char('l'), KeyModifiers::ALT)), Step::Reload);
        assert_eq!(app.mode(), Mode::Delete);
        assert_eq!(app.handle_key(key(KeyCode::Right)), Step::Reload);
        assert_eq!(app.mode(), Mode::Create); // wraps
        assert_eq!(app.handle_key(key(KeyCode::Left)), Step::Reload);
        assert_eq!(app.handle_key(with(KeyCode::Char('h'), KeyModifiers::ALT)), Step::Reload);
        assert_eq!(app.mode(), Mode::Checkout);
    }

    #[test]
    fn ctrl_arrows_turn_the_opener_carousel_only_where_there_is_one() {
        let mut app = app(Mode::Open);
        assert_eq!(app.opener_arg(), None); // "edit": the default opener
        assert_eq!(app.handle_key(with(KeyCode::Right, KeyModifiers::CONTROL)), Step::Stay);
        assert_eq!((app.opener_index(), app.opener_arg().as_deref()), (1, Some("/bin/sh")));
        app.handle_key(with(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(app.opener_index(), 0);
        app.handle_key(with(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(app.opener_index(), 1);

        let mut delete = App::new(Mode::BASE.to_vec(), Mode::Delete, openers());
        delete.handle_key(with(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!((delete.opener_index(), delete.mode()), (0, Mode::Delete));
        assert_eq!(delete.opener_arg(), None);
    }

    #[test]
    fn typing_filters_and_the_best_match_comes_first() {
        let mut app = app(Mode::Open);
        assert_eq!(names(&app), ["feat", "fix-login", "docs"]);
        type_text(&mut app, "f");
        assert_eq!(names(&app), ["feat", "fix-login"]);
        type_text(&mut app, "ix");
        assert_eq!((names(&app), app.query()), (vec!["fix-login"], "fix"));
        app.handle_key(key(KeyCode::Backspace));
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(names(&app).len(), 2);
        // the detail column is searched too
        app.handle_key(with(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut app, "login");
        assert_eq!(names(&app), ["fix-login"]);
        type_text(&mut app, " zzz");
        assert!(names(&app).is_empty());
        app.handle_key(with(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!((app.query(), names(&app)), ("login ", vec!["fix-login"]));
        assert_eq!(app.total(), 3);
    }

    #[test]
    fn selection_moves_within_the_matches() {
        let mut app = app(Mode::Open);
        for down in [
            key(KeyCode::Down),
            with(KeyCode::Char('j'), KeyModifiers::ALT),
            with(KeyCode::Char('n'), KeyModifiers::CONTROL),
        ] {
            app.handle_key(down);
        }
        assert_eq!(app.selected(), 2); // stops at the last row
        for up in [
            key(KeyCode::Up),
            with(KeyCode::Char('k'), KeyModifiers::ALT),
            with(KeyCode::Char('p'), KeyModifiers::CONTROL),
        ] {
            app.handle_key(up);
        }
        assert_eq!(app.selected(), 0);
        app.handle_key(key(KeyCode::Down));
        type_text(&mut app, "d");
        assert_eq!(app.selected(), 0); // a new filter starts at the top
    }

    #[test]
    fn enter_accepts_the_selected_row() {
        let mut app = app(Mode::Open);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Step::Accept("fix-login".into()));
        type_text(&mut app, "zzz");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Step::Stay); // nothing to open
    }

    #[test]
    fn in_create_mode_an_unmatched_query_is_a_new_branch() {
        let mut app = app(Mode::Create);
        type_text(&mut app, " feature/new ");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Step::Accept("feature/new".into()));
        app.handle_key(with(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut app, "fea");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Step::Accept("feat".into())); // a match wins
        let mut empty = App::new(Mode::BASE.to_vec(), Mode::Create, openers());
        assert_eq!(empty.handle_key(key(KeyCode::Enter)), Step::Stay);
    }

    #[test]
    fn esc_and_ctrl_c_quit_and_other_keys_do_nothing() {
        let mut app = app(Mode::Open);
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Step::Quit);
        assert_eq!(app.handle_key(with(KeyCode::Char('c'), KeyModifiers::CONTROL)), Step::Quit);
        assert_eq!(app.handle_key(key(KeyCode::F(5))), Step::Stay);
        assert_eq!(app.handle_key(with(KeyCode::Char('x'), KeyModifiers::ALT)), Step::Stay);
        assert_eq!(app.query(), "");
    }

    #[test]
    fn new_rows_start_the_query_over() {
        let mut app = app(Mode::Open);
        type_text(&mut app, "fix");
        app.set_rows(vec![Row::new("only", "", "")]);
        assert_eq!((app.query(), names(&app)), ("", vec!["only"]));
        assert_eq!(app.modes().len(), 4);
        assert_eq!(app.openers().len(), 2);
    }
}
