use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{KeyCode, KeyEvent, KeyModifiers},
    queue,
    style::{Attribute, Color, Print, SetAttribute, SetForegroundColor},
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::{self, BufWriter, IsTerminal, Write};
use unicode_width::UnicodeWidthChar;
use winapi::um::{
    consoleapi::{GetConsoleMode, SetConsoleMode},
    processenv::GetStdHandle,
    winbase::{GlobalLock, GlobalSize, GlobalUnlock, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    wincon::{
        ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
        ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
    },
    winnt::HANDLE,
    winuser::{CloseClipboard, GetClipboardData, OpenClipboard, CF_UNICODETEXT},
};

pub struct Terminal {
    output: BufWriter<io::Stdout>,
    console_in: ConsoleMode,
    console_out: ConsoleMode,
    alternate: bool,
}

impl Terminal {
    pub fn open() -> io::Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(io::Error::other(
                "Open ee in an interactive Windows terminal.",
            ));
        }
        let mut terminal = Self {
            output: BufWriter::new(io::stdout()),
            console_in: ConsoleMode::get(STD_INPUT_HANDLE)?,
            console_out: ConsoleMode::get(STD_OUTPUT_HANDLE)?,
            alternate: false,
        };
        terminal.console_in.set(
            terminal.console_in.original
                & !(ENABLE_LINE_INPUT
                    | ENABLE_ECHO_INPUT
                    | ENABLE_PROCESSED_INPUT
                    | ENABLE_VIRTUAL_TERMINAL_INPUT
                    | ENABLE_QUICK_EDIT_MODE)
                | ENABLE_WINDOW_INPUT
                | ENABLE_EXTENDED_FLAGS,
        )?;
        terminal.console_out.set(
            terminal.console_out.original
                | ENABLE_PROCESSED_OUTPUT
                | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        )?;
        terminal.alternate = true;
        queue!(
            terminal.output,
            EnterAlternateScreen,
            Clear(ClearType::All),
            Hide
        )?;
        terminal.output.flush()?;
        Ok(terminal)
    }

    pub fn draw(&mut self, frame: Frame) -> io::Result<()> {
        queue!(self.output, Hide)?;
        let mut cursor = frame.cursor;
        for (row, line) in frame.lines.into_iter().enumerate() {
            queue!(
                self.output,
                MoveTo(0, row as u16),
                Clear(ClearType::UntilNewLine)
            )?;
            paint_line(&mut self.output, frame.left, row as u16, &line)?;
        }
        if let Some(popup) = frame.popup {
            let row = popup.top;
            let col = popup.body.left - 2;
            let rule = "─".repeat(popup.body.width + 2);
            let blanks = " ".repeat(popup.body.width + 2);
            queue!(
                self.output,
                MoveTo(col, row),
                Print("╭"),
                Print(&rule),
                Print("╮")
            )?;
            paint_line(
                &mut self.output,
                col + 2,
                row,
                &[Segment {
                    text: format!(" {} ", popup.title),
                    tone: Tone::Accent,
                }],
            )?;
            cursor = popup
                .body
                .cursor
                .map(|(column, local_row)| (column, row + 1 + local_row));
            for (index, line) in popup.body.lines.into_iter().enumerate() {
                let body_row = row + 1 + index as u16;
                queue!(
                    self.output,
                    MoveTo(col, body_row),
                    Print("│"),
                    Print(&blanks),
                    Print("│")
                )?;
                paint_line(&mut self.output, col + 2, body_row, &line)?;
            }
            queue!(
                self.output,
                MoveTo(col, row + 1 + popup.body.height as u16),
                Print("╰"),
                Print(&rule),
                Print("╯")
            )?;
        }
        if let Some((column, row)) = cursor {
            queue!(self.output, MoveTo(column, row), Show)?;
        }
        self.output.flush()
    }
}

fn paint_line(output: &mut impl Write, column: u16, row: u16, line: &[Segment]) -> io::Result<()> {
    if line.is_empty() {
        return Ok(());
    }
    queue!(output, MoveTo(column, row))?;
    for segment in line {
        match segment.tone {
            Tone::Plain => {}
            Tone::Title => queue!(output, SetAttribute(Attribute::Bold))?,
            Tone::Tab => queue!(
                output,
                SetAttribute(Attribute::Reverse),
                SetAttribute(Attribute::Bold)
            )?,
            Tone::Rule => queue!(output, SetForegroundColor(Color::Grey))?,
            tone => queue!(
                output,
                SetForegroundColor(tone.color()),
                SetAttribute(Attribute::Bold)
            )?,
        }
        queue!(output, Print(&segment.text))?;
        if segment.tone != Tone::Plain {
            queue!(output, SetAttribute(Attribute::Reset))?;
        }
    }
    Ok(())
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.alternate {
            let _ = queue!(
                self.output,
                SetAttribute(Attribute::Reset),
                LeaveAlternateScreen,
                Show
            );
            let _ = self.output.flush();
        }
    }
}

