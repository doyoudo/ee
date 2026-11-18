use crate::{
    registry::{self, Entry, Error, Kind, Name, Scope},
    terminal::{ctrl, wrap, Frame, Terminal, Text, Tone},
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal::size,
};
use std::{io, mem};

const MIN_WIDTH: u16 = 48;
const MIN_HEIGHT: u16 = 12;
const FIRST_ROW: usize = 6;
const FOOTER_ROWS: usize = 3;

pub fn run() -> io::Result<()> {
    let mut terminal = Terminal::open()?;
    let mut app = App::new();
    app.refresh(None);
    loop {
        let (width, height) = size()?;
        let too_small = width < MIN_WIDTH || height < MIN_HEIGHT;
        let mut frame = Frame::new(width, height);
        if too_small {
            frame.line(0, "ee  ·  Resize to 48 x 12", Tone::Accent);
            frame.line(1, "Ctrl+Q Quit", Tone::Plain);
        } else {
            app.draw(&mut frame);
        }
        terminal.draw(frame)?;
        loop {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if ctrl(key, 'q')
                        || (!too_small && !app.key(key, app.navigation_page(height as usize)))
                    {
                        return Ok(());
                    }
                    break;
                }
                Event::Paste(text) if !too_small => {
                    app.paste(&text);
                    break;
                }
                Event::Resize(_, _) => break,
                _ => {}
            }
        }
    }
}

struct App {
    scope: Scope,
    names: [Option<Vec<Name>>; 2],
    filtered: Vec<usize>,
    query: Text,
    searching: bool,
    selected: usize,
    mode: Mode,
    status: (Tone, String),
}

#[derive(Default)]
enum Mode {
    #[default]
    List,
    View(View),
    Edit(Draft),
    Path(PathEdit),
    Delete(Entry),
    Help,
}

struct View {
    entry: Entry,
    decoded: Option<(Kind, String)>,
    offset: usize,
}

struct Draft {
    entry: Option<Entry>,
    name: Text,
    value: Text,
    kind: Kind,
    field: Field,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Value,
    Kind,
}

impl Draft {
    fn new() -> Self {
        Self {
            entry: None,
            name: Text::default(),
            value: Text::default(),
            kind: Kind::Plain,
            field: Field::Name,
        }
    }

    fn existing(entry: Entry, kind: Kind, text: String) -> Self {
        Self {
            name: Text::new(entry.name.text.clone()),
            entry: Some(entry),
            kind,
            value: Text::new(text),
            field: Field::Value,
        }
    }

    fn input(&mut self) -> Option<(&mut Text, bool)> {
        match self.field {
            Field::Name => Some((&mut self.name, true)),
            Field::Value => Some((&mut self.value, false)),
            Field::Kind => None,
        }
    }

    fn move_field(&mut self, back: bool) {
        self.field = match (self.field, back) {
            (Field::Name, false) | (Field::Kind, true) => Field::Value,
            (Field::Value, false) | (Field::Name, true) => Field::Kind,
            (Field::Kind, false) | (Field::Value, true) => Field::Name,
        };
    }

    fn toggle_kind(&mut self) {
        self.kind = if self.kind == Kind::Plain {
            Kind::Expandable
        } else {
            Kind::Plain
        };
    }

    fn is_path(&self) -> bool {
        if let Some(entry) = &self.entry {
            entry.name.is_path()
        } else {
            self.name.value.eq_ignore_ascii_case("Path")
        }
    }
}

struct PathEdit {
    draft: Draft,
    items: Vec<String>,
    selected: usize,
    item: Option<PathItem>,
}

struct PathItem {
    index: usize,
    adding: bool,
    text: Text,
}

impl PathEdit {
    fn add_item(&mut self) {
        self.item = Some(PathItem {
            index: (self.selected + 1).min(self.items.len()),
            adding: true,
            text: Text::default(),
        });
    }

    fn edit_item(&mut self) {
        if let Some(text) = self.items.get(self.selected) {
            self.item = Some(PathItem {
                index: self.selected,
                adding: false,
                text: Text::new(text.clone()),
            });
        }
    }

    fn apply_item(&mut self) -> Result<(), &'static str> {
        let Some(item) = self.item.as_ref() else {
            return Ok(());
        };
        if path_items(&item.text.value).is_none_or(|parts| parts.len() > 1) {
            return Err("Enter one item; use F2 from the Path list for raw text.");
        }
        let item = self.item.take().unwrap();
        if item.adding {
            self.items.insert(item.index, item.text.value);
        } else {
            self.items[item.index] = item.text.value;
        }
        self.selected = item.index;
        Ok(())
    }

    fn remove_item(&mut self) {
        if self.selected < self.items.len() {
            self.items.remove(self.selected);
            self.selected = self.selected.min(self.items.len().saturating_sub(1));
        }
    }

    fn move_item(&mut self, down: bool) {
        let adjacent = if down {
            self.selected.checked_add(1)
        } else {
            self.selected.checked_sub(1)
        };
        if let Some(adjacent) = adjacent.filter(|&index| index < self.items.len()) {
            self.items.swap(self.selected, adjacent);
            self.selected = adjacent;
        }
    }

    fn into_draft(self) -> Draft {
        let mut draft = self.draft;
        draft.value = Text::new(self.items.join(";"));
        draft
    }
}

