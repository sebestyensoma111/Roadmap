use crate::devices::{self, Disk, Report, Result, USB_TYPES};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Tabs},
    Frame, Terminal,
};
use std::{
    io::{self, IsTerminal},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Duration,
};

const ACCENT: Color = Color::Cyan;
use crate::i18n::{self, message, tr};

const VIEWS: [&str; 3] = ["1 Összefoglaló", "2 Teljes jelentés", "3 JSON"];
type SmartJob = Receiver<Result<Report>>;

struct App {
    disks: Vec<Disk>,
    list: ListState,
    view: usize,
    reports: [String; 3],
    offset: u16,
    column: u16,
    job: Option<SmartJob>,
    status: String,
    adapter: Option<usize>,
    error: bool,
}

impl App {
    fn new(disks: Vec<Disk>) -> Self {
        let list = ListState::default().with_selected((!disks.is_empty()).then_some(0));
        let mut app = Self {
            disks,
            list,
            view: 0,
            reports: Default::default(),
            offset: 0,
            column: 0,
            job: None,
            status: String::new(),
            adapter: None,
            error: false,
        };
        app.clear_report();
        app
    }

    fn clear_report(&mut self) {
        self.reports = Default::default();
        self.offset = 0;
        self.column = 0;
        self.error = false;
        self.status = if self.disks.is_empty() {
            tr("Nincs meghajtó. Az R frissíti a listát.").into()
        } else {
            tr("Enter – SMART-lekérdezés").into()
        };
    }

    fn navigate(&mut self, change: isize) {
        if let Some(index) = self.list.selected() {
            let next = index.saturating_add_signed(change).min(self.disks.len() - 1);
            if next != index {
                self.list.select(Some(next));
                self.clear_report();
            }
        }
    }

    fn refresh(&mut self) {
        let path = self.list.selected().map(|index| self.disks[index].path.clone());
        match devices::disks() {
            Ok(disks) => {
                let previous = path.and_then(|path| disks.iter().position(|disk| disk.path == path));
                self.list = ListState::default().with_selected(previous.or_else(|| (!disks.is_empty()).then_some(0)));
                self.disks = disks;
                self.clear_report();
            }
            Err(error) => {
                self.status = message("Lista frissítése: {}", &[&i18n::error(&devices::clean(&error.to_string()))]);
                self.error = true;
            }
        }
    }

    fn read(&mut self) {
        if let Some(index) = self.list.selected() {
            let path = self.disks[index].path.clone();
            let adapter = self.adapter.map(|index| USB_TYPES[index]);
            let (sender, receiver) = mpsc::channel();
            self.clear_report();
            self.status = message("SMART-adatok lekérése · {}", &[&path]);
            self.job = Some(receiver);
            thread::spawn(move || {
                let _ = sender.send(devices::read_smart(&path, adapter));
            });
        }
    }

    fn receive(&mut self) -> bool {
        let Some(job) = &self.job else { return false };
        let result = match job.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("A SMART-lekérdezés megszakadt.".into()),
        };
        self.job = None;
        match result {
            Ok(report) => {
                self.reports = [report.summary(), report.full(), report.json()];
                if report.code == 0 {
                    self.status = message("SMART-jelentés · {}", &[&report.path]);
                } else {
                    self.error = true;
                    self.status = message("SMART állapotkód: {} · {}", &[&report.code.to_string(), &report.path]);
                }
            }
            Err(error) => {
                self.error = true;
                self.status = i18n::error(&devices::clean(&error.to_string()));
            }
        }
        true
    }

    fn scroll(&mut self, amount: isize, last: u16) {
        self.offset = (self.offset as isize).saturating_add(amount).clamp(0, last as isize) as u16;
    }
}

