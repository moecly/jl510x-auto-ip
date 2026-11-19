use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::net::Ipv4Addr;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::Parser;
use pnet::datalink::{self, Channel, Config, MacAddr, NetworkInterface};

const ETHERTYPE_LOOP: u16 = 0x9988;
const ETHERTYPE_REPLY: u16 = 0x9989;
const LOOP_RESERVED: u16 = 0x0001;
const REPLY_PAYLOAD_LEN: usize = 42;
const REPLY_CRC_LEN: usize = 36;
const REPLY_CRC_OFFSET: usize = 36;
const FRAME_LEN: usize = 60;
const PAYLOAD_LEN: usize = 42;
const PAYLOAD_CRC_LEN: usize = 30;
const PAYLOAD_CRC_OFFSET: usize = 30;
const ETH_HEADER_LEN: usize = 14;
const LOOP_HEADER_LEN: usize = 4;
const MAX_BOARD_INDEX: u32 = 254;

#[derive(Parser, Debug)]
#[command(
    name = "auto-ip",
    version,
    about = "Send JL510x loop-detection frames to assign IPs from the decremented TTL"
)]
struct Args {
    /// Network interface to send on (e.g. eth0)
    #[arg(short, long)]
    interface: Option<String>,

    /// Source MAC address (defaults to the interface MAC)
    #[arg(long)]
    source_mac: Option<String>,

    /// TTL written into the loop-detection frame
    #[arg(short, long, default_value_t = 200)]
    ttl: u16,

    /// 0: the switch decrements before the CPU copy; 1: it decrements after
    #[arg(long, default_value_t = 0)]
    index_offset: u8,

    /// IP address of board 1
    #[arg(long, default_value = "192.168.1.10")]
    base_ip: Ipv4Addr,

    /// Netmask handed to the boards
    #[arg(long, default_value = "255.255.255.0")]
    netmask: Ipv4Addr,

    /// Gateway handed to the boards
    #[arg(long, default_value = "192.168.1.1")]
    gateway: Ipv4Addr,

    /// Number of request frames to send (0 = forever); one frame reaches the whole chain
    #[arg(long, default_value_t = 1)]
    count: u32,

    /// Delay between frames in milliseconds
    #[arg(long, default_value_t = 1000)]
    interval_ms: u64,

    /// How long to collect board replies after the last frame (0 = skip)
    #[arg(long, default_value_t = 1500)]
    reply_timeout_ms: u64,

    /// Number of mapping rows to print (does not limit frames or target boards)
    #[arg(long, default_value_t = 16)]
    map_count: u32,

    /// Print the frame without sending it
    #[arg(long)]
    dry_run: bool,

    /// List interfaces and exit
    #[arg(long)]
    list: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let interfaces = datalink::interfaces();

    if args.list {
        for iface in &interfaces {
            let mac = iface
                .mac
                .map(|m| m.to_string())
                .unwrap_or_else(|| "-".to_string());
            println!("{:<16} {:<17} up={}", iface.name, mac, iface.is_up());
        }
        return Ok(());
    }

    if args.ttl < 2 {
        bail!("ttl must be >= 2: the switch drops frames whose TTL is 1");
    }
    if args.index_offset > 1 {
        bail!("index-offset must be 0 or 1");
    }
    if args.base_ip.is_unspecified() || args.base_ip.is_broadcast() {
        bail!("base-ip must be a usable unicast address");
    }
    if args.netmask.is_unspecified() {
        bail!("netmask must not be 0.0.0.0");
    }
    if args.map_count > MAX_BOARD_INDEX {
        bail!("map-count must be <= {MAX_BOARD_INDEX}: the firmware ignores larger indexes");
    }