fn path_items(text: &str) -> Option<Vec<String>> {
    let mut items = Vec::new();
    if text.is_empty() {
        return Some(items);
    }
    let mut quoted = false;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ';' if !quoted => {
                items.push(text[start..index].to_owned());
                start = index + 1;
            }
            _ => {}
        }
    }
    if quoted {
        return None;
    }
    items.push(text[start..].to_owned());
    Some(items)
}

impl App {
    fn new() -> Self {
        Self {
            scope: Scope::User,
            names: [None, None],
            filtered: Vec::new(),
            query: Text::default(),
            searching: false,
            selected: 0,
            mode: Mode::List,
            status: (Tone::Plain, String::new()),
        }
    }

    fn name(&self) -> Option<&Name> {
        self.filtered
            .get(self.selected)
            .map(|&index| &self.names()[index])
    }

    fn names(&self) -> &[Name] {
        self.names[scope_index(self.scope)]
            .as_deref()
            .unwrap_or_default()
    }

    fn filter(&mut self, preferred: Option<&Name>) {
        let query: Vec<_> = self.query.value.encode_utf16().collect();
        let names = self.names[scope_index(self.scope)]
            .as_deref()
            .unwrap_or_default();
        self.filtered.clear();
        self.filtered.extend(
            names
                .iter()
                .enumerate()
                .filter_map(|(index, name)| name.contains(&query).then_some(index)),
        );
        self.selected = preferred
            .and_then(|name| {
                self.filtered
                    .iter()
                    .position(|&index| name.matches(&names[index]))
            })
            .unwrap_or(0);
    }

    fn notice(&mut self, tone: Tone, message: impl Into<String>) {
        self.status = (tone, message.into());
    }

    fn reload(&mut self, scope: Scope) -> Result<(), Error> {
        self.names[scope_index(scope)] = Some(scope.store().names()?);
        Ok(())
    }

    fn refresh(&mut self, preferred: Option<&Name>) {
        if let Err(error) = self.reload(self.scope) {
            if self.status.1.is_empty() {
                self.report(error);
            } else {
                self.status.0 = Tone::Warning;
                self.status.1.push_str(" List reload failed.");
            }
        }
        self.filter(preferred);
    }

    fn manual_refresh(&mut self, all: bool) {
        let preferred = self.name().cloned();
        if all {
            let user = self.reload(Scope::User);
            let system = self.reload(Scope::System);
            match (user, system) {
                (Ok(()), Ok(())) => self.notice(Tone::Good, "Reloaded User and System lists."),
                (Err(error), Ok(())) => self.notice(
                    Tone::Warning,
                    format!("System reloaded; User failed: {error}"),
                ),
                (Ok(()), Err(error)) => self.notice(
                    Tone::Warning,
                    format!("User reloaded; System failed: {error}"),
                ),
                (Err(_), Err(_)) => self.notice(Tone::Error, "Could not reload User or System."),
            }
        } else {
            match self.reload(self.scope) {
                Ok(()) => self.notice(
                    Tone::Good,
                    format!("Reloaded {} list.", scope_label(self.scope)),
                ),
                Err(error) => self.report(error),
            }
        }
        self.filter(preferred.as_ref());
    }

    fn report(&mut self, error: Error) {
        let message = if self.scope == Scope::System && error.access_denied() {
            "Access denied. Reopen ee as Administrator.".into()
        } else {
            error.to_string()
        };
        self.notice(Tone::Error, message);
    }

    fn saved(&mut self, action: &str, preferred: Option<&Name>) {
        if registry::notify() {
            self.notice(
                Tone::Good,
                format!("{action}. Open a fresh terminal via Start."),
            );
        } else {
            self.notice(
                Tone::Warning,
                format!("{action}; Windows notification failed."),
            );
        }
        self.refresh(preferred);
    }

    fn save(&mut self, draft: &Draft) -> bool {
        let new;
        let (name, original) = if let Some(entry) = &draft.entry {
            (&entry.name, Some(&entry.raw))
        } else {
            match Name::new(&draft.name.value) {
                Ok(name) => new = name,
                Err(error) => {
                    self.report(error);
                    return false;
                }
            }
            (&new, None)
        };
        match self
            .scope
            .store()
            .save(name, original, draft.kind, &draft.value.value)
        {
            Ok(true) => self.saved("Saved", Some(name)),
            Ok(false) => self.notice(Tone::Plain, "No changes."),
            Err(error) => {
                self.report(error);
                return false;
            }
        }
        true
    }

