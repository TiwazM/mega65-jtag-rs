//! Byte transports to the MEGA65 serial monitor: a real UART, or TCP (e.g. Xemu's
//! `-uartmon :4510`).

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::{Error, Result};

/// Default speed of the MEGA65 serial monitor UART.
pub const DEFAULT_BAUD: u32 = 2_000_000;

pub trait Transport: Send {
    /// Write all bytes.
    fn write_all(&mut self, data: &[u8]) -> Result<()>;
    /// Read whatever is available, waiting at most `timeout`. Returns 0 on timeout.
    fn read_some(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize>;
    /// Human readable description of the endpoint.
    fn describe(&self) -> String;
}

pub struct SerialTransport {
    port: Box<dyn serialport::SerialPort>,
    name: String,
}

impl SerialTransport {
    pub fn open(path: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(path, baud)
            .timeout(Duration::from_millis(10))
            .flow_control(serialport::FlowControl::None)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .open()
            .map_err(|e| Error::Open(format!("{path}: {e}")))?;
        Ok(Self { port, name: format!("{path}@{baud}") })
    }
}

impl Transport for SerialTransport {
    fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.port.write_all(data)?;
        self.port.flush()?;
        Ok(())
    }

    fn read_some(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        self.port.set_timeout(timeout.max(Duration::from_millis(1)))?;
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    fn describe(&self) -> String {
        self.name.clone()
    }
}

pub struct TcpTransport {
    stream: TcpStream,
    name: String,
}

impl TcpTransport {
    pub fn connect(addr: &str) -> Result<Self> {
        let stream = TcpStream::connect(addr).map_err(|e| Error::Open(format!("{addr}: {e}")))?;
        stream.set_nodelay(true)?;
        Ok(Self { stream, name: format!("tcp:{addr}") })
    }
}

impl Transport for TcpTransport {
    fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.stream.write_all(data)?;
        Ok(())
    }

    fn read_some(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        self.stream.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        match self.stream.read(buf) {
            Ok(0) => Err(Error::Protocol("TCP connection closed".into())),
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    fn describe(&self) -> String {
        self.name.clone()
    }
}

/// Xemu's `-uartmon` on macOS/Linux is a unix-domain socket.
#[cfg(unix)]
pub struct UnixTransport {
    stream: std::os::unix::net::UnixStream,
    name: String,
}

#[cfg(unix)]
impl UnixTransport {
    pub fn connect(path: &str) -> Result<Self> {
        let stream = std::os::unix::net::UnixStream::connect(path).map_err(|e| Error::Open(format!("{path}: {e}")))?;
        Ok(Self { stream, name: format!("unix:{path}") })
    }
}

#[cfg(unix)]
impl Transport for UnixTransport {
    fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.stream.write_all(data)?;
        Ok(())
    }

    fn read_some(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        self.stream.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        match self.stream.read(buf) {
            Ok(0) => Err(Error::Protocol("socket closed (did Xemu exit?)".into())),
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    fn describe(&self) -> String {
        self.name.clone()
    }
}

/// Open a transport from a device string: `tcp:host:port`, `tcp` (localhost:4510),
/// `unix:/path` (or a path that is a socket), or a serial port path.
pub fn open(device: &str, baud: u32) -> Result<Box<dyn Transport>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let path = device.strip_prefix("unix:").unwrap_or(device);
        let is_socket = std::fs::metadata(path).map(|m| m.file_type().is_socket()).unwrap_or(false);
        if device.starts_with("unix:") || is_socket {
            return Ok(Box::new(UnixTransport::connect(path)?));
        }
    }
    if let Some(rest) = device.strip_prefix("tcp:") {
        let addr = if rest.contains(':') { rest.to_string() } else { format!("{rest}:4510") };
        Ok(Box::new(TcpTransport::connect(&addr)?))
    } else if device == "tcp" {
        Ok(Box::new(TcpTransport::connect("localhost:4510")?))
    } else {
        Ok(Box::new(SerialTransport::open(device, baud)?))
    }
}

/// Information about a candidate serial port.
#[derive(Debug, Clone)]
pub struct PortInfo {
    pub path: String,
    pub description: String,
    pub likely_mega65: bool,
}

/// List serial ports, flagging FTDI-based ones that are probably a MEGA65 UART.
pub fn list_ports() -> Vec<PortInfo> {
    let mut out = Vec::new();
    if let Ok(ports) = serialport::available_ports() {
        for p in ports {
            // macOS exposes both /dev/tty.* and /dev/cu.*; the cu.* nodes are the right ones.
            if cfg!(target_os = "macos") && p.port_name.starts_with("/dev/tty.") {
                continue;
            }
            let (desc, ftdi) = match &p.port_type {
                serialport::SerialPortType::UsbPort(u) => (
                    format!(
                        "USB {:04x}:{:04x} {} {}",
                        u.vid,
                        u.pid,
                        u.manufacturer.clone().unwrap_or_default(),
                        u.product.clone().unwrap_or_default()
                    ),
                    u.vid == 0x0403,
                ),
                other => (format!("{other:?}"), p.port_name.contains("usbserial")),
            };
            out.push(PortInfo { path: p.port_name, description: desc, likely_mega65: ftdi });
        }
    }
    out
}

/// Pick the most likely MEGA65 UART. On FT2232-based adapters interface A is JTAG and
/// interface B is the UART, so prefer the highest numbered port of an FTDI pair.
pub fn autodetect_port() -> Option<String> {
    let mut c: Vec<_> = list_ports().into_iter().filter(|p| p.likely_mega65).map(|p| p.path).collect();
    c.sort();
    c.pop()
}
