use std::cmp;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::ops::{Deref, DerefMut};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::str;
use std::time::SystemTime;

const VERSION: &'static str = env!("CARGO_PKG_VERSION");
const TAB_STOP: usize = 8;

struct StdinRawMode {
    stdin: io::Stdin,
    orig: termios::Termios,
}

impl StdinRawMode {
    fn new() -> io::Result<StdinRawMode> {
        use termios::*;
        
        let stdin = io::stdin();
        let fd = stdin.as_raw_fd();
        let mut termios = Termios::from_fd(fd)?;
        let orig = termios.clone();
        
        termios.c_lflag &= !(ECHO | ICANON | ISIG | IEXTEN);
        termios.c_iflag &= !(IXON | ICRNL | BRKINT | INPCK | ISTRIP);
        termios.c_oflag &= !OPOST;
        termios.c_cflag |= CS8;
        termios.c_cc[VMIN] = 0;
        termios.c_cc[VTIME] = 1;
        
        tcsetattr(fd, TCSAFLUSH, &mut termios)?;

        Ok(StdinRawMode { stdin, orig })
    }

    fn input_keys(self) -> InputSequences {
        InputSequences {
            stdin: self,
            next_byte: 0,
        }
    }
}

impl Drop for StdinRawMode {
    fn drop(&mut self) {
        termios::tcsetattr(self.stdin.as_raw_fd(), termios::TCSAFLUSH, &mut self.orig).unwrap();
    }
}

impl Deref for StdinRawMode {
    type Target = io::Stdin;

    fn deref(&self) -> &Self::Target {
        &self.stdin
    }
}

impl DerefMut for StdinRawMode {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.stdin
    }
}

#[derive(PartialEq, Debug)]
enum InputSeq {
    Unidentified,
    Key(u8, bool),
    LeftKey,
    RightKey,
    UpKey,
    DownKey,
    PageUpKey,
    PageDownKey,
    HomeKey,
    EndKey,
    DeleteKey,
    Cursor(usize, usize),
}

struct InputSequences {
    stdin: StdinRawMode,
    next_byte: u8,
}

impl InputSequences {
    fn read(&mut self) -> io::Result<u8> {
        let mut one_byte: [u8; 1] = [0];
        self.stdin.read(&mut one_byte)?;
        Ok(one_byte[0])
    }

    fn read_blocking(&mut self) -> io::Result<u8> {
        let mut one_byte: [u8; 1] = [0];
        loop {
            if self.stdin.read(&mut one_byte)? > 0 {
                return Ok(one_byte[0]);
            }
        }
    }

    fn decode(&mut self, b: u8) -> io::Result<InputSeq> {
        match b {
            0x1b => {
                
                match self.read()? {
                    b'[' => {  }
                    0 => return Ok(InputSeq::Key(0x1b, false)),
                    b => {
                        self.next_byte = b;
                        return Ok(InputSeq::Key(0x1b, false));
                    }
                };
                
                let mut buf = vec![];
                let cmd = loop {
                    let b = self.read_blocking()?;
                    match b {
                        b'A' | b'B' | b'C' | b'D' | b'F' | b'H' | b'K' | b'J' | b'R' | b'c'
                        | b'f' | b'g' | b'h' | b'l' | b'm' | b'n' | b'q' | b'y' | b'~' => break b,
                        b'O' => {
                            buf.push(b'O');
                            let b = self.read_blocking()?;
                            match b {
                                b'F' | b'H' => break b,
                                _ => buf.push(b),
                            };
                        }
                        _ => buf.push(b),
                    }
                };

                let mut args = buf.split(|b| *b == b';');
                match cmd {
                    b'R' => {
                        let mut i = args
                            .map(|b| str::from_utf8(b).ok().and_then(|s| s.parse::<usize>().ok()));
                        match (i.next(), i.next()) {
                            (Some(Some(r)), Some(Some(c))) => Ok(InputSeq::Cursor(r, c)),
                            _ => Ok(InputSeq::Unidentified),
                        }
                    }
                    b'A' => Ok(InputSeq::UpKey),
                    b'B' => Ok(InputSeq::DownKey),
                    b'C' => Ok(InputSeq::RightKey),
                    b'D' => Ok(InputSeq::LeftKey),
                    b'~' => {
                        
                        match args.next() {
                            Some(b"5") => Ok(InputSeq::PageUpKey),
                            Some(b"6") => Ok(InputSeq::PageDownKey),
                            Some(b"1") | Some(b"7") => Ok(InputSeq::HomeKey),
                            Some(b"4") | Some(b"8") => Ok(InputSeq::EndKey),
                            Some(b"3") => Ok(InputSeq::DeleteKey),
                            _ => Ok(InputSeq::Unidentified),
                        }
                    }
                    b'H' => Ok(InputSeq::HomeKey),
                    b'F' => Ok(InputSeq::EndKey),
                    _ => unreachable!(),
                }
            }
            0x20..=0x7f => Ok(InputSeq::Key(b, false)),
            0x01..=0x1f => Ok(InputSeq::Key(b | 0b1100000, true)),
            _ => Ok(InputSeq::Unidentified),
        }
    }