    fn open_path(&mut self, mut draft: Draft) -> Mode {
        if let Some(items) = path_items(&draft.value.value) {
            draft.field = Field::Value;
            draft.value = Text::default();
            Mode::Path(PathEdit {
                draft,
                items,
                selected: 0,
                item: None,
            })
        } else {
            self.notice(Tone::Warning, "Unbalanced quotes. Edit the raw Path value.");
            Mode::Edit(draft)
        }
    }

    fn edit_entry(&mut self, entry: Entry, decoded: Option<(Kind, String)>) -> Mode {
        if !entry.name.editable() || decoded.is_none() {
            self.notice(Tone::Warning, "This registry entry is read-only.");
            return Mode::View(View {
                entry,
                decoded,
                offset: 0,
            });
        }
        let (kind, text) = decoded.unwrap();
        let draft = Draft::existing(entry, kind, text);
        if draft.is_path() {
            self.open_path(draft)
        } else {
            Mode::Edit(draft)
        }
    }

    fn list_key(&mut self, key: KeyEvent, page: usize) -> Mode {
        let command = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            _ if ctrl(key, 'r') => self.manual_refresh(true),
            _ if ctrl(key, 'f') => self.searching = true,
            KeyCode::F(5) => self.manual_refresh(false),
            KeyCode::Tab | KeyCode::BackTab => {
                self.scope = if self.scope == Scope::User {
                    Scope::System
                } else {
                    Scope::User
                };
                self.searching = false;
                if self.names[scope_index(self.scope)].is_some() {
                    self.filter(None);
                } else {
                    self.refresh(None);
                }
            }
            KeyCode::Esc => {
                self.query = Text::default();
                self.searching = false;
                self.filter(None);
            }
            KeyCode::F(1) => return Mode::Help,
            KeyCode::Enter => self.searching = false,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                move_selection(key.code, &mut self.selected, self.filtered.len(), page);
                self.searching = false;
            }
            _ if self.searching => {
                let result = self.query.key(key, true);
                if self.input_result(result) {
                    self.filter(None);
                }
            }
            KeyCode::Char('/') if !command => self.searching = true,
            KeyCode::Char('?') if !command => return Mode::Help,
            KeyCode::Char('r' | 'R') if !command => self.manual_refresh(false),
            KeyCode::Char('a' | 'A') if !command => return Mode::Edit(Draft::new()),
            KeyCode::Char('e' | 'E') if !command => return self.open_selected(key.code),
            KeyCode::Right | KeyCode::Delete => return self.open_selected(key.code),
            _ => {
                move_selection(key.code, &mut self.selected, self.filtered.len(), page);
            }
        }
        Mode::List
    }

    fn open_selected(&mut self, action: KeyCode) -> Mode {
        if let Some(name) = self.name().cloned() {
            match self.scope.store().read(name) {
                Ok(entry) => {
                    let decoded = entry.raw.decode();
                    return match action {
                        KeyCode::Delete if entry.name.editable() && decoded.is_some() => {
                            Mode::Delete(entry)
                        }
                        KeyCode::Delete => {
                            self.notice(Tone::Warning, "This registry entry is read-only.");
                            Mode::List
                        }
                        KeyCode::Right => Mode::View(View {
                            entry,
                            decoded,
                            offset: 0,
                        }),
                        _ => self.edit_entry(entry, decoded),
                    };
                }
                Err(error) => self.report(error),
            }
        }
        Mode::List
    }

    fn input_result(&mut self, result: Result<bool, &'static str>) -> bool {
        match result {
            Ok(changed) => changed,
            Err(error) => {
                self.notice(Tone::Error, error);
                false
            }
        }
    }

    fn edit_key(&mut self, mut draft: Draft, key: KeyEvent) -> Mode {
        if ctrl(key, 's') || (confirm(key) && draft.field == Field::Value) {
            return if self.save(&draft) {
                Mode::List
            } else {
                Mode::Edit(draft)
            };
        }
        match key.code {
            KeyCode::Esc => return Mode::List,
            KeyCode::Tab | KeyCode::Down if draft.entry.is_none() => draft.move_field(false),
            KeyCode::BackTab | KeyCode::Up if draft.entry.is_none() => draft.move_field(true),
            KeyCode::Enter if confirm(key) => draft.field = Field::Value,
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if draft.field == Field::Kind => {
                draft.toggle_kind();
            }
            KeyCode::F(2) if draft.is_path() => return self.open_path(draft),
            KeyCode::F(3) if draft.entry.is_none() => draft.toggle_kind(),
            _ => {
                if let Some((input, single_line)) = draft.input() {
                    let result = input.key(key, single_line);
                    self.input_result(result);
                }
            }
        }
        Mode::Edit(draft)
    }

    fn path_key(&mut self, mut path: PathEdit, key: KeyEvent, page: usize) -> Mode {
        if let Some(item) = &mut path.item {
            match key.code {
                KeyCode::Esc => path.item = None,
                KeyCode::Enter if confirm(key) => {
                    if let Err(error) = path.apply_item() {
                        self.notice(Tone::Error, error);
                    }
                }
                _ => {
                    let result = item.text.key(key, false);
                    self.input_result(result);
                }
            }
            return Mode::Path(path);
        }
        if ctrl(key, 's') || confirm(key) {
            path.draft.value = Text::new(path.items.join(";"));
            return if self.save(&path.draft) {
                Mode::List
            } else {
                path.draft.value = Text::default();
                Mode::Path(path)
            };
        }
        match key.code {
            KeyCode::Esc => return Mode::List,
            KeyCode::F(2) => return Mode::Edit(path.into_draft()),
            KeyCode::Right => path.edit_item(),
            KeyCode::Char('a' | 'A') if key.kind == KeyEventKind::Press => path.add_item(),
            KeyCode::Delete => path.remove_item(),
            KeyCode::Up | KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                path.move_item(key.code == KeyCode::Down);
            }
            _ => {
                move_selection(key.code, &mut path.selected, path.items.len(), page);
            }
        }
        Mode::Path(path)
    }

    fn key(&mut self, key: KeyEvent, page: usize) -> bool {
        if matches!(self.mode, Mode::List)
            && key.code == KeyCode::Esc
            && !self.searching
            && self.query.value.is_empty()
        {
            return false;
        }
        self.status.1.clear();
        self.mode = match mem::take(&mut self.mode) {
            Mode::List => self.list_key(key, page),
            Mode::View(mut view) => match key.code {
                KeyCode::Left | KeyCode::Esc => Mode::List,
                KeyCode::Char('e' | 'E') if !key.modifiers.contains(KeyModifiers::ALT) => {
                    self.edit_entry(view.entry, view.decoded)
                }
                _ => {
                    move_selection(key.code, &mut view.offset, usize::MAX, page);
                    Mode::View(view)
                }
            },
            Mode::Edit(draft) => self.edit_key(draft, key),
            Mode::Path(path) => self.path_key(path, key, page),
            Mode::Delete(entry) => match key.code {
                KeyCode::Esc => Mode::List,
                KeyCode::Enter if confirm(key) => match self.scope.store().delete(&entry) {
                    Ok(()) => {
                        self.saved("Deleted", None);
                        Mode::List
                    }
                    Err(error) => {
                        self.report(error);
                        Mode::Delete(entry)
                    }
                },
                _ => Mode::Delete(entry),
            },
            Mode::Help
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Left | KeyCode::F(1) | KeyCode::Char('?')
                ) =>
            {
                Mode::List
            }
            Mode::Help => Mode::Help,
        };
        true
    }

    fn paste(&mut self, text: &str) {
        self.status.1.clear();
        let result = match &mut self.mode {
            Mode::List if self.searching => self.query.paste(text, true),
            Mode::Edit(draft) => {
                let Some((input, single_line)) = draft.input() else {
                    return;
                };
                input.paste(text, single_line)
            }
            Mode::Path(PathEdit {
                item: Some(item), ..
            }) => item.text.paste(text, false),
            _ => return,
        };
        if self.input_result(result) && matches!(self.mode, Mode::List) {
            self.filter(None);
        }
    }

    fn navigation_page(&self, height: usize) -> usize {
        match self.mode {
            Mode::View(_) => detail_height(height) - 5,
            Mode::Path(_) => detail_height(height) - 6,
            _ => page_size(height),
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        self.draw_list(frame);
        let height = detail_height(frame.height);
        match &mut self.mode {
            Mode::List => {}
            Mode::View(view) => draw_view(frame.popup("View variable", height), view),
            Mode::Edit(draft) => {
                let title = if draft.entry.is_some() {
                    "Edit variable"
                } else {
                    "New variable"
                };
                draw_draft(frame.popup(title, 9), draft, &self.status);
            }
            Mode::Path(path) => draw_path(frame.popup("Edit Path", height), path, &self.status),
            Mode::Delete(entry) => {
                let body = frame.popup("Delete variable", 8);
                body.line(0, " Delete this variable?", Tone::Warning);
                body.spans(
                    1,
                    &[(" ", Tone::Plain), (show_name(&entry.name), Tone::Title)],
                );
                body.line(2, " Removes it from this scope.", Tone::Plain);
                draw_status(body, 3, &self.status);
                shortcuts(body, 4, &[("Enter", "Delete"), ("Esc", "Cancel")]);
            }
            Mode::Help => {
                let body = frame.popup("Keyboard shortcuts", 9);
                shortcuts(body, 0, &[("/ or Ctrl+F", "Find"), ("Tab", "Scope")]);
                shortcuts(
                    body,
                    1,
                    &[("→/←", "View/back"), ("A", "Add"), ("E", "Edit")],
                );
                shortcuts(body, 2, &[("R", "Reload"), ("Ctrl+R", "Both lists")]);
                body.line(3, " Reload only reads the registry.", Tone::Plain);
                body.line(4, " Save writes to Windows.", Tone::Plain);
                shortcuts(body, 6, &[("Esc", "Back"), ("Ctrl+Q", "Quit")]);
            }
        }
    }

    fn draw_list(&self, frame: &mut Frame) {
        use Tone::{Accent, Plain, Rule, Tab, Title};
        let rail = "─".repeat(frame.width - 2);
        frame.line(0, &format!("╭{rail}╮"), Rule);
        const TITLE: &str = "  ·  Environment variables";
        const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"), "  ");
        let gap = frame
            .width
            .saturating_sub(6 + TITLE.chars().count() + VERSION.len());
        frame.spans(
            1,
            &[
                ("│", Rule),
                ("  ee", Accent),
                (TITLE, Title),
                (&" ".repeat(gap), Plain),
                (VERSION, Plain),
                ("│", Rule),
            ],
        );
        frame.line(2, &format!("╰{rail}╯"), Rule);

        let tab = |scope| if self.scope == scope { Tab } else { Plain };
        frame.spans(
            3,
            &[
                ("  ", Plain),
                (" User ", tab(Scope::User)),
                ("  ", Plain),
                (" System ", tab(Scope::System)),
            ],
        );
        if self.searching {
            frame.field(4, "  › Searching: ", &self.query, Accent);
        } else if !self.query.value.is_empty() {
            frame.spans(4, &[("  › Filtered: ", Title), (&self.query.value, Plain)]);
        }
        let names = self.names();
        let count = if self.query.value.is_empty() {
            format!("{} variables", names.len())
        } else {
            format!("{} of {}", self.filtered.len(), names.len())
        };
        let rule = "─".repeat(frame.width.saturating_sub(20 + count.len()));
        frame.spans(
            5,
            &[
                ("  VARIABLE NAME  ", Title),
                (&rule, Rule),
                ("  ", Plain),
                (&count, Plain),
            ],
        );

        let page = page_size(frame.height);
        let first = visible_start(self.selected, page);
        for (row, &index) in self.filtered.iter().enumerate().skip(first).take(page) {
            selected_line(
                frame,
                FIRST_ROW + row - first,
                row == self.selected,
                show_name(&names[index]),
            );
        }
        if self.filtered.is_empty() {
            frame.line(FIRST_ROW, "    No matching variables.", Plain);
        }
        if matches!(self.mode, Mode::List) {
            draw_status(frame, frame.height - 3, &self.status);
        }
        let actions: &[(&str, &str)] = if self.searching {
            &[
                ("Enter", "Keep filter"),
                ("Esc", "Clear"),
                ("Ctrl+V", "Paste"),
            ]
        } else {
            &[
                ("→", "View"),
                ("A", "Add"),
                ("E", "Edit"),
                ("Del", "Delete"),
                ("?", ""),
            ]
        };
        shortcuts(frame, frame.height - 2, actions);
        shortcuts(
            frame,
            frame.height - 1,
            &[
                ("/", "Find"),
                ("Tab", "Scope"),
                ("R", "Reload"),
                ("Ctrl+R", "All"),
            ],
        );
    }
}