    let iface = select_interface(&interfaces, args.interface.as_deref())?;
    let source_mac = match args.source_mac.as_deref() {
        Some(text) => parse_mac(text)?,
        None => iface
            .mac
            .context("interface has no MAC address; pass --source-mac")?,
    };

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u32)
        .unwrap_or(0);

    let frame = build_frame(
        source_mac,
        args.ttl,
        args.index_offset,
        args.base_ip,
        args.netmask,
        args.gateway,
        60_000,
        nonce,
    );

    println!("interface: {} ({})", iface.name, source_mac);
    println!(
        "frame: {} bytes, ethertype 0x{:04x}, ttl {}, index-offset {}",
        frame.len(),
        ETHERTYPE_LOOP,
        args.ttl,
        args.index_offset
    );
    println!(
        "base {} netmask {} gateway {}",
        args.base_ip, args.netmask, args.gateway
    );
    print_mapping(args.base_ip, args.map_count);

    if args.dry_run {
        print_hex(&frame);
        return Ok(());
    }

    let channel_config = Config {
        read_timeout: Some(Duration::from_millis(100)),
        ..Default::default()
    };
    let (mut tx, mut rx) = match datalink::channel(iface, channel_config)
        .with_context(|| format!("failed to open {}", iface.name))?
    {
        Channel::Ethernet(tx, rx) => (tx, rx),
        _ => bail!("unsupported datalink channel"),
    };

    let (reply_tx, reply_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let receiver = if args.count != 0 && args.reply_timeout_ms != 0 {
        let stop = stop.clone();
        Some(thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match rx.next() {
                    Ok(frame) => {
                        if let Some(reply) = parse_reply(frame, nonce) {
                            if reply_tx.send(reply).is_err() {
                                break;
                            }
                        }
                    }
                    Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(_) => break,
                }
            }
        }))
    } else {
        None
    };

    let mut sent = 0u32;
    loop {
        match tx.send_to(&frame, None) {
            Some(Ok(())) => {}
            Some(Err(err)) => bail!("send failed: {err}"),
            None => bail!("send failed: channel closed"),
        }

        sent += 1;
        println!("sent {sent}");

        if args.count != 0 && sent >= args.count {
            break;
        }
        thread::sleep(Duration::from_millis(args.interval_ms));
    }

    if args.count == 0 {
        println!("count=0: reply collection is skipped; use a finite --count to verify boards");
    } else if let Some(handle) = receiver {
        let deadline = Instant::now() + Duration::from_millis(args.reply_timeout_ms);
        let mut replies = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match reply_rx.recv_timeout(remaining) {
                Ok(reply) => replies.push(reply),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();
        print_replies(&replies);
    }

    Ok(())
}

#[derive(Debug)]
struct Reply {
    mac: [u8; 6],
    status: u8,
    reason: u8,
    index: u16,
    ttl: u16,
    old_ip: Ipv4Addr,
    new_ip: Ipv4Addr,
}

fn parse_reply(frame: &[u8], nonce: u32) -> Option<Reply> {
    if frame.len() < 14 {
        return None;
    }

    let mut off = 12usize;
    let mut ethertype = u16::from_be_bytes([frame[off], frame[off + 1]]);
    while (ethertype == 0x9999 || ethertype == 0x8100) && frame.len() >= off + 8 {
        off += if ethertype == 0x9999 { 6 } else { 4 };
        ethertype = u16::from_be_bytes([frame[off], frame[off + 1]]);
    }

    if ethertype != ETHERTYPE_REPLY || frame.len() < off + 6 + REPLY_PAYLOAD_LEN {
        return None;
    }

    let payload = &frame[off + 6..off + 6 + REPLY_PAYLOAD_LEN];
    if &payload[0..4] != b"AIP2" || payload[4] != 1 {
        return None;
    }
    if u16::from_be_bytes([payload[REPLY_CRC_OFFSET], payload[REPLY_CRC_OFFSET + 1]])
        != crc16_ccitt(&payload[..REPLY_CRC_LEN])
    {
        return None;
    }
    if u32::from_be_bytes([payload[28], payload[29], payload[30], payload[31]]) != nonce {
        return None;
    }

    Some(Reply {
        mac: [
            frame[6], frame[7], frame[8], frame[9], frame[10], frame[11],
        ],
        status: payload[5],
        reason: payload[6],
        index: u16::from_be_bytes([payload[8], payload[9]]),
        ttl: u16::from_be_bytes([payload[10], payload[11]]),
        old_ip: Ipv4Addr::from(u32::from_be_bytes([
            payload[12], payload[13], payload[14], payload[15],
        ])),
        new_ip: Ipv4Addr::from(u32::from_be_bytes([
            payload[16], payload[17], payload[18], payload[19],
        ])),
    })
}

