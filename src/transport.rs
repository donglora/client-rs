//! Transport abstraction for byte-stream connections.
//!
//! The [`Transport`] trait unifies serial ports and mux socket connections,
//! allowing the [`Client`](crate::client::Client) to work over any of them.

use std::io::{self, Read, Write};
use std::time::Duration;

/// A byte-stream transport that the client can talk over.
pub trait Transport: Read + Write + Send {
    /// Set the read timeout. A read that takes longer returns zero bytes.
    fn set_timeout(&mut self, timeout: Duration) -> anyhow::Result<()>;

    /// Get the current read timeout.
    fn timeout(&self) -> Duration;
}

// ── Serial transport ───────────────────────────────────────────────

/// Direct USB serial port connection to a DongLoRa dongle.
pub struct SerialTransport {
    port: Box<dyn serialport::SerialPort>,
}

impl SerialTransport {
    /// Open a serial port with the given timeout.
    ///
    /// Baud rate is irrelevant for USB CDC-ACM but we pass a conventional value.
    pub fn open(path: &str, timeout: Duration) -> anyhow::Result<Self> {
        let port = serialport::new(path, 115_200)
            .timeout(timeout)
            .open()
            .map_err(|e| anyhow::anyhow!("failed to open serial port {path}: {e}"))?;
        Ok(Self { port })
    }

    /// Clear the serial input buffer.
    pub fn reset_input_buffer(&self) -> anyhow::Result<()> {
        self.port
            .clear(serialport::ClearBuffer::Input)
            .map_err(|e| anyhow::anyhow!("failed to clear input buffer: {e}"))
    }
}

impl Read for SerialTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.port.read(buf)
    }
}

impl Write for SerialTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.port.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.port.flush()
    }
}

impl Transport for SerialTransport {
    fn set_timeout(&mut self, timeout: Duration) -> anyhow::Result<()> {
        self.port
            .set_timeout(timeout)
            .map_err(|e| anyhow::anyhow!("failed to set serial timeout: {e}"))
    }

    fn timeout(&self) -> Duration {
        // serialport doesn't have a getter, so we'd need to track it ourselves.
        // For now, return a sensible default. The actual timeout is set on the port.
        Duration::from_secs(2)
    }
}

// ── Mux transport ──────────────────────────────────────────────────

/// Connection to the DongLoRa mux daemon via Unix socket or TCP.
pub struct MuxTransport {
    stream: MuxStream,
    timeout: Duration,
}

enum MuxStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    Tcp(std::net::TcpStream),
}

impl MuxTransport {
    /// Connect to the mux daemon via Unix domain socket.
    #[cfg(unix)]
    pub fn unix(path: &str, timeout: Duration) -> anyhow::Result<Self> {
        let stream = std::os::unix::net::UnixStream::connect(path)
            .map_err(|e| anyhow::anyhow!("failed to connect to mux socket {path}: {e}"))?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| anyhow::anyhow!("failed to set socket timeout: {e}"))?;
        Ok(Self {
            stream: MuxStream::Unix(stream),
            timeout,
        })
    }

    /// Connect to the mux daemon via TCP.
    pub fn tcp(host: &str, port: u16, timeout: Duration) -> anyhow::Result<Self> {
        let addr = format!("{host}:{port}");
        let stream = std::net::TcpStream::connect(&addr)
            .map_err(|e| anyhow::anyhow!("failed to connect to mux at {addr}: {e}"))?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| anyhow::anyhow!("failed to set TCP timeout: {e}"))?;
        Ok(Self {
            stream: MuxStream::Tcp(stream),
            timeout,
        })
    }
}

impl Read for MuxTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.stream {
            #[cfg(unix)]
            MuxStream::Unix(s) => s.read(buf),
            MuxStream::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for MuxTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.stream {
            #[cfg(unix)]
            MuxStream::Unix(s) => s.write(buf),
            MuxStream::Tcp(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.stream {
            #[cfg(unix)]
            MuxStream::Unix(s) => s.flush(),
            MuxStream::Tcp(s) => s.flush(),
        }
    }
}

impl Transport for MuxTransport {
    fn set_timeout(&mut self, timeout: Duration) -> anyhow::Result<()> {
        self.timeout = timeout;
        let result = match &self.stream {
            #[cfg(unix)]
            MuxStream::Unix(s) => s.set_read_timeout(Some(timeout)),
            MuxStream::Tcp(s) => s.set_read_timeout(Some(timeout)),
        };
        result.map_err(|e| anyhow::anyhow!("failed to set timeout: {e}"))
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }
}

// ── AnyTransport ───────────────────────────────────────────────────

/// Type-erased transport for the [`connect`](crate::connect::connect) return type.
pub enum AnyTransport {
    Serial(SerialTransport),
    Mux(MuxTransport),
}

impl Read for AnyTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Serial(t) => t.read(buf),
            Self::Mux(t) => t.read(buf),
        }
    }
}

impl Write for AnyTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Serial(t) => t.write(buf),
            Self::Mux(t) => t.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Serial(t) => t.flush(),
            Self::Mux(t) => t.flush(),
        }
    }
}

impl Transport for AnyTransport {
    fn set_timeout(&mut self, timeout: Duration) -> anyhow::Result<()> {
        match self {
            Self::Serial(t) => t.set_timeout(timeout),
            Self::Mux(t) => t.set_timeout(timeout),
        }
    }

    fn timeout(&self) -> Duration {
        match self {
            Self::Serial(t) => t.timeout(),
            Self::Mux(t) => t.timeout(),
        }
    }
}
