use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use pnet::datalink::{self, Channel, Config, MacAddr, NetworkInterface};

pub mod ffi;

pub const ETHERTYPE_LOOP: u16 = 0x9988;
pub const ETHERTYPE_REPLY: u16 = 0x9989;
pub const LOOP_RESERVED: u16 = 0x0001;
pub const REPLY_PAYLOAD_LEN: usize = 42;
pub const REPLY_CRC_LEN: usize = 36;
pub const REPLY_CRC_OFFSET: usize = 36;
pub const FRAME_LEN: usize = 60;
pub const PAYLOAD_LEN: usize = 42;
pub const PAYLOAD_CRC_LEN: usize = 30;
pub const PAYLOAD_CRC_OFFSET: usize = 30;
pub const ETH_HEADER_LEN: usize = 14;
pub const LOOP_HEADER_LEN: usize = 4;
pub const MAX_BOARD_INDEX: u32 = 254;

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub interface: Option<String>,
    pub source_mac: Option<String>,
    pub ttl: u16,
    pub index_offset: u8,
    pub base_ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub count: u32,
    pub interval_ms: u64,
    pub reply_timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    pub mac: [u8; 6],
    pub status: u8,
    pub reason: u8,
    pub index: u16,
    pub ttl: u16,
    pub old_ip: Ipv4Addr,
    pub new_ip: Ipv4Addr,
}

#[derive(Debug, Clone)]
pub struct RunSummary {
    pub interface: String,
    pub source_mac: MacAddr,
    pub sent: u32,
    pub replies: Vec<Reply>,
    pub boards: Vec<Reply>,
}

#[derive(Debug, Clone)]
pub enum RunEvent {
    Sent { sent: u32 },
    Reply(Reply),
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub interface: NetworkInterface,
    pub source_mac: MacAddr,
    pub frame: [u8; FRAME_LEN],
    pub nonce: u32,
}

pub fn validate_options(options: &RunOptions) -> Result<()> {
    if options.ttl < 2 {
        bail!("ttl must be >= 2: the switch drops frames whose TTL is 1");
    }
    if options.index_offset > 1 {
        bail!("index-offset must be 0 or 1");
    }
    if options.base_ip.is_unspecified() || options.base_ip.is_broadcast() {
        bail!("base-ip must be a usable unicast address");
    }
    if options.netmask.is_unspecified() {
        bail!("netmask must not be 0.0.0.0");
    }
    Ok(())
}

pub fn prepare(options: &RunOptions) -> Result<Prepared> {
    validate_options(options)?;

    let interfaces = datalink::interfaces();
    let interface = select_interface(&interfaces, options.interface.as_deref())?.clone();
    let source_mac = match options.source_mac.as_deref() {
        Some(text) => parse_mac(text)?,
        None => interface
            .mac
            .context("interface has no MAC address; pass source-mac")?,
    };

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u32)
        .unwrap_or(0);

    let frame = build_frame(
        source_mac,
        options.ttl,
        options.index_offset,
        options.base_ip,
        options.netmask,
        options.gateway,
        60_000,
        nonce,
    );

    Ok(Prepared {
        interface,
        source_mac,
        frame,
        nonce,
    })
}

pub fn run<F>(options: &RunOptions, stop: Arc<AtomicBool>, mut on_event: F) -> Result<RunSummary>
where
    F: FnMut(RunEvent),
{
    let prepared = prepare(options)?;
    let interface = prepared.interface.clone();
    let source_mac = prepared.source_mac;

    let (mut tx, mut rx) = match datalink::channel(
        &interface,
        Config {
            read_timeout: Some(Duration::from_millis(100)),
            ..Default::default()
        },
    )
    .with_context(|| format!("failed to open {}", interface.name))?
    {
        Channel::Ethernet(tx, rx) => (tx, rx),
        _ => bail!("unsupported datalink channel"),
    };

    let (reply_tx, reply_rx) = mpsc::channel();
    let receiver_stop = stop.clone();
    let receiver = if options.reply_timeout_ms != 0 {
        Some(thread::spawn(move || {
            while !receiver_stop.load(Ordering::Relaxed) {
                match rx.next() {
                    Ok(frame) => {
                        if let Some(reply) = parse_reply(frame, prepared.nonce) {
                            if reply_tx.send(reply).is_err() {
                                break;
                            }
                        }
                    }
                    Err(err)
                        if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(_) => break,
                }
            }
        }))
    } else {
        None
    };

    let mut replies = Vec::new();
    let mut sent = 0u32;
    let send_result: Result<()> = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }

        match tx.send_to(&prepared.frame, None) {
            Some(Ok(())) => {}
            Some(Err(err)) => break Err(anyhow::anyhow!("send failed: {err}")),
            None => break Err(anyhow::anyhow!("send failed: channel closed")),
        }

        sent += 1;
        on_event(RunEvent::Sent { sent });

        if options.count != 0 && sent >= options.count {
            break Ok(());
        }

        let deadline = Instant::now() + Duration::from_millis(options.interval_ms);
        drain_replies(&reply_rx, deadline, &stop, &mut replies, &mut on_event);
    };

    if send_result.is_ok() && options.count != 0 && options.reply_timeout_ms != 0 {
        let deadline = Instant::now() + Duration::from_millis(options.reply_timeout_ms);
        drain_replies(&reply_rx, deadline, &stop, &mut replies, &mut on_event);
    }

    stop.store(true, Ordering::Relaxed);
    if let Some(handle) = receiver {
        #[cfg(target_os = "windows")]
        {
            // WinPcap's receiver can block in PacketReceivePacket despite read_timeout.
            drop(handle);
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = handle.join();
        }
    }

    send_result?;

    let boards = dedupe_replies(&replies);
    Ok(RunSummary {
        interface: interface.name,
        source_mac,
        sent,
        replies,
        boards,
    })
}

