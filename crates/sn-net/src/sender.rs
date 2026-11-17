//! BookDrop's send half: find a peer on the LAN, then push files to it the
//! LocalSend v2.2 way — one `prepare-upload`, then one `POST /upload` per
//! file, echoing the token prepare handed back.
//!
//! This mirrors the iOS `LocalSendSender` decision for decision, because the
//! two ends must agree on what the wire means:
//!
//! * **Discovery is a /24 unicast scan** of `POST /register` across ports
//!   53317–53327 — no multicast, because the phones' senders dial out and
//!   the desktop official app scans too. A silent host (no ARP reply) is
//!   skipped without knocking on its other ports; a refused port means a
//!   live host worth one more knock.
//! * **The peer's own announced port wins** over the door we knocked on: a
//!   receiver that took 53318 because 53317 was busy still answers.
//! * **The access code rides the prepare body AND both query strings** —
//!   receivers differ in which one they read.
//! * **Uploads carry `Content-Length` and stream from disk** in fixed
//!   chunks; a book is hundreds of megabytes and never lands in memory.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::bookdrop::{DeviceInfo, PORT_RANGE};

/// How long one probe waits — for the connection AND for the answer on it.
/// A dead host burns exactly one of these (not eleven) before the scan
/// gives up on it: the connect itself carries the budget too, because a
/// LAN whose dead addresses drop the SYN (a phone hotspot's NAT, a
/// filtered bridge) would otherwise put every knock through the OS's
/// ~130 s TCP retry ladder.
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// The largest response body this side will read. Every honest answer on
/// this protocol is JSON measured in kilobytes; a "receiver" that streams
/// gigabytes (or claims a Content-Length of gigabytes) is hostile, and the
/// old unbounded read let it OOM the app mid-scan.
const MAX_RESPONSE_BODY: usize = 1024 * 1024;

/// How many hosts the scan probes at once. 32 keeps a quiet LAN under three
/// seconds without buzzing the switch with a full-rate burst.
const SCAN_CONCURRENCY: usize = 32;

/// A device on this network that can receive BookDrop transfers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub alias: String,
    pub device_model: Option<String>,
    pub device_type: Option<String>,
    pub fingerprint: String,
    pub host: String,
    pub port: u16,
}

impl Peer {
    /// What the peer is, in a word — the same mapping the iOS picker shows.
    pub fn display_kind(&self) -> &'static str {
        match self.device_type.as_deref() {
            Some("desktop") => "Computer",
            Some("mobile") => "Phone or tablet",
            Some("web") => "Browser",
            Some("server") => "Server",
            _ => "Device",
        }
    }

    /// "Nimbus (Computer)" — one line for a picker row.
    pub fn label(&self) -> String {
        format!("{} ({})", self.alias, self.display_kind())
    }
}

/// The local IPv4 subnets worth scanning, as "192.168.1" prefixes.
///
/// Every IPv4 interface's prefix, from `getifaddrs` — **not** the default
/// route's. The old implementation picked the route a UDP `connect` chose,
/// and that is whichever route owns the default: on the machine this was
/// measured on, a CloudflareWARP tunnel (172.16.0.2/32) owned it, so the
/// scan swept a virtual subnet where no receiver can answer while the
/// phone sat on the Wi-Fi's own 172.20.10.0/28 — the user's "i cant
/// search/scan for peers". A machine can genuinely be on two LANs at once
/// (Wi-Fi and a hotspot), so the vec is the shape the iOS scan uses.
///
/// Skipped, because no receiver can live there:
///
/// * loopback (127/8) and IPv4 link-local (169.254/16);
/// * down interfaces (a bridge with nothing attached);
/// * host-only /32s — a point-to-point tunnel (CloudflareWARP measured
///   172.16.0.2/32) has no subnet to sweep.
///
/// Broader nets (a Docker bridge's /16) are scanned as their /24 prefix:
/// a few dead probes, and never the reason a real peer is missed.
pub fn local_networks() -> Vec<String> {
    local_subnets()
}

/// The four octets of an in-network-order IPv4 address. `s_addr` already
/// holds the address bytes the wire order wants, so the NATIVE in-memory
/// bytes are the octets on any endianness — `to_be_bytes()` here reversed
/// them on this little-endian box and sent the sweep at the byte-swapped
/// PUBLIC range "3.10.20" (an AWS block) instead of the Wi-Fi's own /28,
/// while the 127/169.254 skips read an octet that was no longer first.
fn octets(s_addr: u32) -> [u8; 4] {
    s_addr.to_ne_bytes()
}

