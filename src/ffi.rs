// Safety contracts are documented in include/auto_ip.h.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::net::Ipv4Addr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::{RunEvent, RunOptions, RunSummary};

pub const AUTO_IP_ABI_VERSION: u32 = 1;
pub const AUTO_IP_EVENT_SENT: u32 = 0;
pub const AUTO_IP_EVENT_REPLY: u32 = 1;

static DEFAULT_BASE_IP: &[u8] = b"192.168.1.10\0";
static DEFAULT_NETMASK: &[u8] = b"255.255.255.0\0";
static DEFAULT_GATEWAY: &[u8] = b"192.168.1.1\0";

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AutoIpReply {
    pub mac: [u8; 6],
    pub status: u8,
    pub reason: u8,
    pub index: u16,
    pub ttl: u16,
    pub old_ip: u32,
    pub new_ip: u32,
}

impl AutoIpReply {
    const fn zero() -> Self {
        Self {
            mac: [0; 6],
            status: 0,
            reason: 0,
            index: 0,
            ttl: 0,
            old_ip: 0,
            new_ip: 0,
        }
    }
}

#[repr(C)]
pub struct AutoIpConfig {
    pub interface_name: *const c_char,
    pub source_mac: *const c_char,
    pub base_ip: *const c_char,
    pub netmask: *const c_char,
    pub gateway: *const c_char,
    pub ttl: u16,
    pub index_offset: u8,
    pub reserved: u8,
    pub count: u32,
    pub interval_ms: u64,
    pub reply_timeout_ms: u64,
}

#[repr(C)]
pub struct AutoIpEvent {
    pub kind: u32,
    pub sent: u32,
    pub reply: AutoIpReply,
}

pub type AutoIpEventCallback = Option<unsafe extern "C" fn(*const AutoIpEvent, *mut c_void)>;

#[repr(C)]
pub struct AutoIpInterface {
    pub name: *const c_char,
    pub description: *const c_char,
    pub mac: [u8; 6],
    pub has_mac: u8,
    pub is_up: u8,
    pub is_loopback: u8,
    pub reserved: u8,
}

struct InterfaceEntry {
    name: CString,
    description: CString,
    mac: [u8; 6],
    has_mac: u8,
    is_up: u8,
    is_loopback: u8,
}

pub struct AutoIpInterfaceList {
    entries: Vec<InterfaceEntry>,
}

pub struct AutoIpRun {
    stop: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<anyhow::Result<RunSummary>>>>,
}

pub struct AutoIpSummary {
    interface: CString,
    source_mac: CString,
    sent: u32,
    replies: Vec<AutoIpReply>,
    boards: Vec<AutoIpReply>,
}

pub struct AutoIpPreview {
    interface: CString,
    source_mac: CString,
    frame: [u8; crate::FRAME_LEN],
    nonce: u32,
}

struct SendUserData(*mut c_void);

unsafe impl Send for SendUserData {}

impl SendUserData {
    fn ptr(&self) -> *mut c_void {
        self.0
    }
}

fn c_string(text: impl AsRef<str>) -> CString {
    let owned = text.as_ref().replace('\0', " ");
    CString::new(owned).unwrap_or_default()
}

unsafe fn set_error(out: *mut *mut c_char, message: impl AsRef<str>) {
    if !out.is_null() {
        *out = c_string(message).into_raw();
    }
}

unsafe fn clear_error(out: *mut *mut c_char) {
    if !out.is_null() {
        *out = ptr::null_mut();
    }
}

unsafe fn optional_cstr(value: *const c_char, field: &str) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    CStr::from_ptr(value)
        .to_str()
        .map(|text| Some(text.to_owned()))
        .map_err(|_| format!("{field} is not valid UTF-8"))
}

unsafe fn required_cstr(value: *const c_char, field: &str) -> Result<String, String> {
    optional_cstr(value, field)?.ok_or_else(|| format!("{field} is null"))
}