fn drain_replies<F>(
    reply_rx: &Receiver<Reply>,
    deadline: Instant,
    stop: &AtomicBool,
    replies: &mut Vec<Reply>,
    on_event: &mut F,
) where
    F: FnMut(RunEvent),
{
    while !stop.load(Ordering::Relaxed) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match reply_rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(reply) => {
                on_event(RunEvent::Reply(reply));
                replies.push(reply);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

pub fn dedupe_replies(replies: &[Reply]) -> Vec<Reply> {
    let mut boards: HashMap<[u8; 6], Reply> = HashMap::new();
    for reply in replies {
        let entry = boards.entry(reply.mac).or_insert(*reply);
        // Keep the first terminal reply; repeated requests report unchanged afterwards.
        if entry.status == 4 && reply.status != 4 {
            *entry = *reply;
        }
    }

    let mut rows: Vec<Reply> = boards.into_values().collect();
    rows.sort_by_key(|reply| (reply.index, reply.mac));
    rows
}

pub fn mapping_rows(base_ip: Ipv4Addr, count: u32) -> Vec<(u32, Ipv4Addr)> {
    let base = u32::from(base_ip);
    let mut rows = Vec::new();
    for index in 1..=count {
        match base.checked_add(index - 1) {
            Some(ip) => rows.push((index, Ipv4Addr::from(ip))),
            None => break,
        }
    }
    rows
}

pub fn select_interface<'a>(
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

pub fn parse_mac(text: &str) -> Result<MacAddr> {
    let octets: Vec<&str> = text.split(':').collect();
    if octets.len() != 6 {
        bail!("expected a MAC address like aa:bb:cc:dd:ee:ff");
    }

    let mut bytes = [0u8; 6];
    for (index, octet) in octets.iter().enumerate() {
        bytes[index] =
            u8::from_str_radix(octet, 16).with_context(|| format!("invalid MAC octet {octet}"))?;
    }

    Ok(MacAddr::new(
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5],
    ))
}

pub fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

pub fn status_text(status: u8) -> &'static str {
    match status {
        0 => "applied",
        1 => "unchanged",
        2 => "save-failed",
        3 => "rejected",
        4 => "pending-save",
        _ => "unknown",
    }
}

pub fn reason_text(status: u8, reason: u8) -> &'static str {
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

pub fn format_hex(frame: &[u8]) -> String {
    let mut out = String::new();
    for (offset, chunk) in frame.chunks(16).enumerate() {
        out.push_str(&format!("{:04x}: ", offset * 16));
        for byte in chunk {
            out.push_str(&format!("{byte:02x} "));
        }
        out.push('\n');
    }
    out
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
        mac: [frame[6], frame[7], frame[8], frame[9], frame[10], frame[11]],
        status: payload[5],
        reason: payload[6],
        index: u16::from_be_bytes([payload[8], payload[9]]),
        ttl: u16::from_be_bytes([payload[10], payload[11]]),
        old_ip: Ipv4Addr::from(u32::from_be_bytes([
            payload[12],
            payload[13],
            payload[14],
            payload[15],
        ])),
        new_ip: Ipv4Addr::from(u32::from_be_bytes([
            payload[16],
            payload[17],
            payload[18],
            payload[19],
        ])),
    })
}

#[allow(clippy::too_many_arguments)]
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

pub fn crc16_ccitt(data: &[u8]) -> u16 {
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

    fn test_reply(status: u8, old_ip: Ipv4Addr, new_ip: Ipv4Addr) -> Reply {
        Reply {
            mac: [0x6e, 0x7a, 0xf8, 0xd3, 0x19, 0x6a],
            status,
            reason: 0,
            index: 1,
            ttl: 199,
            old_ip,
            new_ip,
        }
    }

    #[test]
    fn dedupe_keeps_first_terminal_reply() {
        let old_ip = Ipv4Addr::new(192, 168, 3, 66);
        let new_ip = Ipv4Addr::new(192, 168, 3, 100);
        let replies = vec![
            test_reply(4, old_ip, new_ip),
            test_reply(0, old_ip, new_ip),
            test_reply(1, new_ip, new_ip),
            test_reply(1, new_ip, new_ip),
        ];

        let boards = dedupe_replies(&replies);
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].status, 0);
        assert_eq!(boards[0].old_ip, old_ip);
        assert_eq!(boards[0].new_ip, new_ip);
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
