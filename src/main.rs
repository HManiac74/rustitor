use std::io::{self, Read};
use std::os::unix::io::{AsRawFd, RawFd};


struct InputRawMode {
    fd: RawFd,
    orig: termios::Termios,
}

impl InputRawMode {
    fn new(stdin: &io::Stdin) -> io::Result<InputRawMode> {
        use termios::*;

        let fd = stdin.as_raw_fd();
        let mut termios = Termios::from_fd(fd)?;
        let orig = termios.clone();

        termios.c_lflag &= !(ECHO | ICANON | ISIG | IEXTEN);
        termios.c_iflag &= !(IXON | ICRNL | BRKINT | INPCK | ISTRIP);
        termios.c_oflag &= !OPOST;
        termios.c_cflag |= CS8;
        tcsetattr(fd, TCSAFLUSH, &mut termios)?;

        Ok(InputRawMode { fd, orig })
    }
}

impl Drop for InputRawMode {
    fn drop(&mut self) {
        termios::tcsetattr(self.fd, termios::TCSAFLUSH, &mut self.orig).unwrap();
    }
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let _raw = InputRawMode::new(&stdin)?;

    for b in stdin.bytes() {
        let c = b? as char;

        if c.is_control() {
            print!("{}\r\n", c as i32);
        } else {
            print!("{} ({})\r\n", c, c as i32);
        }

        if c == 'q' {
            break;
        }
    }
    Ok(())
}