fn draw_view(body: &mut Frame, view: &mut View) {
    use Tone::{Plain, Title, Warning};
    body.spans(
        0,
        &[(" Name   ", Title), (show_name(&view.entry.name), Plain)],
    );
    let footer = body.height - 1;
    if let Some((kind, value)) = &view.decoded {
        body.spans(1, &[(" Type   ", Title), (kind.label(), Plain)]);
        let page = footer - 2;
        let lines = wrap(value, body.width - 2);
        view.offset = view.offset.min(lines.len().saturating_sub(page));
        if value.is_empty() {
            body.line(2, " (empty value)", Plain);
        } else {
            for (row, line) in lines.iter().skip(view.offset).take(page).enumerate() {
                body.spans(2 + row, &[(" ", Plain), (line, Plain)]);
            }
        }
    } else {
        body.line(
            1,
            &format!(" Registry type {} (read-only)", view.entry.raw.kind),
            Warning,
        );
        body.line(2, " Not a supported string value.", Plain);
    }
    if view.decoded.is_some() && view.entry.name.editable() {
        shortcuts(
            body,
            footer,
            &[("←/Esc", "Back"), ("E", "Edit"), ("↑/↓", "Scroll")],
        );
    } else {
        shortcuts(body, footer, &[("←/Esc", "Back"), ("↑/↓", "Scroll")]);
    }
}