fn draw(frame: &mut Frame<'_>, app: &mut App) -> (u16, u16, u16) {
    let [header, body, status, help] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(1),
        Constraint::Length(2),
    ]).areas(frame.area());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ROADMAP ", Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::raw(format!("  {}", tr("Meghajtók és SMART"))),
        ])),
        header,
    );
    let [devices, right] = if body.width >= 92 {
        Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)]).areas(body)
    } else {
        Layout::vertical([Constraint::Length((body.height / 3).clamp(3, 8)), Constraint::Min(0)]).areas(body)
    };
    let disks: Vec<_> = app.disks.iter().map(|disk| ListItem::new(disk.label())).collect();
    let list = List::new(disks)
        .block(panel(tr("Meghajtók")))
        .highlight_symbol("› ")
        .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, devices, &mut app.list);

    let [tabs, detail] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(right);
    frame.render_widget(
        Tabs::new(VIEWS.map(tr)).select(app.view)
            .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        tabs,
    );
    let partitions;
    let report = app.reports[app.view].as_str();
    let has_report = !report.is_empty();
    let text = if has_report {
        report
    } else {
        partitions = app.list.selected().map(|index| {
            let disk = &app.disks[index];
            let rows: Vec<_> = disk.partitions().collect();
            format!("{}\n\n{}\n{}", disk.label(), tr("Partíciók"), if rows.is_empty() { "–".into() } else { rows.join("\n") })
        }).unwrap_or_default();
        &partitions
    };

    let page = detail.height.saturating_sub(2).max(1);
    let max_offset = text.lines().count().saturating_sub(usize::from(page));
    let last = u16::try_from(max_offset).unwrap_or(u16::MAX);
    let max_column = text.lines().map(|line| line.chars().count()).max().unwrap_or(0)
        .saturating_sub(usize::from(detail.width.saturating_sub(2)));
    let rightmost = u16::try_from(max_column).unwrap_or(u16::MAX);
    app.offset = app.offset.min(last);
    app.column = app.column.min(rightmost);
    frame.render_widget(
        Paragraph::new(text).block(panel(if has_report { "SMART" } else { tr("Meghajtóadatok") }))
            .scroll((app.offset, app.column)),
        detail,
    );
    if max_offset > 0 && detail.height > 2 && detail.width > 1 {
        let gutter = Rect::new(detail.right() - 1, detail.y + 1, 1, detail.height - 2);
        let mut scrollbar = ScrollbarState::new(max_offset + 1).position(usize::from(app.offset));
        frame.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight), gutter, &mut scrollbar);
    }
    let status_color = if app.error { Color::Yellow } else { ACCENT };
    frame.render_widget(Paragraph::new(app.status.as_str()).style(Style::default().fg(status_color)), status);
    let adapter = app.adapter.map(|index| USB_TYPES[index]).unwrap_or("auto");
    frame.render_widget(
        Paragraph::new(format!("{}\n{}  U USB: {adapter}",
            tr("↑↓ Meghajtó   Enter SMART   R Frissítés   Q Kilépés"),
            tr("PgUp/PgDn vagy J/K Görgetés   ←→ Oldalra"),
        )),
        help,
    );
    (page, last, rightmost)
}

fn panel(title: &str) -> Block<'static> {
    Block::default().title(format!(" {title} ")).borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray))
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

pub fn run() -> Result<()> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return Err("A TUI-hoz terminál szükséges. Szkripthez használd a list vagy smart parancsot.".into());
    }
    let mut app = App::new(devices::disks()?);
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut scroll = (1, 0, 0);
    let mut redraw = true;
    loop {
        if app.receive() || redraw {
            terminal.draw(|frame| scroll = draw(frame, &mut app))?;
            redraw = false;
        }
        let (page, last, rightmost) = scroll;
        if app.job.is_some() && !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let key = match event::read()? {
            Event::Key(key) => key,
            Event::Resize(_, _) => { redraw = true; continue }
            _ => continue,
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if matches!(key.code, KeyCode::Char('q' | 'Q') | KeyCode::Esc)
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)) {
            break;
        }
        redraw = true;
        match key.code {
            KeyCode::Up if app.job.is_none() => app.navigate(-1),
            KeyCode::Down if app.job.is_none() => app.navigate(1),
            KeyCode::Char('r' | 'R') if app.job.is_none() => app.refresh(),
            KeyCode::Char('u' | 'U') if app.job.is_none() => {
                app.adapter = match app.adapter {
                    None => Some(0),
                    Some(index) if index + 1 < USB_TYPES.len() => Some(index + 1),
                    _ => None,
                };
                app.clear_report();
            }
            KeyCode::Enter if app.job.is_none() => app.read(),
            KeyCode::Char(view @ '1'..='3') => {
                app.view = view as usize - '1' as usize;
                app.offset = 0;
                app.column = 0;
            }
            KeyCode::PageDown => app.scroll(page as isize, last),
            KeyCode::PageUp => app.scroll(-(page as isize), last),
            KeyCode::Char('j') => app.scroll(1, last),
            KeyCode::Char('k') => app.scroll(-1, last),
            KeyCode::Home => app.offset = 0,
            KeyCode::End => app.offset = last,
            KeyCode::Left => app.column = app.column.saturating_sub(8),
            KeyCode::Right => app.column = app.column.saturating_add(8).min(rightmost),
            _ => {}
        }
    }
    Ok(())
}
