//! Host intrinsics behind `lusa::fs` and `lusa::net` (Module 29).
//!
//! The public API is Lusa source in the bundled crate (`src/std/*.lusa`); it
//! calls these `__fs_*` / `__net_*` builtins and turns their status into
//! `Result` values.  Every intrinsic records whether it succeeded:
//! `__io_ok()` reports it, `__io_error()` holds the message of the last
//! failure, and `__io_eof()` tells a line read that hit the end apart from an
//! empty line.  Files and sockets are numbered handles; the native runtime
//! (`compiler_bootstrap.lusa`) implements the same intrinsics over stdio,
//! POSIX sockets, and Winsock.
//!
//! The dynamic engines hold text as UTF-8 strings, so bytes that are not
//! valid UTF-8 are replaced when they are read.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::Arc;

use crate::error::{LResult, LusaError, Span, E_CALL};
use crate::interp::Interp;
use crate::value::Value;

type BF = Box<dyn Fn(&mut Interp, Vec<Value>, Span) -> LResult<Vec<Value>>>;

enum Resource {
    Reader(BufReader<File>),
    Writer(File),
    Listener(TcpListener),
    Stream(TcpStream),
    Udp(UdpSocket, Option<SocketAddr>),
}

/// Open handles and the status of the last host call.
pub struct HostState {
    handles: HashMap<i32, Resource>,
    next: i32,
    error: Option<String>,
    eof: bool,
}

impl Default for HostState {
    fn default() -> Self {
        HostState {
            handles: HashMap::new(),
            next: 3,
            error: None,
            eof: false,
        }
    }
}

impl HostState {
    fn ok(&mut self) {
        self.error = None;
        self.eof = false;
    }

    fn fail(&mut self, message: String) {
        self.error = Some(message);
        self.eof = false;
    }

    fn add(&mut self, resource: Resource) -> i32 {
        let handle = self.next;
        self.next += 1;
        self.handles.insert(handle, resource);
        handle
    }
}

fn arg<'a>(args: &'a [Value], index: usize, name: &str, span: Span) -> LResult<&'a Value> {
    args.get(index)
        .ok_or_else(|| LusaError::new(E_CALL, Some(span), format!("{name}: missing argument {}", index + 1)))
}

fn text(args: &[Value], index: usize, name: &str, span: Span) -> LResult<String> {
    match arg(args, index, name, span)?.base_value() {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(LusaError::new(
            E_CALL,
            Some(span),
            format!("{name}: expected a string, got {} ({})", other.type_name(), other.fmt_value()),
        )),
    }
}

fn number(args: &[Value], index: usize, name: &str, span: Span) -> LResult<i64> {
    match arg(args, index, name, span)?.base_value() {
        Value::Number(n) if n.fract() == 0.0 => Ok(*n as i64),
        other => Err(LusaError::new(
            E_CALL,
            Some(span),
            format!("{name}: expected an integer, got {} ({})", other.type_name(), other.fmt_value()),
        )),
    }
}

fn flag(args: &[Value], index: usize, name: &str, span: Span) -> LResult<bool> {
    match arg(args, index, name, span)?.base_value() {
        Value::Bool(b) => Ok(*b),
        other => Err(LusaError::new(
            E_CALL,
            Some(span),
            format!("{name}: expected a bool, got {} ({})", other.type_name(), other.fmt_value()),
        )),
    }
}

fn s(value: impl AsRef<str>) -> Vec<Value> {
    vec![Value::Str(Arc::from(value.as_ref()))]
}

fn n(value: i64) -> Vec<Value> {
    vec![Value::Number(value as f64)]
}

fn b(value: bool) -> Vec<Value> {
    vec![Value::Bool(value)]
}

fn resolve(address: &str) -> std::io::Result<SocketAddr> {
    address.to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "address resolved to nothing")
    })
}

fn bad_handle(handle: i64) -> String {
    format!("handle {handle} is not open")
}

// ---- status ----------------------------------------------------------------

fn io_ok(i: &mut Interp, _: Vec<Value>, _: Span) -> LResult<Vec<Value>> {
    Ok(b(i.host.error.is_none()))
}

fn io_error(i: &mut Interp, _: Vec<Value>, _: Span) -> LResult<Vec<Value>> {
    Ok(s(i.host.error.clone().unwrap_or_default()))
}

fn io_eof(i: &mut Interp, _: Vec<Value>, _: Span) -> LResult<Vec<Value>> {
    Ok(b(i.host.eof))
}

// ---- files -----------------------------------------------------------------

fn fs_read(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let path = text(&args, 0, "__fs_read", span)?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            i.host.ok();
            Ok(s(String::from_utf8_lossy(&bytes)))
        }
        Err(e) => {
            i.host.fail(format!("cannot read `{path}`: {e}"));
            Ok(s(""))
        }
    }
}