fn draw_draft(body: &mut Frame, draft: &Draft, status: &(Tone, String)) {
    use Tone::{Accent, Plain, Title};
    for (row, (field, caption, input)) in [
        (Field::Name, "Name", &draft.name),
        (Field::Value, "Value", &draft.value),
    ]
    .into_iter()
    .enumerate()
    {
        if draft.field == field {
            body.field(row, &format!(" › {caption:<7}"), input, Accent);
        } else {
            body.spans(
                row,
                &[(&format!("   {caption:<7}"), Title), (&input.value, Plain)],
            );
        }
    }
    if draft.field == Field::Kind {
        body.spans(
            2,
            &[
                (" › Type   ", Accent),
                ("‹ ", Accent),
                (draft.kind.label(), Plain),
                (" ›", Accent),
            ],
        );
    } else {
        body.spans(2, &[("   Type   ", Title), (draft.kind.label(), Plain)]);
    }
    draw_status(body, 3, status);
    let confirm = if draft.field == Field::Value {
        "Save"
    } else {
        "Next"
    };
    shortcuts(body, 4, &[("Enter", confirm), ("Esc", "Cancel")]);
    if draft.entry.is_none() {
        shortcuts(body, 5, &[("Tab/↑↓", "Field"), ("F3", "Type")]);
    }
    if draft.is_path() {
        shortcuts(body, 6, &[("Ctrl+V", "Paste"), ("F2", "Path items")]);
    } else {
        shortcuts(body, 6, &[("Ctrl+V", "Paste"), ("Ctrl+U", "Clear")]);
    }
}

