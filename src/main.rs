use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::ops::{Deref, DerefMut};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::str;

const VERSION: &'static str = env!("CARGO_PKG_VERSION");

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
        InputSequences { stdin: self }
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
                    b => return self.decode(b),
                };
                
                let mut buf = vec![];
                let cmd = loop {
                    let b = self.read_blocking()?;
                    match b {
                        b'R' | b'A' | b'B' | b'C' | b'D' | b'~' | b'F' | b'H' => break b,
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
        let b = self.read()?;
        self.decode(b)
    }
}

impl Iterator for InputSequences {
    type Item = io::Result<InputSeq>;
    
    fn next(&mut self) -> Option<Self::Item> {
        Some(self.read_seq())
    }
}

struct Row {
    text: String,
}

enum CursorDir {
    Left,
    Right,
    Up,
    Down,
}

struct Editor {
    
    cx: usize,
    cy: usize,
    screen_rows: usize,
    screen_cols: usize,
    row: Vec<Row>,
    rowoff: usize,
    coloff: usize,
}

impl Editor {
    fn new(size: Option<(usize, usize)>) -> Editor {
        let (screen_cols, screen_rows) = size.unwrap_or((0, 0));
        Editor {
            cx: 0,
            cy: 0,
            screen_cols,
            screen_rows,
            row: Vec::with_capacity(screen_rows),
            rowoff: 0,
            coloff: 0,
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

    fn write_rows<W: Write>(&self, mut buf: W) -> io::Result<()> {
        for y in 0..self.screen_rows {
            let file_row = y + self.rowoff;
            if file_row >= self.row.len() {
                if self.row.is_empty() && y == self.screen_rows / 3 {
                    let msg_buf = format!("Kilo editor -- version {}", VERSION);
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
                let line = self.trim_line(&self.row[file_row].text);
                buf.write(line.as_bytes())?;
            }
            
            buf.write(b"\x1b[K")?;
            
            if y < self.screen_rows - 1 {
                buf.write(b"\r\n")?;
            }
        }
        Ok(())
    }
    
    fn redraw_screen(&self) -> io::Result<()> {
        let mut buf = Vec::with_capacity((self.screen_rows + 1) * self.screen_cols);
        
        buf.write(b"\x1b[?25l")?;
        buf.write(b"\x1b[H")?;
        self.write_rows(&mut buf)?;

        let cursor_row = self.cy - self.rowoff + 1;
        let cursor_col = self.cx - self.coloff + 1;
        
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

    fn open_file<P: AsRef<Path>>(&mut self, file: P) -> io::Result<()> {
        let file = fs::File::open(file)?;
        for line in io::BufReader::new(file).lines() {
            self.row.push(Row { text: line? });
        }
        Ok(())
    }

    fn scroll(&mut self) {
        if self.cy < self.rowoff {
            self.rowoff = self.cy;
        }
        if self.cy >= self.rowoff + self.screen_rows {
            self.rowoff = self.cy - self.screen_rows + 1;
        }
        if self.cx < self.coloff {
            self.coloff = self.cx;
        }
        if self.cx >= self.coloff + self.screen_cols {
            self.coloff = self.cx - self.screen_cols + 1;
        }
    }

    fn move_cursor(&mut self, dir: CursorDir) {
        match dir {
            CursorDir::Up => self.cy = self.cy.saturating_sub(1),
            CursorDir::Left => {
                if self.cx > 0 {
                    self.cx -= 1;
                } else if self.cy > 0 {
                    self.cy -= 1;
                    self.cx = self.row[self.cy].text.len();
                }
            }
            CursorDir::Down => {
                if self.cy < self.row.len() {
                    self.cy += 1;
                }
            }
            CursorDir::Right => {
                if self.cy < self.row.len() {
                    let len = self.row[self.cy].text.len();
                    if self.cx < len {
                        self.cx += 1;
                    } else if self.cx >= len {
                        self.cy += 1;
                        self.cx = 0;
                    }
                }
            }
        };
        let len = self.row.get(self.cy).map(|r| r.text.len()).unwrap_or(0);
        if self.cx > len {
            self.cx = len;
        }
    }

    fn process_sequence(&mut self, seq: InputSeq) -> io::Result<bool> {
        let mut exit = false;
        match seq {
            InputSeq::Key(b'w', false) | InputSeq::UpKey => self.move_cursor(CursorDir::Up),
            InputSeq::Key(b'a', false) | InputSeq::LeftKey => self.move_cursor(CursorDir::Left),
            InputSeq::Key(b's', false) | InputSeq::DownKey => self.move_cursor(CursorDir::Down),
            InputSeq::Key(b'd', false) | InputSeq::RightKey => self.move_cursor(CursorDir::Right),
            InputSeq::PageUpKey => {
                for _ in 0..self.screen_rows {
                    self.move_cursor(CursorDir::Up);
                }
            }
            InputSeq::PageDownKey => {
                for _ in 0..self.screen_rows {
                    self.move_cursor(CursorDir::Down)
                }
            }
            InputSeq::HomeKey => self.cx = 0,
            InputSeq::EndKey => self.cx = self.screen_cols - 1,
            InputSeq::DeleteKey => unimplemented!("delete key press"),
            InputSeq::Key(b'q', true) => exit = true,
            _ => {}
        }
        Ok(exit)
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
                self.screen_rows = r;
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
        
        for seq in input {
            self.scroll();
            self.redraw_screen()?;
            if self.process_sequence(seq?)? {
                break;
            }
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