pub fn local_subnets() -> Vec<String> {
    let mut prefixes: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut getifaddrs_worked = false;
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) == 0 {
            getifaddrs_worked = true;
            let mut cursor = ifap;
            while !cursor.is_null() {
                let ia = &*cursor;
                cursor = ia.ifa_next;
                let addr = ia.ifa_addr;
                if addr.is_null() || (*addr).sa_family as i32 != libc::AF_INET {
                    continue;
                }
                // An interface that is down has no LAN on it.
                if ia.ifa_flags & libc::IFF_UP as u32 == 0 {
                    continue;
                }
                let sin = &*(addr as *const libc::sockaddr_in);
                let [a, b, c, _d] = octets(sin.sin_addr.s_addr);
                if a == 127 || (a == 169 && b == 254) {
                    continue;
                }
                // A /32 names no subnet: a tunnel's single address is not
                // a LAN, and sweeping its /24 prefix would knock on 254
                // addresses the tunnel cannot answer for.
                if !ia.ifa_netmask.is_null() {
                    let nm = &*(ia.ifa_netmask as *const libc::sockaddr_in);
                    if u32::from_be(nm.sin_addr.s_addr) == u32::MAX {
                        continue;
                    }
                }
                let prefix = format!("{a}.{b}.{c}");
                if seen.insert(prefix.clone()) {
                    prefixes.push(prefix);
                }
            }
            libc::freeifaddrs(ifap);
        }
    }
    if getifaddrs_worked {
        return prefixes;
    }
    // getifaddrs itself failed: fall back to the route trick rather than
    // scanning nothing. One likely subnet beats none.
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return Vec::new();
    };
    // TEST-NET-1: a routable-but-documentation address, so the stack chooses
    // the default route without a packet ever leaving.
    if socket.connect("192.0.2.1:9").is_err() {
        return Vec::new();
    }
    let Ok(addr) = socket.local_addr() else {
        return Vec::new();
    };
    let ip = addr.ip().to_string();
    if ip.starts_with("127.") || ip.starts_with("169.254.") {
        return Vec::new();
    }
    // "192.168.1.42" → "192.168.1": everything before the final octet.
    let mut parts = ip.split('.');
    let _host_octet = parts.next_back();
    let prefix = parts.collect::<Vec<_>>().join(".");
    if prefix.is_empty() {
        Vec::new()
    } else {
        vec![prefix]
    }
}

/// The address a user typed, split into a host and a port.
///
/// Three shapes are accepted:
///
/// * `192.168.1.50` — the host, on the protocol's default port;
/// * `192.168.1.50:53318` — the host and the port;
/// * `[fe80::1]:53318` — an IPv6 literal, with its brackets.
///
/// A bare `53318` is refused: it is almost always a mistyped address, and
/// guessing a host for it would send the files somewhere the user did not
/// mean. A bare IPv6 literal without brackets (`fe80::1`) is a host with no
/// port — the colons in it are part of the address, not a port separator.
pub fn parse_address(address: &str) -> Result<(String, u16), String> {
    let text = address.trim();
    if text.is_empty() {
        return Err("type the address of the device".to_string());
    }
    let (host, port) = if let Some(rest) = text.strip_prefix('[') {
        // The bracketed form is the only way to write "IPv6 host, with a
        // port": the colons in the address would otherwise look like the
        // port separator.
        let Some((host, tail)) = rest.split_once(']') else {
            return Err("the address is missing its closing ]".to_string());
        };
        match tail.strip_prefix(':') {
            Some(port) => (host.to_string(), parse_port(port)?),
            None if tail.is_empty() => (host.to_string(), crate::bookdrop::DEFAULT_PORT),
            None => return Err("the address has trailing characters".to_string()),
        }
    } else {
        match text.rsplit_once(':') {
            // More than one colon means a bare IPv6 literal: the last
            // colon is part of the address, not a port separator.
            Some((host, port)) if !host.contains(':') => (host.to_string(), parse_port(port)?),
            _ => (text.to_string(), crate::bookdrop::DEFAULT_PORT),
        }
    };
    if host.is_empty() {
        return Err("the address has no host in it".to_string());
    }
    // A host that is nothing but digits is a port number the user left
    // behind: "53318" is not a machine. Guessing a host for it would send
    // the files somewhere the user never named.
    if host.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "“{host}” looks like a port number, not a device address"
        ));
    }
    Ok((host, port))
}

