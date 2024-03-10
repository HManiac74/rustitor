use std::io::{self, Read, Write};
use std::ops::{Deref, DerefMut};
use std::os::unix::io::AsRawFd;
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
        termios.c_cc[VTIME] = 10;

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

enum SpecialKey {
    Left,
    Right,
    Up,
    Down,
}

#[derive(PartialEq, Debug)]
enum InputSeq {
    Unidentified,
    Key(u8, bool),
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
                let b = self.read_blocking()?;
                if b != b'[' {
                    return self.decode(b);
                }

                let mut buf = vec![];
                let cmd = loop {
                    let b = self.read_blocking()?;
                    match b {
                        b'R' => break b,
                        _ => buf.push(b),
                    }
                };

                let args = buf.split(|b| *b == b';');
                match cmd {
                    b'R' => {
                        let mut i = args
                            .map(|b| str::from_utf8(b).ok().and_then(|s| s.parse::<usize>().ok()));
                        match (i.next(), i.next()) {
                            (Some(Some(r)), Some(Some(c))) => Ok(InputSeq::Cursor(r, c)),
                            _ => Ok(InputSeq::Unidentified),
                        }
                    }
                    _ => Ok(InputSeq::Unidentified),
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

struct Editor {
    screen_rows: usize,
    screen_cols: usize,
}

impl Editor {
    fn new(size: Option<(usize, usize)>) -> Editor {
        let (screen_cols, screen_rows) = size.unwrap_or((0, 0));
        Editor {
            screen_cols,
            screen_rows,
        }
    }

    fn write_rows<W: Write>(&self, mut buf: W) -> io::Result<()> {
        for y in 0..self.screen_rows {
            if y == self.screen_rows / 3 {
                let msg_buf = format!("Rustitor editor -- version {}", VERSION);
                let mut welcome = msg_buf.as_str();
                if welcome.len() > self.screen_cols {
                    welcome = &welcome[..self.screen_cols];
                }
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

            buf.write(b"\x1b[K")?;

            if y < self.screen_rows - 1 {
                buf.write(b"\r\n")?;
            }
        }
        Ok(())
    }

    fn refresh_screen(&self) -> io::Result<()> {
        let mut buf = Vec::with_capacity((self.screen_rows + 1) * self.screen_cols);

        buf.write(b"\x1b[?25l")?;
        buf.write(b"\x1b[H")?;
        self.write_rows(&mut buf)?;
        buf.write(b"\x1b[H")?;
        buf.write(b"\x1b[?25h")?;

        let mut stdout = io::stdout();
        stdout.write(&buf)?;
        stdout.flush()
    }

    fn process_sequence(&mut self, seq: InputSeq) -> io::Result<bool> {
        match seq {
            InputSeq::Key(b'q', true) => Ok(true),
            _ => Ok(false),
        }
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
            self.refresh_screen()?;
            if self.process_sequence(seq?)? {
                break;
            }
        }
        self.refresh_screen()
    }
}

fn main() -> io::Result<()> {
    Editor::new(term_size::dimensions_stdout()).run(StdinRawMode::new()?.input_keys())
}