fn parse_ipv4(value: String, field: &str) -> Result<Ipv4Addr, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {field}: {value}"))
}

unsafe fn config_to_options(config: *const AutoIpConfig) -> Result<RunOptions, String> {
    let config = config.as_ref().ok_or("config is null")?;
    let options = RunOptions {
        interface: optional_cstr(config.interface_name, "interface_name")?,
        source_mac: optional_cstr(config.source_mac, "source_mac")?,
        ttl: config.ttl,
        index_offset: config.index_offset,
        base_ip: parse_ipv4(required_cstr(config.base_ip, "base_ip")?, "base_ip")?,
        netmask: parse_ipv4(required_cstr(config.netmask, "netmask")?, "netmask")?,
        gateway: parse_ipv4(required_cstr(config.gateway, "gateway")?, "gateway")?,
        count: config.count,
        interval_ms: config.interval_ms,
        reply_timeout_ms: config.reply_timeout_ms,
    };
    crate::validate_options(&options).map_err(|error| format!("{error:#}"))?;
    Ok(options)
}

fn reply_to_ffi(reply: crate::Reply) -> AutoIpReply {
    AutoIpReply {
        mac: reply.mac,
        status: reply.status,
        reason: reply.reason,
        index: reply.index,
        ttl: reply.ttl,
        old_ip: u32::from_be_bytes(reply.old_ip.octets()),
        new_ip: u32::from_be_bytes(reply.new_ip.octets()),
    }
}

fn event_to_ffi(event: RunEvent) -> AutoIpEvent {
    match event {
        RunEvent::Sent { sent } => AutoIpEvent {
            kind: AUTO_IP_EVENT_SENT,
            sent,
            reply: AutoIpReply::zero(),
        },
        RunEvent::Reply(reply) => AutoIpEvent {
            kind: AUTO_IP_EVENT_REPLY,
            sent: 0,
            reply: reply_to_ffi(reply),
        },
    }
}