fn print_replies(replies: &[Reply]) {
    let mut boards: HashMap<[u8; 6], &Reply> = HashMap::new();
    for reply in replies {
        let entry = boards.entry(reply.mac).or_insert(reply);
        if entry.status == 4 || reply.status != 4 {
            *entry = reply;
        }
    }

    println!(
        "replies: {} frame(s), {} board(s)",
        replies.len(),
        boards.len()
    );
    if boards.is_empty() {
        println!("  no board replied; check link, interface and that frames reach the chain");
        return;
    }

    let mut rows: Vec<&Reply> = boards.into_values().collect();
    rows.sort_by_key(|reply| (reply.index, reply.mac));

    println!(
        "{:<5} {:<17} {:<15} {:<15} {:<5} {:<14} {}",
        "board", "mac", "old ip", "new ip", "ttl", "status", "detail"
    );
    for reply in rows {
        println!(
            "{:<5} {:<17} {:<15} {:<15} {:<5} {:<14} {}",
            reply.index,
            format_mac(&reply.mac),
            reply.old_ip,
            reply.new_ip,
            reply.ttl,
            status_text(reply.status),
            reason_text(reply.status, reply.reason),
        );
    }
}

fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn status_text(status: u8) -> &'static str {
    match status {
        0 => "applied",
        1 => "unchanged",
        2 => "save-failed",
        3 => "rejected",
        4 => "pending-save",
        _ => "unknown",
    }
}

fn reason_text(status: u8, reason: u8) -> &'static str {
    if status == 0 {
        return "saved to config";
    }
    if status == 1 {
        return "already configured";
    }
    if status == 2 {
        return "runtime only; config write failed";
    }
    if status == 4 {
        return "runtime applied; config write queued";
    }
    match reason {
        0 => "-",
        1 => "short frame",
        2 => "bad reserved",
        3 => "bad magic/version",
        4 => "bad crc",
        5 => "bad ttl",
        6 => "index out of range",
        7 => "invalid ip",
        8 => "out of subnet",
        _ => "unknown reason",
    }
}

fn select_interface<'a>(
    interfaces: &'a [NetworkInterface],
    name: Option<&str>,
) -> Result<&'a NetworkInterface> {
    match name {
        Some(name) => interfaces
            .iter()
            .find(|iface| iface.name == name)
            .with_context(|| format!("interface {name} not found")),
        None => interfaces
            .iter()
            .find(|iface| iface.is_up() && !iface.is_loopback() && iface.mac.is_some())
            .context("no usable interface found; pass --interface"),
    }
}

fn parse_mac(text: &str) -> Result<MacAddr> {
    let octets: Vec<&str> = text.split(':').collect();
    if octets.len() != 6 {
        bail!("expected a MAC address like aa:bb:cc:dd:ee:ff");
    }

    let mut bytes = [0u8; 6];
    for (index, octet) in octets.iter().enumerate() {
        bytes[index] = u8::from_str_radix(octet, 16)
            .with_context(|| format!("invalid MAC octet {octet}"))?;
    }

    Ok(MacAddr::new(
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5],
    ))
}

fn print_mapping(base_ip: Ipv4Addr, count: u32) {
    let base = u32::from(base_ip);
    for index in 1..=count {
        match base.checked_add(index - 1) {
            Some(ip) => println!("  board {index:>2} -> {}", Ipv4Addr::from(ip)),
            None => break,
        }
    }
}

fn print_hex(frame: &[u8]) {
    for (offset, chunk) in frame.chunks(16).enumerate() {
        print!("{:04x}: ", offset * 16);
        for byte in chunk {
            print!("{byte:02x} ");
        }
        println!();
    }
}