fn fs_write(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let path = text(&args, 0, "__fs_write", span)?;
    let contents = text(&args, 1, "__fs_write", span)?;
    let append = flag(&args, 2, "__fs_write", span)?;
    let result = OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(&path)
        .and_then(|mut file| file.write_all(contents.as_bytes()));
    match result {
        Ok(()) => {
            i.host.ok();
            Ok(n(contents.len() as i64))
        }
        Err(e) => {
            i.host.fail(format!("cannot write `{path}`: {e}"));
            Ok(n(0))
        }
    }
}

fn fs_exists(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let path = text(&args, 0, "__fs_exists", span)?;
    i.host.ok();
    Ok(b(std::path::Path::new(&path).exists()))
}

fn fs_is_dir(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let path = text(&args, 0, "__fs_is_dir", span)?;
    i.host.ok();
    Ok(b(std::path::Path::new(&path).is_dir()))
}

fn fs_mkdir(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let path = text(&args, 0, "__fs_mkdir", span)?;
    let all = flag(&args, 1, "__fs_mkdir", span)?;
    let result = if all {
        std::fs::create_dir_all(&path)
    } else {
        std::fs::create_dir(&path)
    };
    match result {
        Ok(()) => {
            i.host.ok();
            Ok(b(true))
        }
        Err(e) => {
            i.host.fail(format!("cannot create directory `{path}`: {e}"));
            Ok(b(false))
        }
    }
}

fn fs_remove(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let path = text(&args, 0, "__fs_remove", span)?;
    let dir = flag(&args, 1, "__fs_remove", span)?;
    let result = if dir {
        std::fs::remove_dir(&path)
    } else {
        std::fs::remove_file(&path)
    };
    match result {
        Ok(()) => {
            i.host.ok();
            Ok(b(true))
        }
        Err(e) => {
            i.host.fail(format!("cannot remove `{path}`: {e}"));
            Ok(b(false))
        }
    }
}

fn fs_size(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let path = text(&args, 0, "__fs_size", span)?;
    match std::fs::metadata(&path) {
        Ok(meta) => {
            i.host.ok();
            Ok(n(meta.len().min(u32::MAX as u64) as i64))
        }
        Err(e) => {
            i.host.fail(format!("cannot inspect `{path}`: {e}"));
            Ok(n(0))
        }
    }
}

fn fs_open(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let path = text(&args, 0, "__fs_open", span)?;
    let mode = number(&args, 1, "__fs_open", span)?;
    let opened = match mode {
        0 => File::open(&path).map(|f| Resource::Reader(BufReader::new(f))),
        1 => File::create(&path).map(Resource::Writer),
        _ => OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .map(Resource::Writer),
    };
    match opened {
        Ok(resource) => {
            i.host.ok();
            Ok(n(i.host.add(resource) as i64))
        }
        Err(e) => {
            i.host.fail(format!("cannot open `{path}`: {e}"));
            Ok(n(-1))
        }
    }
}

/// Reads one line without its `\n` (and a `\r` before it).  At the end of
/// the input the result is empty and `__io_eof()` is true.
fn read_line_from(reader: &mut dyn Read) -> std::io::Result<(Vec<u8>, bool)> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte)? {
            0 => return Ok((line.clone(), line.is_empty())),
            _ if byte[0] == b'\n' => {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok((line, false));
            }
            _ => line.push(byte[0]),
        }
    }
}

fn fs_read_line(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "__fs_read_line", span)?;
    let result = match i.host.handles.get_mut(&(handle as i32)) {
        Some(Resource::Reader(reader)) => {
            let mut buffer = String::new();
            reader.read_line(&mut buffer).map(|count| {
                if buffer.ends_with('\n') {
                    buffer.pop();
                    if buffer.ends_with('\r') {
                        buffer.pop();
                    }
                }
                (buffer, count == 0)
            })
        }
        _ => {
            i.host.fail(bad_handle(handle));
            return Ok(s(""));
        }
    };
    match result {
        Ok((line, eof)) => {
            i.host.ok();
            i.host.eof = eof;
            Ok(s(line))
        }
        Err(e) => {
            i.host.fail(format!("cannot read handle {handle}: {e}"));
            Ok(s(""))
        }
    }
}

fn fs_read_all(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "__fs_read_all", span)?;
    let result = match i.host.handles.get_mut(&(handle as i32)) {
        Some(Resource::Reader(reader)) => {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).map(|_| bytes)
        }
        _ => {
            i.host.fail(bad_handle(handle));
            return Ok(s(""));
        }
    };
    match result {
        Ok(bytes) => {
            i.host.ok();
            Ok(s(String::from_utf8_lossy(&bytes)))
        }
        Err(e) => {
            i.host.fail(format!("cannot read handle {handle}: {e}"));
            Ok(s(""))
        }
    }
}