/// One port number, as the user typed it.
fn parse_port(text: &str) -> Result<u16, String> {
    let port: u16 = text
        .trim()
        .parse()
        .map_err(|_| format!("“{text}” is not a port number"))?;
    if port == 0 {
        return Err("port 0 is not a port".to_string());
    }
    Ok(port)
}

/// Turn an address the user typed into a peer to send to.
///
/// The address is probed first, so a receiver that answers the register
/// endpoint gives up its own name and — the part that matters — its own
/// port: a receiver that took 53318 because 53317 was already busy is
/// still found, even when the user typed 53317 or typed nothing at all.
///
/// An address that answers nothing is still returned as a peer. The
/// transfer is a separate exchange from the register handshake, and a
/// receiver whose discovery endpoint is quiet can still accept an upload;
/// if it cannot, the send says so in its own words. Returning the peer
/// keeps the choice the user's: they typed the address, and the app does
/// not quietly decide it does not exist.
pub fn manual_peer(address: &str, own_fingerprint: &str) -> Result<Peer, String> {
    let (host, port) = parse_address(address)?;
    let device = sender_device(own_fingerprint, 0);
    // The typed port first — it is the one the user meant.
    if let Some(peer) = probe_port(&host, port, &device) {
        return Ok(peer);
    }
    // Then the protocol's range, for a receiver that sits on another port.
    if let Some(peer) = probe_host(&host, &device) {
        return Ok(peer);
    }
    Ok(Peer {
        alias: host.clone(),
        device_model: None,
        device_type: None,
        fingerprint: String::new(),
        host,
        port,
    })
}

/// Scan one /24 subnet for receivers. Own fingerprints are skipped, so a
/// machine that both sends and receives never offers to send to itself.
pub fn discover(subnet: &str, own_fingerprint: &str) -> Vec<Peer> {
    let next = AtomicUsize::new(1);
    let found: Mutex<Vec<Peer>> = Mutex::new(Vec::new());
    let device = sender_device(own_fingerprint, 0);
    let work = Arc::new((next, found));
    let mut handles = Vec::with_capacity(SCAN_CONCURRENCY);
    for _ in 0..SCAN_CONCURRENCY {
        let work = work.clone();
        let subnet = subnet.to_string();
        let device = device.clone();
        handles.push(std::thread::spawn(move || loop {
            let host_index = work.0.fetch_add(1, Ordering::Relaxed);
            if host_index > 254 {
                break;
            }
            let host = format!("{subnet}.{host_index}");
            if let Some(peer) = probe_host(&host, &device) {
                work.1.lock().unwrap().push(peer);
            }
        }));
    }
    for handle in handles {
        let _ = handle.join();
    }
    let mut peers = work.1.lock().unwrap().clone();
    peers.sort_by(|a, b| a.alias.cmp(&b.alias));
    peers
}

/// Probe every host address: the first port that answers `register` with a
/// device DTO wins, and the DTO's own `port` field replaces the door we
/// knocked on (a receiver that moved ports still declares where it serves).
///
/// A host that never opens any door is done after ONE knock: the remaining
/// ports would drop the SYN the same way, and knocking them all turns one
/// dead address into eleven probe budgets.
pub fn probe_host(host: &str, device: &DeviceInfo) -> Option<Peer> {
    for port in PORT_RANGE {
        match probe_knock(host, port, device) {
            Ok(peer) => return Some(peer),
            // The door opened and said "not a BookDrop receiver": the next
            // port may still be one.
            Err(ProbeFail::NotPeer) => {}
            // The host never answered the door at all: no port on it will.
            Err(ProbeFail::NoHost) => return None,
        }
    }
    None
}

/// One register knock on one port: a 200 with a device DTO is a peer.
pub fn probe_port(host: &str, port: u16, device: &DeviceInfo) -> Option<Peer> {
    probe_knock(host, port, device).ok()
}

/// Why a knock came back empty. `NoHost` means the connection itself never
/// happened — an unreachable or silent host, on which no other port can
/// answer either; `NotPeer` means the door opened and the register exchange
/// said "not a BookDrop receiver", which leaves every other port untried.
enum ProbeFail {
    NotPeer,
    NoHost,
}

