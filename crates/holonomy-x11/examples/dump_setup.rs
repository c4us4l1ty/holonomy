//! Is the handshake's declared length the whole story?
//!
//! After the setup reply this host sends an extra 8 bytes: `ff 00 00 00 00 00 00 00`. A client that
//! reads only the declared length leaves them in the socket, and then every reply it parses starts 8
//! bytes into a packet -- which is how this crate's first live run hung. This measures the total,
//! declared against sent.
//!
//! ```text
//!   cargo run -p holonomy-x11 --example dump_setup
//! ```

use std::io::{Read, Write};

fn main() {
    let path = "/tmp/.X11-unix/X0";
    let mut stream = match std::os::unix::net::UnixStream::connect(path) {
        Ok(s) => s,
        Err(_) => {
            use std::os::linux::net::SocketAddrExt;
            let addr = std::os::unix::net::SocketAddr::from_abstract_name(path.as_bytes())
                .expect("abstract name");
            std::os::unix::net::UnixStream::connect_addr(&addr).expect("abstract connect")
        }
    };
    let cookie = {
        let bytes = std::fs::read(std::env::var("XAUTHORITY").expect("XAUTHORITY")).expect("auth");
        let mut at = 2usize;
        let mut data = Vec::new();
        for field in 0..4 {
            let n = u16::from_be_bytes([bytes[at], bytes[at + 1]]) as usize;
            at += 2;
            if field == 3 {
                data = bytes[at..at + n].to_vec();
            }
            at += n;
        }
        data
    };
    let mut req = Vec::new();
    req.extend_from_slice(&[0x6C, 0]);
    req.extend_from_slice(&11u16.to_le_bytes());
    req.extend_from_slice(&0u16.to_le_bytes());
    req.extend_from_slice(&18u16.to_le_bytes());
    req.extend_from_slice(&(cookie.len() as u16).to_le_bytes());
    req.extend_from_slice(&[0, 0]);
    req.extend_from_slice(b"MIT-MAGIC-COOKIE-1");
    req.extend_from_slice(&[0, 0]);
    req.extend_from_slice(&cookie);
    stream.write_all(&req).expect("write");

    let mut head = [0u8; 8];
    read_all(&mut stream, &mut head);
    let extra = u16::from_le_bytes([head[6], head[7]]) as usize;
    println!(
        "declared additional data: {extra} words = {} bytes",
        extra * 4
    );
    let declared_total = 8 + extra * 4;
    println!("declared total reply: {declared_total} bytes");

    let mut got = 8usize;
    let mut rest = vec![0u8; extra * 4 - 8];
    read_all(&mut stream, &mut rest);
    got += rest.len();
    println!("read {got} bytes; the socket now holds:");

    // Everything else the server sends, with no request outstanding.
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(300)))
        .expect("timeout");
    let mut tail = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => tail.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    println!("  {} further bytes", tail.len());
    let hex: Vec<String> = tail.iter().map(|b| format!("{b:02x}")).collect();
    println!("  {}", hex.join(" "));

    // Where does the extra data end, according to the structures inside it? If the tail is the tail
    // of the last visual, then the declared length was short by exactly this much.
    let vendor_len = u16::from_le_bytes([rest[0x10], rest[0x11]]) as usize;
    let nformats = rest[0x15] as usize;
    let nscreens = rest[0x14] as usize;
    let mut at = 0x20 + vendor_len;
    at += (4 - vendor_len % 4) % 4;
    at += nformats * 8;
    println!("vendor at 0x20 ({vendor_len} bytes), {nformats} formats, {nscreens} screen(s)");
    let mut screen_total = 0usize;
    for _ in 0..nscreens {
        let ndepths = rest[at + 39] as usize;
        let mut p = at + 40;
        let mut visuals = 0usize;
        for _ in 0..ndepths {
            let nvisuals = u16::from_le_bytes([rest[p + 2], rest[p + 3]]) as usize;
            visuals += nvisuals;
            p += 8 + nvisuals * 24;
        }
        println!("  screen at {at:#x}: {ndepths} depths, {visuals} visuals, ends at {p:#x}");
        screen_total = p - at;
        at = p;
    }
    println!("structures account for {at} bytes of additional data");
    println!(
        "declared {} bytes, structures {} bytes, tail {} bytes",
        rest.len(),
        at,
        tail.len()
    );
    assert!(screen_total > 40, "a SCREEN is 40 bytes plus its depths");
}

fn read_all(stream: &mut std::os::unix::net::UnixStream, buf: &mut [u8]) {
    let mut at = 0;
    while at < buf.len() {
        at += stream.read(&mut buf[at..]).expect("read");
    }
}