    fn read_seq(&mut self) -> io::Result<InputSeq> {
        let b = match self.next_byte {
            0 => self.read()?,
            b => {
                self.next_byte = 0;
                b
            }
        };
        self.decode(b)
    }
}

impl Iterator for InputSequences {
    type Item = io::Result<InputSeq>;
    
    fn next(&mut self) -> Option<Self::Item> {
        Some(self.read_seq())
    }
}

struct FilePath {
    path: PathBuf,
    display: String,
}

impl FilePath {
    fn from<P: AsRef<Path>>(path: P) -> FilePath {
        let path = path.as_ref();
        FilePath {
            path: PathBuf::from(path),
            display: path.to_string_lossy().to_string(),
        }
    }
}

struct StatusMessage {
    text: String,
    timestamp: SystemTime,
}

impl StatusMessage {
    fn new<S: Into<String>>(message: S) -> StatusMessage {
        StatusMessage {
            text: message.into(),
            timestamp: SystemTime::now(),
        }
    }
}

struct Row {
    buf: String,
    render: String,
}

impl Row {
    fn new<S: Into<String>>(line: S) -> Row {
        let mut row = Row {
            buf: line.into(),
            render: "".to_string(),
        };
        row.update_render();
        row
    }

    fn empty() -> Row {
        Row {
            buf: "".to_string(),
            render: "".to_string(),
        }
    }

    fn update_render(&mut self) {
        self.render = String::with_capacity(self.buf.len());
        let mut index = 0;
        for c in self.buf.chars() {
            if c == '\t' {
                loop {
                    self.render.push(' ');
                    index += 1;
                    if index % TAB_STOP == 0 {
                        break;
                    }
                }
            } else {
                self.render.push(c);
                index += 1;
            }
        }
    }

    fn rx_from_cx(&self, cx: usize) -> usize {
        self.buf.chars().take(cx).fold(0, |rx, ch| {
            if ch == '\t' {
                rx + TAB_STOP - (rx % TAB_STOP)
            } else {
                rx + 1
            }
        })
    }

    fn insert_char(&mut self, at: usize, c: char) {
        if self.buf.len() <= at {
            self.buf.push(c);
        } else {
            self.buf.insert(at, c);
        }
        self.update_render();
    }

    fn delete_char(&mut self, at: usize) {
        if at < self.buf.len() {
            self.buf.remove(at);
            self.update_render();
        }
    }

    fn append<S: AsRef<str>>(&mut self, s: S) {
        let s = s.as_ref();
        if s.is_empty() {
            return;
        }
        self.buf.push_str(s);
        self.update_render();
    }

    fn truncate(&mut self, at: usize) {
        if at < self.buf.len() {
            self.buf.truncate(at);
            self.update_render();
        }
    }
}

enum CursorDir {
    Left,
    Right,
    Up,
    Down,
}

struct Editor {
    
    file: Option<FilePath>,

    cx: usize,
    cy: usize,

    rx: usize,