fn fs_write_handle(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "__fs_write_handle", span)?;
    let contents = text(&args, 1, "__fs_write_handle", span)?;
    let result = match i.host.handles.get_mut(&(handle as i32)) {
        Some(Resource::Writer(file)) => file.write_all(contents.as_bytes()),
        _ => {
            i.host.fail(bad_handle(handle));
            return Ok(n(0));
        }
    };
    match result {
        Ok(()) => {
            i.host.ok();
            Ok(n(contents.len() as i64))
        }
        Err(e) => {
            i.host.fail(format!("cannot write handle {handle}: {e}"));
            Ok(n(0))
        }
    }
}

fn close(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "close", span)?;
    match i.host.handles.remove(&(handle as i32)) {
        Some(_) => i.host.ok(),
        None => i.host.fail(bad_handle(handle)),
    }
    Ok(Vec::new())
}

// ---- sockets ---------------------------------------------------------------

fn net_listen(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let address = text(&args, 0, "__net_listen", span)?;
    match resolve(&address).and_then(TcpListener::bind) {
        Ok(listener) => {
            i.host.ok();
            Ok(n(i.host.add(Resource::Listener(listener)) as i64))
        }
        Err(e) => {
            i.host.fail(format!("cannot listen on `{address}`: {e}"));
            Ok(n(-1))
        }
    }
}

fn net_accept(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "__net_accept", span)?;
    let accepted = match i.host.handles.get(&(handle as i32)) {
        Some(Resource::Listener(listener)) => listener.accept(),
        _ => {
            i.host.fail(bad_handle(handle));
            return Ok(n(-1));
        }
    };
    match accepted {
        Ok((stream, _)) => {
            i.host.ok();
            Ok(n(i.host.add(Resource::Stream(stream)) as i64))
        }
        Err(e) => {
            i.host.fail(format!("accept failed: {e}"));
            Ok(n(-1))
        }
    }
}

fn net_connect(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let address = text(&args, 0, "__net_connect", span)?;
    match resolve(&address).and_then(|addr| TcpStream::connect(addr)) {
        Ok(stream) => {
            i.host.ok();
            Ok(n(i.host.add(Resource::Stream(stream)) as i64))
        }
        Err(e) => {
            i.host.fail(format!("cannot connect to `{address}`: {e}"));
            Ok(n(-1))
        }
    }
}

fn net_port(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "__net_port", span)?;
    let local = match i.host.handles.get(&(handle as i32)) {
        Some(Resource::Listener(l)) => l.local_addr(),
        Some(Resource::Stream(s)) => s.local_addr(),
        Some(Resource::Udp(u, _)) => u.local_addr(),
        _ => {
            i.host.fail(bad_handle(handle));
            return Ok(n(0));
        }
    };
    match local {
        Ok(addr) => {
            i.host.ok();
            Ok(n(addr.port() as i64))
        }
        Err(e) => {
            i.host.fail(format!("cannot read the local address: {e}"));
            Ok(n(0))
        }
    }
}

fn net_send(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "__net_send", span)?;
    let data = text(&args, 1, "__net_send", span)?;
    let result = match i.host.handles.get_mut(&(handle as i32)) {
        Some(Resource::Stream(stream)) => stream.write_all(data.as_bytes()).and_then(|_| stream.flush()),
        _ => {
            i.host.fail(bad_handle(handle));
            return Ok(n(0));
        }
    };
    match result {
        Ok(()) => {
            i.host.ok();
            Ok(n(data.len() as i64))
        }
        Err(e) => {
            i.host.fail(format!("send failed: {e}"));
            Ok(n(0))
        }
    }
}

fn net_recv(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "__net_recv", span)?;
    let max = number(&args, 1, "__net_recv", span)?.clamp(1, 65536) as usize;
    let result = match i.host.handles.get_mut(&(handle as i32)) {
        Some(Resource::Stream(stream)) => {
            let mut buffer = vec![0u8; max];
            stream.read(&mut buffer).map(|count| {
                buffer.truncate(count);
                buffer
            })
        }
        _ => {
            i.host.fail(bad_handle(handle));
            return Ok(s(""));
        }
    };
    match result {
        Ok(bytes) => {
            i.host.ok();
            i.host.eof = bytes.is_empty();
            Ok(s(String::from_utf8_lossy(&bytes)))
        }
        Err(e) => {
            i.host.fail(format!("receive failed: {e}"));
            Ok(s(""))
        }
    }
}

