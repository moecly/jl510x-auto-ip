use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use auto_ip::{Reply, RunEvent, RunOptions, RunSummary, MAX_BOARD_INDEX};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

#[derive(Clone, Default)]
struct AppState {
    running: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UiConfig {
    interface: Option<String>,
    source_mac: Option<String>,
    ttl: u16,
    index_offset: u8,
    base_ip: String,
    netmask: String,
    gateway: String,
    count: u32,
    interval_ms: u64,
    reply_timeout_ms: u64,
    map_count: u32,
}

impl UiConfig {
    fn to_options(&self) -> Result<RunOptions> {
        Ok(RunOptions {
            interface: self
                .interface
                .clone()
                .filter(|value| !value.trim().is_empty()),
            source_mac: self
                .source_mac
                .clone()
                .filter(|value| !value.trim().is_empty()),
            ttl: self.ttl,
            index_offset: self.index_offset,
            base_ip: parse_ip(&self.base_ip, "base IP")?,
            netmask: parse_ip(&self.netmask, "netmask")?,
            gateway: parse_ip(&self.gateway, "gateway")?,
            count: self.count,
            interval_ms: self.interval_ms,
            reply_timeout_ms: self.reply_timeout_ms,
        })
    }
}

fn parse_ip(value: &str, name: &str) -> Result<Ipv4Addr> {
    value
        .trim()
        .parse()
        .with_context(|| format!("invalid {name}: {value}"))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InterfaceView {
    name: String,
    description: String,
    mac: Option<String>,
    is_up: bool,
    is_loopback: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplyView {
    index: u16,
    ttl: u16,
    mac: String,
    old_ip: String,
    new_ip: String,
    status: u8,
    status_text: String,
    detail: String,
}

impl From<Reply> for ReplyView {
    fn from(reply: Reply) -> Self {
        Self {
            index: reply.index,
            ttl: reply.ttl,
            mac: auto_ip::format_mac(&reply.mac),
            old_ip: reply.old_ip.to_string(),
            new_ip: reply.new_ip.to_string(),
            status: reply.status,
            status_text: auto_ip::status_text(reply.status).to_string(),
            detail: auto_ip::reason_text(reply.status, reply.reason).to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MappingView {
    index: u32,
    ip: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewView {
    interface: String,
    source_mac: String,
    frame_hex: String,
    mapping: Vec<MappingView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunSummaryView {
    interface: String,
    source_mac: String,
    sent: u32,
    reply_frames: usize,
    boards: Vec<ReplyView>,
}

impl From<RunSummary> for RunSummaryView {
    fn from(summary: RunSummary) -> Self {
        Self {
            interface: summary.interface,
            source_mac: summary.source_mac.to_string(),
            sent: summary.sent,
            reply_frames: summary.replies.len(),
            boards: summary.boards.into_iter().map(ReplyView::from).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum RunEventView {
    Sent { sent: u32 },
    Reply { reply: ReplyView },
}

#[tauri::command]
fn list_interfaces() -> Vec<InterfaceView> {
    let mut interfaces: Vec<InterfaceView> = pnet::datalink::interfaces()
        .into_iter()
        .map(|interface| {
            let is_up = interface.is_up();
            let is_loopback = interface.is_loopback();
            InterfaceView {
                name: interface.name,
                description: interface.description,
                mac: interface.mac.map(|mac| mac.to_string()),
                is_up,
                is_loopback,
            }
        })
        .collect();

    interfaces.sort_by(|a, b| {
        b.is_up
            .cmp(&a.is_up)
            .then(a.is_loopback.cmp(&b.is_loopback))
            .then(a.name.cmp(&b.name))
    });
    interfaces
}

#[tauri::command]
fn dry_run(config: UiConfig) -> std::result::Result<PreviewView, String> {
    if config.map_count > MAX_BOARD_INDEX {
        return Err(format!("map count must be <= {MAX_BOARD_INDEX}"));
    }
    let options = config.to_options().map_err(|err| format!("{err:#}"))?;
    let prepared = auto_ip::prepare(&options).map_err(|err| format!("{err:#}"))?;
    let mapping = auto_ip::mapping_rows(options.base_ip, config.map_count)
        .into_iter()
        .map(|(index, ip)| MappingView {
            index,
            ip: ip.to_string(),
        })
        .collect();

    Ok(PreviewView {
        interface: prepared.interface.name,
        source_mac: prepared.source_mac.to_string(),
        frame_hex: auto_ip::format_hex(&prepared.frame),
        mapping,
    })
}

#[tauri::command]
async fn start_run(
    app: AppHandle,
    state: State<'_, AppState>,
    config: UiConfig,
) -> std::result::Result<RunSummaryView, String> {
    if config.map_count > MAX_BOARD_INDEX {
        return Err(format!("map count must be <= {MAX_BOARD_INDEX}"));
    }
    let options = config.to_options().map_err(|err| format!("{err:#}"))?;
    let running = state.running.clone();
    drop(state);

    let stop = {
        let mut slot = running
            .lock()
            .map_err(|_| "run state is poisoned".to_string())?;
        if slot.is_some() {
            return Err("a run is already active".to_string());
        }
        let stop = Arc::new(AtomicBool::new(false));
        *slot = Some(stop.clone());
        stop
    };

    let app_handle = app.clone();
    let task = tauri::async_runtime::spawn_blocking(move || {
        auto_ip::run(&options, stop, |event| {
            let view = match event {
                RunEvent::Sent { sent } => RunEventView::Sent { sent },
                RunEvent::Reply(reply) => RunEventView::Reply {
                    reply: reply.into(),
                },
            };
            let _ = app_handle.emit("run-event", view);
        })
    })
    .await;

    if let Ok(mut slot) = running.lock() {
        *slot = None;
    }

    match task {
        Ok(Ok(summary)) => Ok(summary.into()),
        Ok(Err(err)) => Err(format!("{err:#}")),
        Err(err) => Err(format!("run task failed: {err}")),
    }
}

#[tauri::command]
fn stop_run(state: State<'_, AppState>) -> std::result::Result<(), String> {
    let slot = state
        .running
        .lock()
        .map_err(|_| "run state is poisoned".to_string())?;
    if let Some(stop) = slot.as_ref() {
        stop.store(true, Ordering::Relaxed);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn configure_linux_rendering() {
    // WebKitGTK DMABUF/Wayland rendering is broken under niri; X11 is the supported path.
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    std::env::set_var("GDK_BACKEND", "x11");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    configure_linux_rendering();

    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            list_interfaces,
            dry_run,
            start_run,
            stop_run
        ])
        .run(tauri::generate_context!())
        .expect("error while running Auto IP");
}