struct ConsoleMode {
    handle: HANDLE,
    original: u32,
}

impl ConsoleMode {
    fn get(which: u32) -> io::Result<Self> {
        let handle = unsafe { GetStdHandle(which) };
        let mut original = 0;
        if unsafe { GetConsoleMode(handle, &mut original) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle, original })
    }

    fn set(&self, mode: u32) -> io::Result<()> {
        if unsafe { SetConsoleMode(self.handle, mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for ConsoleMode {
    fn drop(&mut self) {
        let _ = self.set(self.original);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Plain,
    Title,
    Tab,
    Rule,
    Accent,
    Good,
    Warning,
    Error,
}

impl Tone {
    fn color(self) -> Color {
        match self {
            Self::Accent => Color::Cyan,
            Self::Good => Color::Green,
            Self::Warning => Color::Yellow,
            Self::Error => Color::Red,
            _ => Color::Reset,
        }
    }
}

struct Segment {
    text: String,
    tone: Tone,
}

pub struct Frame {
    pub width: usize,
    pub height: usize,
    left: u16,
    lines: Vec<Vec<Segment>>,
    cursor: Option<(u16, u16)>,
    popup: Option<Box<Popup>>,
}

struct Popup {
    title: &'static str,
    top: u16,
    body: Frame,
}

impl Frame {
    pub fn new(width: u16, height: u16) -> Self {
        let content = width
            .saturating_sub(if width >= 48 { 4 } else { 1 })
            .min(88);
        Self::area(content as usize, height as usize, (width - content) / 2)
    }

    fn area(width: usize, height: usize, left: u16) -> Self {
        Self {
            width,
            height,
            left,
            lines: (0..height).map(|_| Vec::new()).collect(),
            cursor: None,
            popup: None,
        }
    }

    pub fn popup(&mut self, title: &'static str, height: usize) -> &mut Self {
        let width = (self.width - 6).min(66);
        let left = self.left + ((self.width - width) / 2) as u16;
        let popup = Popup {
            title,
            top: (self.height - height).div_ceil(2) as u16,
            body: Self::area(width - 4, height - 2, left + 2),
        };
        &mut self.popup.insert(Box::new(popup)).body
    }

    pub fn line(&mut self, row: usize, text: &str, tone: Tone) {
        self.spans(row, &[(text, tone)]);
    }

    pub fn spans(&mut self, row: usize, spans: &[(&str, Tone)]) {
        if let Some(line) = self.lines.get_mut(row) {
            line.clear();
            let mut width = self.width;
            for &(text, tone) in spans {
                let (text, complete) = clip(text, &mut width);
                if !text.is_empty() {
                    line.push(Segment { text, tone });
                }
                if !complete {
                    break;
                }
            }
        }
    }

    pub fn field(&mut self, row: usize, label: &str, input: &Text, tone: Tone) {
        let label_width = label.chars().map(display_width).sum::<usize>();
        if row >= self.height || label_width >= self.width {
            return;
        }
        let available = self.width - label_width;
        let mut remaining = available - 1;
        let mut start = input.cursor;
        for (index, character) in input.value[..input.cursor].char_indices().rev() {
            let cells = display_width(character);
            if cells > remaining {
                break;
            }
            remaining -= cells;
            start = index;
        }
        self.spans(row, &[(label, tone), (&input.value[start..], Tone::Plain)]);
        self.cursor = Some((self.left + (self.width - 1 - remaining) as u16, row as u16));
    }

    #[cfg(test)]
    pub fn rendered(&self, row: usize) -> Vec<(&str, Tone)> {
        self.lines[row]
            .iter()
            .map(|span| (span.text.as_str(), span.tone))
            .collect()
    }

    #[cfg(test)]
    pub fn popup_contents(&self) -> Option<(&str, &Self)> {
        let popup = self.popup.as_ref()?;
        Some((popup.title, &popup.body))
    }
}

fn escaped(character: char) -> Option<&'static str> {
    match character {
        '\r' => Some("\\r"),
        '\n' => Some("\\n"),
        '\t' => Some("\\t"),
        '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => Some("?"),
        character if character.is_control() => Some("?"),
        _ => None,
    }
}

fn display_width(character: char) -> usize {
    escaped(character).map_or_else(|| character.width().unwrap_or(0), str::len)
}

fn push_visible(output: &mut String, character: char) {
    if let Some(escaped) = escaped(character) {
        output.push_str(escaped);
    } else {
        output.push(character);
    }
}

fn clip(text: &str, width: &mut usize) -> (String, bool) {
    let mut output = String::new();
    for character in text.chars() {
        let cells = display_width(character);
        if cells > *width {
            return (output, false);
        }
        *width -= cells;
        push_visible(&mut output, character);
    }
    (output, true)
}

pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cells = display_width(character);
        if used + cells > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        push_visible(&mut line, character);
        used += cells;
    }
    lines.push(line);
    lines
}

#[derive(Default)]
pub struct Text {
    pub value: String,
    cursor: usize,
}

impl Text {
    pub fn new(value: String) -> Self {
        Self {
            cursor: value.len(),
            value,
        }
    }

    pub fn paste(&mut self, text: &str, single_line: bool) -> Result<bool, &'static str> {
        if text.contains('\0') || (single_line && text.chars().any(char::is_control)) {
            return Err("This field does not accept the pasted control characters.");
        }
        if self.value.len() + text.len() > 128 * 1024 {
            return Err("Text is too large.");
        }
        self.value.insert_str(self.cursor, text);
        self.cursor += text.len();
        Ok(!text.is_empty())
    }

    pub fn key(&mut self, key: KeyEvent, single_line: bool) -> Result<bool, &'static str> {
        if ctrl(key, 'v') {
            return self.paste(&clipboard_text()?, single_line);
        }
        if ctrl(key, 'u') {
            let changed = !self.value.is_empty();
            *self = Self::default();
            return Ok(changed);
        }
        let previous = self.value[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(self.cursor, |(position, _)| position);
        let next = self.cursor
            + self.value[self.cursor..]
                .chars()
                .next()
                .map_or(0, char::len_utf8);
        match key.code {
            KeyCode::Left => self.cursor = previous,
            KeyCode::Right => self.cursor = next,
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.len(),
            KeyCode::Backspace => {
                self.value.drain(previous..self.cursor);
                let changed = previous != self.cursor;
                self.cursor = previous;
                return Ok(changed);
            }
            KeyCode::Delete => {
                self.value.drain(self.cursor..next);
                return Ok(self.cursor != next);
            }
            KeyCode::Char(character)
                if !character.is_control()
                    && (!key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.modifiers.contains(KeyModifiers::ALT)) =>
            {
                return self.paste(character.encode_utf8(&mut [0; 4]), single_line);
            }
            _ => {}
        }
        Ok(false)
    }
}

