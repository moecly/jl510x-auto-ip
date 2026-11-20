use std::net::Ipv4Addr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{bail, Result};
use clap::Parser;
use pnet::datalink;

use auto_ip::{
    format_hex, format_mac, mapping_rows, prepare, reason_text, run, status_text, RunOptions,
    RunSummary, MAX_BOARD_INDEX,
};

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

    if args.map_count > MAX_BOARD_INDEX {
        bail!("map-count must be <= {MAX_BOARD_INDEX}: the firmware ignores larger indexes");
    }

    let options = RunOptions {
        interface: args.interface.clone(),
        source_mac: args.source_mac.clone(),
        ttl: args.ttl,
        index_offset: args.index_offset,
        base_ip: args.base_ip,
        netmask: args.netmask,
        gateway: args.gateway,
        count: args.count,
        interval_ms: args.interval_ms,
        reply_timeout_ms: args.reply_timeout_ms,
    };

    let prepared = prepare(&options)?;
    println!(
        "interface: {} ({})",
        prepared.interface.name, prepared.source_mac
    );
    println!(
        "frame: {} bytes, ethertype 0x{:04x}, ttl {}, index-offset {}",
        prepared.frame.len(),
        auto_ip::ETHERTYPE_LOOP,
        args.ttl,
        args.index_offset
    );
    println!(
        "base {} netmask {} gateway {}",
        args.base_ip, args.netmask, args.gateway
    );
    print_mapping(args.base_ip, args.map_count);

    if args.dry_run {
        print_hex(&prepared.frame);
        return Ok(());
    }

    let stop = Arc::new(AtomicBool::new(false));
    let summary = run(&options, stop, |event| {
        if let auto_ip::RunEvent::Sent { sent } = event {
            println!("sent {sent}");
        }
    })?;
    print_replies(&summary);
    Ok(())
}

fn print_replies(summary: &RunSummary) {
    println!(
        "replies: {} frame(s), {} board(s)",
        summary.replies.len(),
        summary.boards.len()
    );
    if summary.boards.is_empty() {
        println!("  no board replied; check link, interface and that frames reach the chain");
        return;
    }

    println!(
        "{:<5} {:<17} {:<15} {:<15} {:<5} {:<14} detail",
        "board", "mac", "old ip", "new ip", "ttl", "status"
    );
    for reply in &summary.boards {
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

fn print_mapping(base_ip: Ipv4Addr, count: u32) {
    for (index, ip) in mapping_rows(base_ip, count) {
        println!("  board {index:>2} -> {ip}");
    }
}

fn print_hex(frame: &[u8]) {
    print!("{}", format_hex(frame));
}