unsafe fn run_start_impl(
    config: *const AutoIpConfig,
    callback: AutoIpEventCallback,
    user_data: *mut c_void,
    out_run: *mut *mut AutoIpRun,
) -> Result<*mut AutoIpRun, String> {
    if out_run.is_null() {
        return Err("out_run is null".to_string());
    }
    let options = config_to_options(config)?;
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let user = SendUserData(user_data);

    let worker = thread::Builder::new()
        .name("auto-ip-run".to_string())
        .spawn(move || {
            crate::run(&options, worker_stop, |event| {
                if let Some(callback) = callback {
                    let ffi_event = event_to_ffi(event);
                    unsafe { callback(&ffi_event, user.ptr()) };
                }
            })
        })
        .map_err(|error| format!("spawn run thread: {error}"))?;

    Ok(Box::into_raw(Box::new(AutoIpRun {
        stop,
        worker: Mutex::new(Some(worker)),
    })))
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_config_init(config: *mut AutoIpConfig) {
    if config.is_null() {
        return;
    }
    ptr::write(
        config,
        AutoIpConfig {
            interface_name: ptr::null(),
            source_mac: ptr::null(),
            base_ip: DEFAULT_BASE_IP.as_ptr().cast(),
            netmask: DEFAULT_NETMASK.as_ptr().cast(),
            gateway: DEFAULT_GATEWAY.as_ptr().cast(),
            ttl: 200,
            index_offset: 0,
            reserved: 0,
            count: 1,
            interval_ms: 1000,
            reply_timeout_ms: 1500,
        },
    );
}

#[no_mangle]
pub extern "C" fn auto_ip_abi_version() -> u32 {
    AUTO_IP_ABI_VERSION
}

#[no_mangle]
pub extern "C" fn auto_ip_version() -> *const c_char {
    static VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");
    VERSION.as_ptr().cast()
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(CString::from_raw(value));
    }
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_list_interfaces(
    out_list: *mut *mut AutoIpInterfaceList,
    error: *mut *mut c_char,
) -> c_int {
    if !out_list.is_null() {
        *out_list = ptr::null_mut();
    }
    clear_error(error);
    if out_list.is_null() {
        set_error(error, "out_list is null");
        return -1;
    }

    let result = catch_unwind(AssertUnwindSafe(|| {
        let entries = pnet::datalink::interfaces()
            .into_iter()
            .map(|interface| {
                let has_mac = interface.mac.is_some();
                let mac = interface.mac.map(|mac| mac.octets()).unwrap_or([0; 6]);
                let is_up = interface.is_up();
                let is_loopback = interface.is_loopback();
                InterfaceEntry {
                    name: c_string(interface.name),
                    description: c_string(interface.description),
                    mac,
                    has_mac: has_mac as u8,
                    is_up: is_up as u8,
                    is_loopback: is_loopback as u8,
                }
            })
            .collect();
        AutoIpInterfaceList { entries }
    }));

    match result {
        Ok(list) => {
            *out_list = Box::into_raw(Box::new(list));
            0
        }
        Err(_) => {
            set_error(error, "panic while enumerating interfaces");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_interface_list_count(list: *const AutoIpInterfaceList) -> usize {
    list.as_ref().map_or(0, |list| list.entries.len())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_interface_list_get(
    list: *const AutoIpInterfaceList,
    index: usize,
    out: *mut AutoIpInterface,
) -> c_int {
    let Some(entry) = list.as_ref().and_then(|list| list.entries.get(index)) else {
        return -1;
    };
    if out.is_null() {
        return -1;
    }
    *out = AutoIpInterface {
        name: entry.name.as_ptr(),
        description: entry.description.as_ptr(),
        mac: entry.mac,
        has_mac: entry.has_mac,
        is_up: entry.is_up,
        is_loopback: entry.is_loopback,
        reserved: 0,
    };
    0
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_interface_list_free(list: *mut AutoIpInterfaceList) {
    if !list.is_null() {
        drop(Box::from_raw(list));
    }
}

unsafe fn preview_impl(config: *const AutoIpConfig) -> Result<AutoIpPreview, String> {
    let options = config_to_options(config)?;
    let prepared = crate::prepare(&options).map_err(|error| format!("{error:#}"))?;
    Ok(AutoIpPreview {
        interface: c_string(prepared.interface.name),
        source_mac: c_string(prepared.source_mac.to_string()),
        frame: prepared.frame,
        nonce: prepared.nonce,
    })
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_preview_build(
    config: *const AutoIpConfig,
    out_preview: *mut *mut AutoIpPreview,
    error: *mut *mut c_char,
) -> c_int {
    if !out_preview.is_null() {
        *out_preview = ptr::null_mut();
    }
    clear_error(error);
    if out_preview.is_null() {
        set_error(error, "out_preview is null");
        return -1;
    }

    match catch_unwind(AssertUnwindSafe(|| preview_impl(config))) {
        Ok(Ok(preview)) => {
            *out_preview = Box::into_raw(Box::new(preview));
            0
        }
        Ok(Err(message)) => {
            set_error(error, message);
            -1
        }
        Err(_) => {
            set_error(error, "panic while preparing frame");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_preview_interface(preview: *const AutoIpPreview) -> *const c_char {
    preview
        .as_ref()
        .map_or(ptr::null(), |preview| preview.interface.as_ptr())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_preview_source_mac(
    preview: *const AutoIpPreview,
) -> *const c_char {
    preview
        .as_ref()
        .map_or(ptr::null(), |preview| preview.source_mac.as_ptr())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_preview_frame(preview: *const AutoIpPreview) -> *const u8 {
    preview
        .as_ref()
        .map_or(ptr::null(), |preview| preview.frame.as_ptr())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_preview_frame_len(_preview: *const AutoIpPreview) -> usize {
    crate::FRAME_LEN
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_preview_nonce(preview: *const AutoIpPreview) -> u32 {
    preview.as_ref().map_or(0, |preview| preview.nonce)
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_preview_free(preview: *mut AutoIpPreview) {
    if !preview.is_null() {
        drop(Box::from_raw(preview));
    }
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_parse_reply(
    frame: *const u8,
    len: usize,
    nonce: u32,
    out_reply: *mut AutoIpReply,
) -> c_int {
    if frame.is_null() || out_reply.is_null() || len > isize::MAX as usize {
        return -1;
    }

    match catch_unwind(AssertUnwindSafe(|| {
        let frame = std::slice::from_raw_parts(frame, len);
        crate::parse_reply(frame, nonce)
    })) {
        Ok(Some(reply)) => {
            *out_reply = reply_to_ffi(reply);
            0
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_run_start(
    config: *const AutoIpConfig,
    callback: AutoIpEventCallback,
    user_data: *mut c_void,
    out_run: *mut *mut AutoIpRun,
    error: *mut *mut c_char,
) -> c_int {
    if !out_run.is_null() {
        *out_run = ptr::null_mut();
    }
    clear_error(error);

    match catch_unwind(AssertUnwindSafe(|| {
        run_start_impl(config, callback, user_data, out_run)
    })) {
        Ok(Ok(run)) => {
            *out_run = run;
            0
        }
        Ok(Err(message)) => {
            set_error(error, message);
            -1
        }
        Err(_) => {
            set_error(error, "panic while starting run");
            -1
        }
    }
}

unsafe fn run_wait_impl(
    run: *mut AutoIpRun,
    out_summary: *mut *mut AutoIpSummary,
) -> Result<*mut AutoIpSummary, String> {
    if run.is_null() {
        return Err("run is null".to_string());
    }
    if out_summary.is_null() {
        return Err("out_summary is null".to_string());
    }

    let handle = {
        let mut worker = (*run)
            .worker
            .lock()
            .map_err(|_| "run handle is poisoned".to_string())?;
        worker
            .take()
            .ok_or_else(|| "run has already been waited for".to_string())?
    };

    let summary = handle
        .join()
        .map_err(|_| "run thread panicked".to_string())?
        .map_err(|error| format!("{error:#}"))?;

    Ok(Box::into_raw(Box::new(AutoIpSummary {
        interface: c_string(summary.interface),
        source_mac: c_string(summary.source_mac.to_string()),
        sent: summary.sent,
        replies: summary.replies.into_iter().map(reply_to_ffi).collect(),
        boards: summary.boards.into_iter().map(reply_to_ffi).collect(),
    })))
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_run_wait(
    run: *mut AutoIpRun,
    out_summary: *mut *mut AutoIpSummary,
    error: *mut *mut c_char,
) -> c_int {
    if !out_summary.is_null() {
        *out_summary = ptr::null_mut();
    }
    clear_error(error);

    match catch_unwind(AssertUnwindSafe(|| run_wait_impl(run, out_summary))) {
        Ok(Ok(summary)) => {
            *out_summary = summary;
            0
        }
        Ok(Err(message)) => {
            set_error(error, message);
            -1
        }
        Err(_) => {
            set_error(error, "panic while waiting for run");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_run_stop(run: *mut AutoIpRun) {
    if let Some(run) = run.as_ref() {
        run.stop.store(true, Ordering::Relaxed);
    }
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_run_free(run: *mut AutoIpRun) {
    if run.is_null() {
        return;
    }
    let run_ref = &*run;
    run_ref.stop.store(true, Ordering::Relaxed);
    let worker = run_ref
        .worker
        .lock()
        .ok()
        .and_then(|mut worker| worker.take());
    if let Some(worker) = worker {
        let _ = worker.join();
    }
    drop(Box::from_raw(run));
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_run_blocking(
    config: *const AutoIpConfig,
    callback: AutoIpEventCallback,
    user_data: *mut c_void,
    out_summary: *mut *mut AutoIpSummary,
    error: *mut *mut c_char,
) -> c_int {
    if !out_summary.is_null() {
        *out_summary = ptr::null_mut();
    }
    clear_error(error);
    if out_summary.is_null() {
        set_error(error, "out_summary is null");
        return -1;
    }

    let mut run = ptr::null_mut();
    let mut status = auto_ip_run_start(config, callback, user_data, &mut run, error);
    if status == 0 {
        status = auto_ip_run_wait(run, out_summary, error);
    }
    auto_ip_run_free(run);
    status
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_summary_interface(summary: *const AutoIpSummary) -> *const c_char {
    summary
        .as_ref()
        .map_or(ptr::null(), |summary| summary.interface.as_ptr())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_summary_source_mac(
    summary: *const AutoIpSummary,
) -> *const c_char {
    summary
        .as_ref()
        .map_or(ptr::null(), |summary| summary.source_mac.as_ptr())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_summary_sent(summary: *const AutoIpSummary) -> u32 {
    summary.as_ref().map_or(0, |summary| summary.sent)
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_summary_reply_count(summary: *const AutoIpSummary) -> usize {
    summary.as_ref().map_or(0, |summary| summary.replies.len())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_summary_board_count(summary: *const AutoIpSummary) -> usize {
    summary.as_ref().map_or(0, |summary| summary.boards.len())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_summary_reply(
    summary: *const AutoIpSummary,
    index: usize,
    out_reply: *mut AutoIpReply,
) -> c_int {
    let Some(reply) = summary
        .as_ref()
        .and_then(|summary| summary.replies.get(index))
    else {
        return -1;
    };
    if out_reply.is_null() {
        return -1;
    }
    *out_reply = *reply;
    0
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_summary_board(
    summary: *const AutoIpSummary,
    index: usize,
    out_reply: *mut AutoIpReply,
) -> c_int {
    let Some(reply) = summary
        .as_ref()
        .and_then(|summary| summary.boards.get(index))
    else {
        return -1;
    };
    if out_reply.is_null() {
        return -1;
    }
    *out_reply = *reply;
    0
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_summary_free(summary: *mut AutoIpSummary) {
    if !summary.is_null() {
        drop(Box::from_raw(summary));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_version_is_stable() {
        assert_eq!(auto_ip_abi_version(), 1);
    }

    #[test]
    fn config_init_sets_defaults() {
        let mut config = AutoIpConfig {
            interface_name: ptr::null(),
            source_mac: ptr::null(),
            base_ip: ptr::null(),
            netmask: ptr::null(),
            gateway: ptr::null(),
            ttl: 0,
            index_offset: 0,
            reserved: 0,
            count: 0,
            interval_ms: 0,
            reply_timeout_ms: 0,
        };
        unsafe { auto_ip_config_init(&mut config) };
        assert_eq!(config.ttl, 200);
        assert_eq!(config.count, 1);
        assert!(!config.base_ip.is_null());
    }

    #[test]
    fn invalid_ttl_is_rejected_before_spawning() {
        let mut config = AutoIpConfig {
            interface_name: ptr::null(),
            source_mac: ptr::null(),
            base_ip: ptr::null(),
            netmask: ptr::null(),
            gateway: ptr::null(),
            ttl: 0,
            index_offset: 0,
            reserved: 0,
            count: 1,
            interval_ms: 0,
            reply_timeout_ms: 0,
        };
        unsafe { auto_ip_config_init(&mut config) };
        config.ttl = 1;

        let mut run = ptr::null_mut();
        let mut error = ptr::null_mut();
        let status =
            unsafe { auto_ip_run_start(&config, None, ptr::null_mut(), &mut run, &mut error) };
        assert_eq!(status, -1);
        assert!(run.is_null());
        assert!(!error.is_null());
        unsafe { auto_ip_string_free(error) };
    }
}