pub fn ctrl(key: KeyEvent, character: char) -> bool {
    matches!(key.code, KeyCode::Char(actual) if actual.eq_ignore_ascii_case(&character))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn clipboard_text() -> Result<String, &'static str> {
    if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        return Err("Could not open the clipboard. Try again.");
    }
    struct Clipboard;
    impl Drop for Clipboard {
        fn drop(&mut self) {
            unsafe { CloseClipboard() };
        }
    }
    let _clipboard = Clipboard;
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
    if handle.is_null() {
        return Err("Clipboard has no text.");
    }
    let size = unsafe { GlobalSize(handle) };
    if !(2..=256 * 1024).contains(&size) {
        return Err("Clipboard text is too large or unavailable.");
    }
    let data = unsafe { GlobalLock(handle) };
    if data.is_null() {
        return Err("Could not read the clipboard.");
    }
    let units = unsafe { std::slice::from_raw_parts(data.cast::<u16>(), size / 2) };
    let text = units
        .iter()
        .position(|&unit| unit == 0)
        .and_then(|end| String::from_utf16(&units[..end]).ok());
    unsafe { GlobalUnlock(handle) };
    text.ok_or("Clipboard contains invalid UTF-16 text.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safely_clips_and_places_the_cursor_with_wide_text() {
        assert_eq!(clip("x\x1b[33m\n中", &mut 9), ("x?[33m\\n".into(), false));
        assert_eq!(wrap("中文\nab", 4), ["中文", "\\nab"]);
        let mut frame = Frame::new(12, 8);
        frame.field(2, "v: ", &Text::new("1234567测试".into()), Tone::Accent);
        assert_eq!(
            frame.rendered(2),
            [("v: ", Tone::Accent), ("567测试", Tone::Plain)]
        );
        assert_eq!(frame.cursor, Some((10, 2)));
    }

    #[test]
    fn text_edits_on_utf8_boundaries_and_treats_paste_as_data() {
        let mut input = Text::new("a😀b".into());
        input
            .key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), false)
            .unwrap();
        input
            .key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), false)
            .unwrap();
        assert_eq!(input.value, "ab");
        assert!(input.paste("e\r\n键", true).is_err());
        input.paste("e\r\n键", false).unwrap();
        assert_eq!(input.value, "ae\r\n键b");
    }
}