fn draw_path(body: &mut Frame, path: &PathEdit, status: &(Tone, String)) {
    use Tone::{Accent, Plain, Title};
    body.spans(
        0,
        &[
            (" ", Plain),
            (&path.draft.name.value, Title),
            (&format!("  ·  {} items", path.items.len()), Plain),
        ],
    );
    let status_row = body.height - 3;
    let page = status_row - 1 - usize::from(path.item.is_some());
    let first = visible_start(path.selected, page);
    for (row, value) in path.items.iter().enumerate().skip(first).take(page) {
        let text = if value.is_empty() {
            "(empty item)"
        } else {
            value
        };
        selected_line(
            body,
            1 + row - first,
            row == path.selected,
            &format!("{}. {text}", row + 1),
        );
    }
    if path.items.is_empty() {
        body.line(1, " No Path items yet.", Plain);
    }
    draw_status(body, status_row, status);
    if let Some(item) = &path.item {
        let label = if item.adding {
            " › Add item   "
        } else {
            " › Edit item  "
        };
        body.field(status_row - 1, label, &item.text, Accent);
        shortcuts(
            body,
            status_row + 1,
            &[("Enter", "Confirm"), ("Esc", "Back")],
        );
        shortcuts(
            body,
            status_row + 2,
            &[("Ctrl+V", "Paste"), ("Ctrl+U", "Clear")],
        );
    } else {
        shortcuts(
            body,
            status_row + 1,
            &[("Enter", "Save"), ("→", "Edit"), ("A", "Add")],
        );
        shortcuts(
            body,
            status_row + 2,
            &[("Del", "Remove"), ("^↑↓", "Move"), ("F2", "Raw")],
        );
    }
}

fn draw_status(frame: &mut Frame, row: usize, status: &(Tone, String)) {
    frame.spans(row, &[(" ", Tone::Plain), (&status.1, status.0)]);
}

fn detail_height(height: usize) -> usize {
    (height - 3).min(15)
}

fn show_name(name: &Name) -> &str {
    if name.text.is_empty() {
        "(default registry value)"
    } else {
        &name.text
    }
}

fn selected_line(frame: &mut Frame, row: usize, selected: bool, text: &str) {
    let (prefix, tone) = if selected {
        ("  › ", Tone::Accent)
    } else {
        ("    ", Tone::Plain)
    };
    frame.spans(row, &[(prefix, tone), (text, tone)]);
}

fn shortcuts(frame: &mut Frame, row: usize, items: &[(&str, &str)]) {
    let mut spans = Vec::with_capacity(items.len() * 4);
    for (index, &(key, text)) in items.iter().enumerate() {
        spans.push((if index == 0 { " " } else { " · " }, Tone::Plain));
        spans.push((key, Tone::Accent));
        if !text.is_empty() {
            spans.push((" ", Tone::Plain));
            spans.push((text, Tone::Plain));
        }
    }
    frame.spans(row, &spans);
}

