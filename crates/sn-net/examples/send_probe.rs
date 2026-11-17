//! Probe one BookDrop peer and send it a file — the real send path
//! (`probe_port` → `send_files`), print every phase.
//!
//! ```text
//! cargo run -p sn-net --example send_probe -- 127.0.0.1 53317 /tmp/note.txt
//! ```
//!
//! Useful for verifying a receiver (the app's `--receive`, or a phone on
//! `IP 53317`) without driving the GUI's file chooser.

use sn_net::bookdrop::DeviceInfo;
use sn_net::sender::{probe_port, send_files, SendItem, SendPhase};

fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_string());
    let port: u16 = args.next().map(|p| p.parse().expect("port")).unwrap_or(53317);
    let path = args.next().expect("usage: send_probe [HOST] [PORT] FILE");

    let me = DeviceInfo {
        alias: "send-probe".to_string(),
        version: "2.2".to_string(),
        device_model: None,
        device_type: "desktop".to_string(),
        fingerprint: "sn-net-send-probe".to_string(),
        port: 0,
        protocol: "http".to_string(),
        download: false,
    };

    let Some(peer) = probe_port(&host, port, &me) else {
        eprintln!("no peer answered at {host}:{port}");
        std::process::exit(1);
    };
    println!("peer: {}", peer.label());

    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    let items = vec![SendItem { path: path.into(), file_name: name }];
    let result = send_files(&items, &peer, &me.fingerprint, None, |phase| match phase {
        SendPhase::Preparing => println!("preparing…"),
        SendPhase::Uploading(f, t) => println!("uploading {f}/{t}"),
        SendPhase::Done(summary) => println!("done: {summary}"),
        SendPhase::Failed(why) => println!("failed: {why}"),
    });
    match result {
        Ok(()) => println!("send ok"),
        Err(e) => {
            eprintln!("send failed: {e}");
            std::process::exit(1);
        }
    }
}