fn net_recv_line(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "__net_recv_line", span)?;
    let result = match i.host.handles.get_mut(&(handle as i32)) {
        Some(Resource::Stream(stream)) => read_line_from(stream),
        _ => {
            i.host.fail(bad_handle(handle));
            return Ok(s(""));
        }
    };
    match result {
        Ok((line, eof)) => {
            i.host.ok();
            i.host.eof = eof;
            Ok(s(String::from_utf8_lossy(&line)))
        }
        Err(e) => {
            i.host.fail(format!("receive failed: {e}"));
            Ok(s(""))
        }
    }
}

fn net_udp_bind(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let address = text(&args, 0, "__net_udp_bind", span)?;
    match resolve(&address).and_then(UdpSocket::bind) {
        Ok(socket) => {
            i.host.ok();
            Ok(n(i.host.add(Resource::Udp(socket, None)) as i64))
        }
        Err(e) => {
            i.host.fail(format!("cannot bind `{address}`: {e}"));
            Ok(n(-1))
        }
    }
}

fn net_send_to(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "__net_send_to", span)?;
    let data = text(&args, 1, "__net_send_to", span)?;
    let address = text(&args, 2, "__net_send_to", span)?;
    let result = match i.host.handles.get(&(handle as i32)) {
        Some(Resource::Udp(socket, _)) => resolve(&address).and_then(|addr| socket.send_to(data.as_bytes(), addr)),
        _ => {
            i.host.fail(bad_handle(handle));
            return Ok(n(0));
        }
    };
    match result {
        Ok(count) => {
            i.host.ok();
            Ok(n(count as i64))
        }
        Err(e) => {
            i.host.fail(format!("cannot send to `{address}`: {e}"));
            Ok(n(0))
        }
    }
}

fn net_recv_from(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "__net_recv_from", span)?;
    let max = number(&args, 1, "__net_recv_from", span)?.clamp(1, 65536) as usize;
    let result = match i.host.handles.get_mut(&(handle as i32)) {
        Some(Resource::Udp(socket, last)) => {
            let mut buffer = vec![0u8; max];
            socket.recv_from(&mut buffer).map(|(count, from)| {
                *last = Some(from);
                buffer.truncate(count);
                buffer
            })
        }
        _ => {
            i.host.fail(bad_handle(handle));
            return Ok(s(""));
        }
    };
    match result {
        Ok(bytes) => {
            i.host.ok();
            Ok(s(String::from_utf8_lossy(&bytes)))
        }
        Err(e) => {
            i.host.fail(format!("receive failed: {e}"));
            Ok(s(""))
        }
    }
}

fn net_peer(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let handle = number(&args, 0, "__net_peer", span)?;
    let peer = match i.host.handles.get(&(handle as i32)) {
        Some(Resource::Stream(stream)) => stream.peer_addr().ok(),
        Some(Resource::Udp(_, last)) => *last,
        _ => {
            i.host.fail(bad_handle(handle));
            return Ok(s(""));
        }
    };
    i.host.ok();
    Ok(s(peer.map(|addr| addr.to_string()).unwrap_or_default()))
}

/// The intrinsics, installed as global builtins.
pub fn builtins() -> Vec<(&'static str, BF)> {
    vec![
        ("__io_ok", Box::new(io_ok) as BF),
        ("__io_error", Box::new(io_error)),
        ("__io_eof", Box::new(io_eof)),
        ("__fs_read", Box::new(fs_read)),
        ("__fs_write", Box::new(fs_write)),
        ("__fs_exists", Box::new(fs_exists)),
        ("__fs_is_dir", Box::new(fs_is_dir)),
        ("__fs_mkdir", Box::new(fs_mkdir)),
        ("__fs_remove", Box::new(fs_remove)),
        ("__fs_size", Box::new(fs_size)),
        ("__fs_open", Box::new(fs_open)),
        ("__fs_read_line", Box::new(fs_read_line)),
        ("__fs_read_all", Box::new(fs_read_all)),
        ("__fs_write_handle", Box::new(fs_write_handle)),
        ("__fs_close", Box::new(close)),
        ("__net_listen", Box::new(net_listen)),
        ("__net_accept", Box::new(net_accept)),
        ("__net_connect", Box::new(net_connect)),
        ("__net_port", Box::new(net_port)),
        ("__net_send", Box::new(net_send)),
        ("__net_recv", Box::new(net_recv)),
        ("__net_recv_line", Box::new(net_recv_line)),
        ("__net_close", Box::new(close)),
        ("__net_udp_bind", Box::new(net_udp_bind)),
        ("__net_send_to", Box::new(net_send_to)),
        ("__net_recv_from", Box::new(net_recv_from)),
        ("__net_peer", Box::new(net_peer)),
    ]
}