    screen_rows: usize,
    screen_cols: usize,
    row: Vec<Row>,
    rowoff: usize,
    coloff: usize,

    message: StatusMessage,
    dirty: bool,
    quitting: bool,
}

impl Editor {
    fn new(window_size: Option<(usize, usize)>) -> Editor {
        let (w, h) = window_size.unwrap_or((0, 0));
        Editor {
            file: None,
            cx: 0,
            cy: 0,
            rx: 0,
            screen_cols: w,
            screen_rows: h.saturating_sub(2),
            row: Vec::with_capacity(h),
            rowoff: 0,
            coloff: 0,
            message: StatusMessage::new("HELP: Ctrl-S = save | Ctrl-Q = quit"),
            dirty: false,
            quitting: false,
        }
    }

    fn trim_line<'a, S: AsRef<str>>(&self, line: &'a S) -> &'a str {
        let mut line = line.as_ref();
        if line.len() <= self.coloff {
            return "";
        }
        if self.coloff > 0 {
            line = &line[self.coloff..];
        }
        if line.len() > self.screen_cols {
            line = &line[..self.screen_cols]
        }
        line
    }

    fn draw_status_bar<W: Write>(&self, mut buf: W) -> io::Result<()> {
        buf.write(b"\x1b[7m")?;

        let file = if let Some(ref f) = self.file {
            f.display.as_str()
        } else {
            "[No Name]"
        };

        let modified = if self.dirty { "(modified) " } else { "" };
        let left = format!("{:<20?} - {} lines {}", file, self.row.len(), modified);
        let left = &left[..cmp::min(left.len(), self.screen_cols)];
        buf.write(left.as_bytes())?;

        let rest_len = self.screen_cols - left.len();
        if rest_len == 0 {
            return Ok(());
        }

        let right = format!("{}/{}", self.cy, self.row.len());
        if right.len() > rest_len {
            for _ in 0..rest_len {
                buf.write(b" ")?;
            }
            return Ok(());
        }

        for _ in 0..rest_len - right.len() {
            buf.write(b" ")?;
        }
        buf.write(right.as_bytes())?;

        buf.write(b"\x1b[m")?;
        buf.write(b"\r\n")?;
        Ok(())
    }

    fn draw_message_bar<W: Write>(&self, mut buf: W) -> io::Result<()> {
        if let Ok(d) = SystemTime::now().duration_since(self.message.timestamp) {
            if d.as_secs() < 5 {
                let msg = &self.message.text[..cmp::min(self.message.text.len(), self.screen_cols)];
                buf.write(msg.as_bytes())?;
            }
        }
        buf.write(b"\x1b[K")?;
        Ok(())
    }

    fn draw_rows<W: Write>(&self, mut buf: W) -> io::Result<()> {
        for y in 0..self.screen_rows {
            let file_row = y + self.rowoff;
            if file_row >= self.row.len() {
                if self.row.is_empty() && y == self.screen_rows / 3 {
                    let msg_buf = format!("Rustitor editor -- version {}", VERSION);
                    let welcome = self.trim_line(&msg_buf);
                    let padding = (self.screen_cols - welcome.len()) / 2;
                    if padding > 0 {
                        buf.write(b"~")?;
                        for _ in 0..padding - 1 {
                            buf.write(b" ")?;
                        }
                    }
                    buf.write(welcome.as_bytes())?;
                } else {
                    buf.write(b"~")?;
                }
            } else {
                let line = self.trim_line(&self.row[file_row].render);
                buf.write(line.as_bytes())?;
            }
            
            buf.write(b"\x1b[K")?;
            buf.write(b"\r\n")?;
        }
        Ok(())
    }

    fn refresh_screen(&self) -> io::Result<()> {
        let mut buf = Vec::with_capacity((self.screen_rows + 1) * self.screen_cols);
        
        buf.write(b"\x1b[?25l")?;
        buf.write(b"\x1b[H")?;

        self.draw_rows(&mut buf)?;
        self.draw_status_bar(&mut buf)?;
        self.draw_message_bar(&mut buf)?;

        let cursor_row = self.cy - self.rowoff + 1;
        let cursor_col = self.rx - self.coloff + 1;
        write!(buf, "\x1b[{};{}H", cursor_row, cursor_col)?;
        
        buf.write(b"\x1b[?25h")?;

        let mut stdout = io::stdout();
        stdout.write(&buf)?;
        stdout.flush()
    }

    fn clear_screen(&self) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.write(b"\x1b[2J")?;
        stdout.write(b"\x1b[H")?;
        stdout.flush()
    }

    fn open_file<P: AsRef<Path>>(&mut self, path: P) -> io::Result<()> {
        let path = path.as_ref();
        let file = fs::File::open(path)?;
        for line in io::BufReader::new(file).lines() {
            self.row.push(Row::new(line?));
        }
        self.file = Some(FilePath::from(path));
        self.dirty = false;
        Ok(())
    }

    fn save(&mut self) -> io::Result<()> {
        let ref file = if let Some(ref file) = self.file {
            file
        } else {
            self.message = StatusMessage::new("Cannot save unnamed buffer");
            return Ok(());
        };

        let mut f = io::BufWriter::new(fs::File::create(&file.path)?);
        let mut bytes = 0;
        for line in self.row.iter() {
            let b = line.buf.as_bytes();
            f.write(b)?;
            f.write(b"\n")?;
            bytes += b.len() + 1;
        }
        f.flush()?;

        let msg = format!("{} bytes written to {}", bytes, &file.display);
        self.message = StatusMessage::new(msg);
        self.dirty = false;
        Ok(())
    }

    fn setup_scroll(&mut self) {

        if self.cy < self.row.len() {
            self.rx = self.row[self.cy].rx_from_cx(self.cx);
        } else {
            self.rx = 0;
        }

        if self.cy < self.rowoff {

            self.rowoff = self.cy;
        }
        if self.cy >= self.rowoff + self.screen_rows {

            self.rowoff = self.cy - self.screen_rows + 1;
        }
        if self.rx < self.coloff {
            self.coloff = self.rx;
        }
        if self.rx >= self.coloff + self.screen_cols {
            self.coloff = self.rx - self.screen_cols + 1;
        }
    }

    fn insert_char(&mut self, ch: char) {
        if self.cy == self.row.len() {
            self.row.push(Row::empty());
        }
        self.row[self.cy].insert_char(self.cx, ch);
        self.cx += 1;
        self.dirty = true;
    }

    fn delete_char(&mut self) {
        if self.cy == self.row.len() || self.cx == 0 && self.cy == 0 {
            return;
        }
        if self.cx > 0 {
            self.row[self.cy].delete_char(self.cx - 1);
            self.cx -= 1;
        } else {
            self.cx = self.row[self.cy - 1].buf.len();
            let row = self.row.remove(self.cy);
            self.cy -= 1;
            self.row[self.cy].append(row.buf);
        }
        self.dirty = true;
    }

    fn insert_line(&mut self) {
        if self.cy >= self.row.len() {
            self.row.push(Row::new(""));
        } else if self.cx >= self.row[self.cy].buf.len() {
            self.row.insert(self.cy + 1, Row::new(""));
        } else {
            let split = String::from(&self.row[self.cy].buf[self.cx..]);
            self.row[self.cy].truncate(self.cx);
            self.row.insert(self.cy + 1, Row::new(split));
        }
        self.cy += 1;
        self.cx = 0;
    }

    fn move_cursor(&mut self, dir: CursorDir) {
        match dir {
            CursorDir::Up => self.cy = self.cy.saturating_sub(1),
            CursorDir::Left => {
                if self.cx > 0 {
                    self.cx -= 1;
                } else if self.cy > 0 {
                    self.cy -= 1;
                    self.cx = self.row[self.cy].buf.len();
                }
            }
            CursorDir::Down => {
                if self.cy < self.row.len() {
                    self.cy += 1;
                }
            }
            CursorDir::Right => {
                if self.cy < self.row.len() {
                    let len = self.row[self.cy].buf.len();
                    if self.cx < len {
                        self.cx += 1;
                    } else if self.cx >= len {
                        self.cy += 1;
                        self.cx = 0;
                    }
                }
            }
        };
        let len = self.row.get(self.cy).map(|r| r.buf.len()).unwrap_or(0);
        if self.cx > len {
            self.cx = len;
        }
    }

    fn process_keypress(&mut self, seq: InputSeq) -> io::Result<bool> {

        match seq {
            InputSeq::Key(b'p', true) | InputSeq::UpKey => self.move_cursor(CursorDir::Up),
            InputSeq::Key(b'b', true) | InputSeq::LeftKey => self.move_cursor(CursorDir::Left),
            InputSeq::Key(b'n', true) | InputSeq::DownKey => self.move_cursor(CursorDir::Down),
            InputSeq::Key(b'f', true) | InputSeq::RightKey => self.move_cursor(CursorDir::Right),
            InputSeq::PageUpKey => {
                self.cy = self.rowoff;
                for _ in 0..self.screen_rows {
                    self.move_cursor(CursorDir::Up);
                }
            }
            InputSeq::PageDownKey => {
                self.cy = cmp::min(self.rowoff + self.screen_rows - 1, self.row.len());
                for _ in 0..self.screen_rows {
                    self.move_cursor(CursorDir::Down)
                }
            }
            InputSeq::Key(b'a', true) | InputSeq::HomeKey => self.cx = 0,
            InputSeq::Key(b'e', true) | InputSeq::EndKey => {
                if self.cy < self.row.len() {
                    self.cx = self.screen_cols - 1;
                }
            }
            InputSeq::DeleteKey | InputSeq::Key(b'd', true) => {
                self.move_cursor(CursorDir::Right);
                self.delete_char();
            } 
            
            InputSeq::Key(b'q', true) => {
                if self.quitting {
                    return Ok(true);
                } else {
                    self.quitting = true;
                    self.message = StatusMessage::new(
                        "File has unsaved changes! Press Ctrl-Q again to quit");
                    return Ok(false);
                }
            }
            InputSeq::Key(b'\r', false) | InputSeq::Key(b'm', true) => self.insert_line(),
            InputSeq::Key(b'h', true) | InputSeq::Key(0x08, false) | InputSeq::Key(0x7f, false) => {
                self.delete_char();
            }
            InputSeq::Key(b'l', true) | InputSeq::Key(0x1b, false) => {
            }
            InputSeq::Key(b's', true) => self.save()?,
            InputSeq::Key(b, false) => self.insert_char(b as char),
            InputSeq::Key(..) => { }
            _ => unreachable!(),
        }
        self.quitting = false;
        Ok(false)
    }

    fn ensure_screen_size<I>(&mut self, mut input: I) -> io::Result<I>
    where
        I: Iterator<Item = io::Result<InputSeq>>,
    {
        if self.screen_cols > 0 && self.screen_rows > 0 {
            return Ok(input);
        }
        
        let mut stdout = io::stdout();
        stdout.write(b"\x1b[9999C\x1b[9999B\x1b[6n")?;
        stdout.flush()?;
        
        for seq in &mut input {
            if let InputSeq::Cursor(r, c) = seq? {
                self.screen_cols = c;
                self.screen_rows = r.saturating_sub(2);
                break;
            }
        }

        Ok(input)
    }

    fn run<I>(&mut self, input: I) -> io::Result<()>
    where
        I: Iterator<Item = io::Result<InputSeq>>,
    {
        let input = self.ensure_screen_size(input)?;

        self.setup_scroll();
        self.refresh_screen()?;

        for seq in input {
            let seq = seq?;
            if seq == InputSeq::Unidentified {
                continue;
            }
            if self.process_keypress(seq)? {
                break;
            }
            self.setup_scroll();
            self.refresh_screen()?;
        }

        self.clear_screen()
    }
}

fn main() -> io::Result<()> {
    let mut editor = Editor::new(term_size::dimensions_stdout());
    if let Some(arg) = std::env::args().skip(1).next() {
        editor.open_file(arg)?;
    }
    editor.run(StdinRawMode::new()?.input_keys())
}