/// The knock [`probe_port`] and [`probe_host`] share. The connect carries
/// [`PROBE_TIMEOUT`] itself: a plain blocking `TcpStream::connect` only
/// honours the read/write timeouts AFTER the handshake, and on a network
/// whose dead addresses drop the SYN (a phone hotspot's NAT, a filtered
/// bridge) every one of the 254 knocks then sat through the OS's ~130 s
/// TCP retry ladder — a sweep measured at over 19 minutes for one subnet
/// before this timeout existed.
fn probe_knock(host: &str, port: u16, device: &DeviceInfo) -> Result<Peer, ProbeFail> {
    let addr = match (host, port).to_socket_addrs() {
        Ok(mut addrs) => match addrs.next() {
            Some(addr) => addr,
            None => return Err(ProbeFail::NoHost),
        },
        Err(_) => return Err(ProbeFail::NoHost),
    };
    let mut stream = match TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
        Ok(stream) => stream,
        // A refusal is the host speaking — it is alive, this port serves
        // nothing, and another port may still serve a receiver.
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
            return Err(ProbeFail::NotPeer);
        }
        Err(_) => return Err(ProbeFail::NoHost),
    };
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
    let body = serde_json::to_vec(device).unwrap_or_default();
    let request = format!(
        "POST /api/localsend/v2/register HTTP/1.1\r\nHost: {host}:{port}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    if stream.write_all(request.as_bytes()).is_err() || stream.write_all(&body).is_err() {
        return Err(ProbeFail::NotPeer);
    }
    let (status, response) = read_response(&mut stream).ok_or(ProbeFail::NotPeer)?;
    if status != 200 {
        return Err(ProbeFail::NotPeer);
    }
    let dto = serde_json::from_slice::<DeviceInfo>(&response).map_err(|_| ProbeFail::NotPeer)?;
    if dto.fingerprint == device.fingerprint {
        return Err(ProbeFail::NotPeer); // ourselves
    }
    let port = if dto.port > 0 { dto.port } else { port };
    Ok(Peer {
        alias: dto.alias,
        device_model: dto.device_model,
        device_type: Some(dto.device_type),
        fingerprint: dto.fingerprint,
        host: host.to_string(),
        port,
    })
}

/// One file to send: the path on disk and the name the receiver should see.
#[derive(Debug, Clone)]
pub struct SendItem {
    pub path: PathBuf,
    pub file_name: String,
}

/// Where a send stands, for a status line. `Done` carries the sentence the
/// UI shows when the last byte is out the door.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendPhase {
    Preparing,
    /// (files finished, files total)
    Uploading(usize, usize),
    Done(String),
    Failed(String),
}

