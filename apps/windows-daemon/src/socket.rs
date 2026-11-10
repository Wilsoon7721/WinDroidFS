use std::io::{self, Read, Write};
use std::net::TcpStream;

pub struct Guarded {
    sock: TcpStream,
    /// A send timed out, so what the phone has received is unknown.
    broken: bool,
}

impl Guarded {
    pub fn new(sock: TcpStream) -> Self {
        Guarded {
            sock,
            broken: false,
        }
    }

    /// The socket underneath for timeouts and shutting down. Never write to it directly.
    pub fn get_ref(&self) -> &TcpStream {
        &self.sock
    }
}

fn unusable() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "a send to the phone timed out, so the link can't be trusted any more",
    )
}

impl Write for Guarded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.broken {
            return Err(unusable());
        }
        match self.sock.write(buf) {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                self.broken = true;
                Err(unusable())
            }
            other => other,
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.broken {
            return Err(unusable());
        }
        self.sock.flush()
    }
}

impl Read for Guarded {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.sock.read(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn refuses_to_write_after_a_send_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let sock = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = listener.accept().unwrap();
        sock.set_write_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut guarded = Guarded::new(sock);

        let chunk = vec![0u8; 1024 * 1024];
        let error = (0..1024)
            .find_map(|_| guarded.write(&chunk).err())
            .expect("a send should have timed out");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);

        assert_eq!(
            guarded.write(b"x").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            guarded.flush().unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