fn scope_index(scope: Scope) -> usize {
    usize::from(scope == Scope::System)
}

fn scope_label(scope: Scope) -> &'static str {
    if scope == Scope::User {
        "User"
    } else {
        "System"
    }
}

fn confirm(key: KeyEvent) -> bool {
    key.code == KeyCode::Enter && key.kind == KeyEventKind::Press && key.modifiers.is_empty()
}

fn page_size(height: usize) -> usize {
    height - FIRST_ROW - FOOTER_ROWS
}

fn visible_start(selected: usize, page: usize) -> usize {
    selected / page * page
}

fn move_selection(code: KeyCode, selected: &mut usize, len: usize, page: usize) -> bool {
    let last = len.saturating_sub(1);
    *selected = match code {
        KeyCode::Up => selected.saturating_sub(1),
        KeyCode::Down => selected.saturating_add(1).min(last),
        KeyCode::PageUp => selected.saturating_sub(page),
        KeyCode::PageDown => selected.saturating_add(page).min(last),
        KeyCode::Home => 0,
        KeyCode::End => last,
        _ => return false,
    };
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_preserves_quotes_empty_entries_and_references() {
        let raw = ";C:\\one;\"C:\\two;three\";%SystemRoot%\\System32;c:\\One;;";
        assert_eq!(
            path_items(raw).unwrap(),
            [
                "",
                "C:\\one",
                "\"C:\\two;three\"",
                "%SystemRoot%\\System32",
                "c:\\One",
                "",
                ""
            ]
        );
        assert_eq!(path_items(raw).unwrap().join(";"), raw);
        assert!(path_items("C:\\one;\"C:\\unfinished").is_none());
        assert_eq!(path_items("").unwrap().join(";"), "");
        assert_eq!(path_items(";").unwrap(), ["", ""]);
    }

    #[test]
    fn path_stages_items_and_rejects_unquoted_separators() {
        let mut app = App::new();
        let mut draft = Draft::new();
        draft.name = Text::new("pAtH".into());
        draft.value = Text::new("a;;b".into());
        assert!(draft.is_path());
        let Mode::Path(mut path) = app.open_path(draft) else {
            panic!("expected Path list")
        };
        path.move_item(true);
        path.add_item();
        path.item.as_mut().unwrap().text = Text::new("x;y".into());
        assert!(path.apply_item().is_err());
        path.item.as_mut().unwrap().text = Text::new("\"x;y\"".into());
        path.apply_item().unwrap();
        path.selected = 1;
        path.edit_item();
        path.item.as_mut().unwrap().text = Text::new("%Temp%".into());
        path.apply_item().unwrap();
        path.selected = 3;
        path.remove_item();
        assert_eq!(path.into_draft().value.value, ";%Temp%;\"x;y\"");
    }

    #[test]
    fn search_filters_and_escape_clears_before_quitting() {
        let mut app = App::new();
        app.names[0] = Some(vec![
            Name::new("Alpha").unwrap(),
            Name::new("RUSTUP_HOME").unwrap(),
        ]);
        app.filter(None);
        assert!(app.key(KeyEvent::from(KeyCode::Char('u')), 8));
        app.paste("ignored outside search");
        assert!(app.query.value.is_empty());
        assert!(!app.searching);
        assert!(app.key(KeyEvent::from(KeyCode::Char('/')), 8));
        assert!(app.key(KeyEvent::from(KeyCode::Char('r')), 8));
        assert!(app.key(KeyEvent::from(KeyCode::Char('u')), 8));
        assert_eq!(app.name().unwrap().text, "RUSTUP_HOME");
        let mut frame = Frame::new(48, 12);
        app.draw(&mut frame);
        assert_eq!(
            frame.rendered(4),
            [("  › Searching: ", Tone::Accent), ("ru", Tone::Plain)]
        );
        assert!(app.key(KeyEvent::from(KeyCode::Enter), 8));
        assert!(matches!(app.mode, Mode::List));
        assert!(!app.searching);
        assert_eq!(app.filtered.len(), 1);
        app.draw(&mut frame);
        assert_eq!(
            frame.rendered(4),
            [("  › Filtered: ", Tone::Title), ("ru", Tone::Plain)]
        );
        assert_eq!(frame.rendered(11).last(), Some(&("All", Tone::Plain)));
        assert!(app.key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL), 8));
        assert!(app.key(KeyEvent::from(KeyCode::Backspace), 8));
        assert!(app.searching);
        assert_eq!(app.query.value, "r");
        assert!(app.key(KeyEvent::from(KeyCode::Esc), 8));
        assert_eq!(app.filtered.len(), 2);
        assert!(app.key(KeyEvent::from(KeyCode::Char('/')), 8));
        assert!(app.key(KeyEvent::from(KeyCode::Esc), 8));
        assert!(!app.key(KeyEvent::from(KeyCode::Esc), 8));
    }

    #[test]
    fn single_letter_actions_do_not_run_inside_search() {
        let mut app = App::new();
        app.key(KeyEvent::from(KeyCode::Char('a')), 8);
        assert!(matches!(app.mode, Mode::Edit(_)));
        app.key(KeyEvent::from(KeyCode::Esc), 8);
        app.key(KeyEvent::from(KeyCode::Char('?')), 8);
        assert!(matches!(app.mode, Mode::Help));
        app.key(KeyEvent::from(KeyCode::Char('?')), 8);
        app.key(KeyEvent::from(KeyCode::Char('/')), 8);
        for character in ['a', 'e', 'r', '?', '/'] {
            app.key(KeyEvent::from(KeyCode::Char(character)), 8);
        }
        assert!(matches!(app.mode, Mode::List));
        assert_eq!(app.query.value, "aer?/");
        assert!(app.names[0].is_none());
    }

    #[test]
    fn header_and_form_popup_keep_the_filtered_list_and_selected_row() {
        let mut app = App::new();
        app.names[0] = Some(vec![
            Name::new("First").unwrap(),
            Name::new("Second").unwrap(),
        ]);
        app.query = Text::new("s".into());
        app.filter(None);
        app.selected = 1;
        app.key(KeyEvent::from(KeyCode::Char('a')), 3);
        let mut frame = Frame::new(MIN_WIDTH, MIN_HEIGHT);
        app.draw(&mut frame);
        assert!(frame.rendered(3).contains(&(" User ", Tone::Tab)));
        assert!(frame
            .rendered(5)
            .contains(&("  VARIABLE NAME  ", Tone::Title)));
        assert_eq!(
            frame.rendered(7),
            [("  › ", Tone::Accent), ("Second", Tone::Accent)]
        );
        let (title, body) = frame.popup_contents().unwrap();
        assert_eq!(title, "New variable");
        assert_eq!(body.rendered(0)[0], (" › Name   ", Tone::Accent));

        for code in [KeyCode::Down, KeyCode::Tab, KeyCode::Right] {
            app.key(KeyEvent::from(code), 3);
        }
        let Mode::Edit(draft) = &app.mode else {
            panic!("expected editor still open")
        };
        assert!(draft.field == Field::Kind);
        assert_eq!(draft.kind, Kind::Expandable);
        app.key(KeyEvent::from(KeyCode::Esc), 3);
        let mut frame = Frame::new(MIN_WIDTH, MIN_HEIGHT);
        app.draw(&mut frame);
        assert!(frame.popup_contents().is_none());
        assert_eq!(app.name().unwrap().text, "Second");
        assert_eq!(app.query.value, "s");
    }

    #[test]
    fn path_item_edits_inside_the_same_popup_at_minimum_size() {
        let mut app = App::new();
        let mut draft = Draft::new();
        draft.name = Text::new("Path".into());
        draft.value = Text::new("one;two".into());
        app.mode = app.open_path(draft);
        app.key(KeyEvent::from(KeyCode::Right), 3);
        let mut frame = Frame::new(MIN_WIDTH, MIN_HEIGHT);
        app.draw(&mut frame);
        let (title, body) = frame.popup_contents().unwrap();
        assert_eq!(title, "Edit Path");
        assert!(body.rendered(1).contains(&("1. one", Tone::Accent)));
        assert!(body.rendered(3).contains(&(" › Edit item  ", Tone::Accent)));
    }

    #[test]
    fn refresh_keys_reload_only_the_requested_scopes_and_keep_the_filter() {
        let mut app = App::new();
        app.key(KeyEvent::from(KeyCode::Char('/')), 8);
        app.paste("unlikely-ee-test-name");
        app.key(KeyEvent::from(KeyCode::Enter), 8);
        assert!(app.key(KeyEvent::from(KeyCode::Char('r')), 8));
        assert!(app.names[0].is_some());
        assert!(app.names[1].is_none());
        assert!(app.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL), 8));
        assert!(app.names[1].is_some());
        assert_eq!(
            app.status,
            (Tone::Good, "Reloaded User and System lists.".into())
        );
        assert_eq!(app.query.value, "unlikely-ee-test-name");
        assert!(app.key(KeyEvent::from(KeyCode::Tab), 8));
        assert!(app.scope == Scope::System);
        assert_eq!(app.query.value, "unlikely-ee-test-name");
    }
}