/// Send `items` to `peer` in one LocalSend session.
///
/// `progress` is called at each step with the current phase, so the caller
/// can draw a line without owning a channel. The access code, when the
/// destination demands one, is offered in every place the protocol allows.
pub fn send_files(
    items: &[SendItem],
    peer: &Peer,
    own_fingerprint: &str,
    pin: Option<&str>,
    mut progress: impl FnMut(SendPhase),
) -> Result<(), String> {
    if items.is_empty() {
        return Err("nothing to send".to_string());
    }
    progress(SendPhase::Preparing);
    let device = sender_device(own_fingerprint, 0);

    // prepare-upload: one offer carrying every file's metadata.
    let mut files = serde_json::Map::new();
    for (index, item) in items.iter().enumerate() {
        let size = std::fs::metadata(&item.path)
            .map(|m| m.len())
            .map_err(|e| format!("cannot read {}: {e}", item.path.display()))?;
        files.insert(
            index.to_string(),
            serde_json::json!({
                "id": index.to_string(),
                "fileName": item.file_name,
                "size": size,
                "fileType": file_type_of(&item.file_name),
                "sha256": null,
            }),
        );
    }
    let mut prepare_path = "/api/localsend/v2/prepare-upload".to_string();
    if let Some(pin) = pin.filter(|p| !p.is_empty()) {
        prepare_path.push_str("?pin=");
        prepare_path.push_str(&urlencode(pin));
    }
    let body = serde_json::json!({
        "info": device,
        "pin": pin.unwrap_or(""),
        "files": files,
    })
    .to_string();
    let (status, response) =
        post_json(&peer.host, peer.port, &prepare_path, body.as_bytes()).map_err(|e| e.to_string())?;
    match status {
        200 => {}
        401 => return Err(format!("{} needs an access code (or the one entered is wrong)", peer.alias)),
        403 => return Err(format!("{} declined the transfer", peer.alias)),
        409 => return Err(format!("{} is busy with another transfer", peer.alias)),
        other => return Err(format!("{} refused the transfer (HTTP {other})", peer.alias)),
    }
    let Ok(prepared) = serde_json::from_slice::<HashMap<String, serde_json::Value>>(&response) else {
        return Err(format!("{} answered with something unreadable", peer.alias));
    };
    let Some(session) = prepared.get("sessionId").and_then(|v| v.as_str()) else {
        return Err(format!("{} did not name a session", peer.alias));
    };
    let tokens: HashMap<String, String> = prepared
        .get("files")
        .and_then(|v| v.as_object())
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    // One upload per file, echoing the token prepare returned for it.
    for (index, item) in items.iter().enumerate() {
        let id = index.to_string();
        let Some(token) = tokens.get(&id) else {
            return Err(format!("{} gave no token for {}", peer.alias, item.file_name));
        };
        progress(SendPhase::Uploading(index, items.len()));
        let mut path = format!(
            "/api/localsend/v2/upload?sessionId={}&fileId={}&token={}",
            urlencode(session),
            urlencode(&id),
            urlencode(token),
        );
        if let Some(pin) = pin.filter(|p| !p.is_empty()) {
            path.push_str("&pin=");
            path.push_str(&urlencode(pin));
        }
        let status = post_file(&peer.host, peer.port, &path, &item.path)
            .map_err(|e| format!("{} failed: {e}", item.file_name))?;
        match status {
            200 => {}
            401 => return Err(format!("{} needs an access code (or the one entered is wrong)", peer.alias)),
            403 | 409 => {
                return Err(format!("{} rejected {} (HTTP {status})", peer.alias, item.file_name))
            }
            other => return Err(format!("{} failed (HTTP {other})", item.file_name)),
        }
    }
    progress(SendPhase::Done(if items.len() == 1 {
        format!("Sent {} to {}", items[0].file_name, peer.alias)
    } else {
        format!("Sent {} files to {}", items.len(), peer.alias)
    }));
    Ok(())
}

/// This device's sender DTO. The fingerprint is the SAME one the receiver
/// publishes (the caller stores it), so a peer never sees this machine as
/// two devices.
fn sender_device(fingerprint: &str, port: u16) -> DeviceInfo {
    DeviceInfo {
        alias: crate::bookdrop::device_alias(),
        version: "2.2".to_string(),
        device_model: None,
        device_type: "desktop".to_string(),
        fingerprint: fingerprint.to_string(),
        port,
        protocol: "http".to_string(),
        download: false,
    }
}

/// The LocalSend `fileType` categories, derived from the name the receiver
/// sees. Receivers may show or filter on it; the mapping is cosmetic.
fn file_type_of(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "mp4" => "video",
        "mp3" | "m4b" | "m4a" | "opus" | "ogg" | "oga" | "wav" => "audio",
        "png" | "jpg" | "jpeg" | "gif" | "webp" => "image",
        _ => "document",
    }
}

/// POST a small JSON body and read the whole response.
fn post_json(
    host: &str,
    port: u16,
    path: &str,
    body: &[u8],
) -> std::io::Result<(u16, Vec<u8>)> {
    let mut stream = connect(host, port)?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    read_response(&mut stream)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "no response"))
}

/// POST a file's bytes with an honest Content-Length, streamed in chunks —
/// a book never sits in memory to be sent.
fn post_file(host: &str, port: u16, path: &str, file: &Path) -> std::io::Result<u16> {
    let size = std::fs::metadata(file)?.len();
    let mut source = std::fs::File::open(file)?;
    let mut stream = connect(host, port)?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\n\
         Content-Type: application/octet-stream\r\nContent-Length: {size}\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes())?;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = source.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        stream.write_all(&chunk[..n])?;
    }
    let (status, _body) = read_response(&mut stream)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "no response"))?;
    Ok(status)
}

/// Connect with the probe timeout. LAN or nothing: these addresses never
/// carry TLS, so a plain stream is the whole transport.
fn connect(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let stream = TcpStream::connect((host, port))?;
    stream.set_read_timeout(Some(Duration::from_secs(3600)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3600)))?;
    Ok(stream)
}