fn build_frame(
    src: MacAddr,
    ttl: u16,
    index_offset: u8,
    base_ip: Ipv4Addr,
    netmask: Ipv4Addr,
    gateway: Ipv4Addr,
    lease_ms: u32,
    nonce: u32,
) -> [u8; FRAME_LEN] {
    let mut frame = [0u8; FRAME_LEN];

    frame[0..6].fill(0xff);
    frame[6..12].copy_from_slice(&src.octets());
    frame[12..14].copy_from_slice(&ETHERTYPE_LOOP.to_be_bytes());
    frame[14..16].copy_from_slice(&LOOP_RESERVED.to_be_bytes());
    frame[16..18].copy_from_slice(&ttl.to_be_bytes());

    let payload_start = ETH_HEADER_LEN + LOOP_HEADER_LEN;
    let payload = &mut frame[payload_start..payload_start + PAYLOAD_LEN];
    payload[0..4].copy_from_slice(b"AIP1");
    payload[4] = 1;
    payload[5] = 0;
    payload[6] = index_offset;
    payload[7] = 0;
    payload[8..10].copy_from_slice(&ttl.to_be_bytes());
    payload[10..14].copy_from_slice(&u32::from(base_ip).to_be_bytes());
    payload[14..18].copy_from_slice(&u32::from(netmask).to_be_bytes());
    payload[18..22].copy_from_slice(&u32::from(gateway).to_be_bytes());
    payload[22..26].copy_from_slice(&lease_ms.to_be_bytes());
    payload[26..30].copy_from_slice(&nonce.to_be_bytes());

    let crc = crc16_ccitt(&payload[..PAYLOAD_CRC_LEN]);
    payload[PAYLOAD_CRC_OFFSET..PAYLOAD_CRC_OFFSET + 2].copy_from_slice(&crc.to_be_bytes());

    frame
}

fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for byte in data {
        crc ^= (u16::from(*byte)) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_ccitt_known_vector() {
        assert_eq!(crc16_ccitt(b"123456789"), 0x29b1);
    }

    #[test]
    fn frame_has_expected_layout() {
        let frame = build_frame(
            MacAddr::new(0x02, 0x00, 0x00, 0x00, 0x00, 0x01),
            200,
            0,
            Ipv4Addr::new(192, 168, 1, 10),
            Ipv4Addr::new(255, 255, 255, 0),
            Ipv4Addr::new(192, 168, 1, 1),
            60_000,
            0x12345678,
        );

        assert_eq!(frame.len(), FRAME_LEN);
        assert_eq!(&frame[0..6], &[0xff; 6]);
        assert_eq!(&frame[12..14], &ETHERTYPE_LOOP.to_be_bytes());
        assert_eq!(&frame[14..16], &LOOP_RESERVED.to_be_bytes());
        assert_eq!(&frame[16..18], &200u16.to_be_bytes());
        assert_eq!(&frame[18..22], b"AIP1");

        let crc = u16::from_be_bytes([frame[48], frame[49]]);
        assert_eq!(crc, crc16_ccitt(&frame[18..48]));
    }

    #[test]
    fn reply_parser_reports_old_and_new_ip() {
        let mut frame = [0u8; 60];
        frame[0..6].copy_from_slice(&[0x02, 0, 0, 0, 0, 1]);
        frame[6..12].copy_from_slice(&[0x6e, 0x7a, 0xf8, 0xd3, 0x19, 0x6a]);
        frame[12..14].copy_from_slice(&ETHERTYPE_REPLY.to_be_bytes());

        let payload = &mut frame[18..];
        payload[0..4].copy_from_slice(b"AIP2");
        payload[4] = 1;
        payload[5] = 0;
        payload[6] = 0;
        payload[8..10].copy_from_slice(&3u16.to_be_bytes());
        payload[10..12].copy_from_slice(&198u16.to_be_bytes());
        payload[12..16].copy_from_slice(&u32::from(Ipv4Addr::new(192, 168, 3, 199)).to_be_bytes());
        payload[16..20].copy_from_slice(&u32::from(Ipv4Addr::new(192, 168, 1, 12)).to_be_bytes());
        payload[28..32].copy_from_slice(&0x12345678u32.to_be_bytes());
        let crc = crc16_ccitt(&payload[..REPLY_CRC_LEN]);
        payload[REPLY_CRC_OFFSET..REPLY_CRC_OFFSET + 2].copy_from_slice(&crc.to_be_bytes());

        let reply = parse_reply(&frame, 0x12345678).expect("valid reply");
        assert_eq!(reply.mac, [0x6e, 0x7a, 0xf8, 0xd3, 0x19, 0x6a]);
        assert_eq!(reply.index, 3);
        assert_eq!(reply.old_ip, Ipv4Addr::new(192, 168, 3, 199));
        assert_eq!(reply.new_ip, Ipv4Addr::new(192, 168, 1, 12));
        assert!(parse_reply(&frame, 0x12345679).is_none());
    }
}