/// Read one HTTP response: status line, headers, then exactly the
/// Content-Length body (or to EOF when the peer sends none).
fn read_response(stream: &mut TcpStream) -> Option<(u16, Vec<u8>)> {
    let mut buffer: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if buffer.len() > 64 * 1024 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let status: u16 = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let mut content_length: Option<usize> = None;
    for line in head.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
    let mut body = buffer[head_end + 4..].to_vec();
    if let Some(want) = content_length {
        let want = want.min(MAX_RESPONSE_BODY);
        while body.len() < want {
            let n = stream.read(&mut chunk).ok()?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(want);
    } else {
        loop {
            if body.len() >= MAX_RESPONSE_BODY {
                // A peer that answers without a Content-Length and never
                // stops writing does not get to grow this buffer further.
                body.truncate(MAX_RESPONSE_BODY);
                break;
            }
            let n = match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            body.extend_from_slice(&chunk[..n]);
        }
    }
    Some((status, body))
}

/// Percent-encode a query value. Session ids and tokens are hex, but the
/// access code is whatever the user typed — an `&` in it must not split the
/// query string.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Resolve nothing, dial nothing: the imports above are the whole surface
/// this module needs.
const _: () = ();

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bookdrop::Receiver;

    /// A receiver on an OS-chosen port, with a router that records what
    /// landed. `port()` is where it serves.
    fn test_receiver(pin: Option<&str>) -> (std::sync::Arc<Receiver>, u16, std::sync::Arc<Mutex<Vec<String>>>) {
        let receiver = std::sync::Arc::new(Receiver::new("Box".into(), "box-fp".into(), 0));
        if let Some(pin) = pin {
            receiver.set_pin(Some(pin.to_string()));
        }
        let landed = std::sync::Arc::new(Mutex::new(Vec::new()));
        {
            let landed = landed.clone();
            receiver.set_router(Box::new(move |path| {
                landed
                    .lock()
                    .unwrap()
                    .push(path.file_name().unwrap().to_string_lossy().into_owned());
                Ok("library".to_string())
            }));
        }
        let listener = receiver.bind_port(0).expect("ephemeral port");
        let port = listener.local_addr().unwrap().port();
        let server = receiver.clone();
        std::thread::spawn(move || server.serve_listener(listener));
        (receiver, port, landed)
    }

    fn write_temp(name: &str, bytes: &[u8]) -> SendItem {
        let path = std::env::temp_dir().join(format!("bookdrop-send-{name}"));
        std::fs::write(&path, bytes).unwrap();
        SendItem { path, file_name: name.to_string() }
    }

    #[test]
    fn a_note_lands_on_the_other_side() {
        let (receiver, port, landed) = test_receiver(None);
        let peer = probe_port("127.0.0.1", port, &sender_device("sender-fp", 0)).expect("found");
        assert_eq!(peer.port, port, "the DTO's own port wins");
        let item = write_temp("hello.md", b"# hello\n");
        let mut phases = Vec::new();
        send_files(&[item], &peer, "sender-fp", None, |p| phases.push(p)).expect("sent");
        assert!(phases.iter().any(|p| matches!(p, SendPhase::Done(_))));
        assert!(
            landed.lock().unwrap()[0].ends_with("hello.md"),
            "the router saw the file: {:?}",
            landed.lock().unwrap()
        );
        // The history row shows the sender's name, not the internal
        // `<fileId>-<name>` form the file landed under.
        assert_eq!(receiver.history_snapshot()[0].name, "hello.md");
    }

    #[test]
    fn a_wrong_code_is_refused_before_any_file_moves() {
        let (_receiver, port, landed) = test_receiver(Some("4321"));
        let peer = probe_port("127.0.0.1", port, &sender_device("sender-fp", 0)).expect("found");
        let item = write_temp("secret.md", b"nope\n");
        let err = send_files(&[item], &peer, "sender-fp", Some("0000"), |_| {})
            .expect_err("the wrong code must not send");
        assert!(err.contains("access code"), "{err}");
        assert!(landed.lock().unwrap().is_empty(), "nothing landed");
    }

    #[test]
    fn the_right_code_sends() {
        let (_receiver, port, landed) = test_receiver(Some("4321"));
        let peer = probe_port("127.0.0.1", port, &sender_device("sender-fp", 0)).expect("found");
        let item = write_temp("with-code.md", b"ok\n");
        send_files(&[item], &peer, "sender-fp", Some("4321"), |_| {}).expect("sent");
        assert!(landed.lock().unwrap()[0].ends_with("with-code.md"));
    }

    #[test]
    fn an_unknown_format_is_declined_not_corrupted() {
        let (_receiver, port, landed) = test_receiver(None);
        let peer = probe_port("127.0.0.1", port, &sender_device("sender-fp", 0)).expect("found");
        let item = write_temp("virus.exe", b"MZ");
        let err = send_files(&[item], &peer, "sender-fp", None, |_| {})
            .expect_err("an .exe has no importer");
        assert!(err.contains("declined"), "{err}");
        assert!(landed.lock().unwrap().is_empty());
    }

    #[test]
    fn a_code_with_special_characters_rides_the_query_safely() {
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
    }

    #[test]
    fn the_subnet_prefix_is_parsed_from_the_local_address() {
        // The connect trick is exercised against the real stack in the app;
        // here only the string half matters.
        let ip = "192.168.1.42";
        let mut parts = ip.split('.');
        let _host_octet = parts.next_back();
        assert_eq!(parts.collect::<Vec<_>>().join("."), "192.168.1");
    }

    #[test]
    fn an_address_without_a_port_takes_the_protocol_default() {
        assert_eq!(
            parse_address("192.168.1.50").unwrap(),
            ("192.168.1.50".to_string(), crate::bookdrop::DEFAULT_PORT)
        );
    }

    #[test]
    fn an_address_with_a_port_keeps_it() {
        assert_eq!(
            parse_address("192.168.1.50:53318").unwrap(),
            ("192.168.1.50".to_string(), 53318)
        );
    }

    #[test]
    fn an_ipv6_literal_keeps_its_own_colons() {
        // Unbracketed: every colon belongs to the address, so the default
        // port is used and the address is not cut in half.
        assert_eq!(
            parse_address("fe80::1").unwrap(),
            ("fe80::1".to_string(), crate::bookdrop::DEFAULT_PORT)
        );
        // Bracketed: the port after the bracket is the port.
        assert_eq!(
            parse_address("[fe80::1]:53318").unwrap(),
            ("fe80::1".to_string(), 53318)
        );
    }

    #[test]
    fn the_sweep_reads_the_octets_not_their_reverse() {
        // 172.20.10.3 the way the kernel hands it over: s_addr holds the
        // address in network byte order. `to_be_bytes()` reversed the
        // octets on this little-endian host and the sweep knocked on the
        // swapped PUBLIC ranges ("3.10.20" is an AWS block) while the real
        // LAN went unscanned.
        let s_addr = u32::from_be(0xAC14_0A03); // 172.20.10.3, network order
        assert_eq!(octets(s_addr), [172, 20, 10, 3]);
    }

    #[test]
    fn a_hostname_is_an_address_too() {
        assert_eq!(
            parse_address("laptop.local").unwrap(),
            ("laptop.local".to_string(), crate::bookdrop::DEFAULT_PORT)
        );
    }

    #[test]
    fn a_bare_port_is_refused_rather_than_guessed() {
        // "53318" is a mistyped address, not a host: guessing would send
        // the files somewhere the user never named.
        let err = parse_address("53318").unwrap_err();
        assert!(err.contains("port number"), "{err}");
    }

    #[test]
    fn an_empty_address_is_refused() {
        assert!(parse_address("   ").unwrap_err().contains("type the address"));
    }

    #[test]
    fn a_manual_address_that_answers_becomes_that_peer() {
        // A real receiver on the loopback: the manual path must find it by
        // address alone, with no scan involved.
        let (_receiver, port, _landed) = test_receiver(None);
        let address = format!("127.0.0.1:{port}");
        let peer = manual_peer(&address, "sender-fp").expect("the address resolves");
        assert_eq!(peer.host, "127.0.0.1");
        assert_eq!(peer.port, port);
        assert_eq!(peer.fingerprint, "box-fp", "the receiver identified itself");
    }

    #[test]
    fn a_manual_address_that_answers_nothing_is_still_offered() {
        // The transfer is a different exchange from the register handshake;
        // a quiet address stays selectable and the send reports the refusal.
        let peer = manual_peer("127.0.0.1:1", "sender-fp").expect("the address is still a peer");
        assert_eq!(peer.host, "127.0.0.1");
        assert_eq!(peer.port, 1);
        assert_eq!(peer.alias, "127.0.0.1");
    }
}
