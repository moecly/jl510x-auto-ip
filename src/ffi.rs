// C API users only need config + auto_ip_run; the C header documents ownership.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, CStr, CString};
use std::net::Ipv4Addr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::{RunOptions, RunSummary};

pub const AUTO_IP_ABI_VERSION: u32 = 1;

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

pub struct AutoIpResult {
    interface: CString,
    source_mac: CString,
    sent: u32,
    replies: Vec<AutoIpReply>,
    boards: Vec<AutoIpReply>,
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

fn result_from_summary(summary: RunSummary) -> AutoIpResult {
    AutoIpResult {
        interface: c_string(summary.interface),
        source_mac: c_string(summary.source_mac.to_string()),
        sent: summary.sent,
        replies: summary.replies.into_iter().map(reply_to_ffi).collect(),
        boards: summary.boards.into_iter().map(reply_to_ffi).collect(),
    }
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

#[no_mangle]
pub unsafe extern "C" fn auto_ip_run(
    config: *const AutoIpConfig,
    out_result: *mut *mut AutoIpResult,
    error: *mut *mut c_char,
) -> c_int {
    if !out_result.is_null() {
        *out_result = ptr::null_mut();
    }
    clear_error(error);
    if out_result.is_null() {
        set_error(error, "out_result is null");
        return -1;
    }

    match catch_unwind(AssertUnwindSafe(|| {
        let options = config_to_options(config)?;
        crate::run(&options, Arc::new(AtomicBool::new(false)), |_| {})
            .map(result_from_summary)
            .map_err(|error| format!("{error:#}"))
    })) {
        Ok(Ok(result)) => {
            *out_result = Box::into_raw(Box::new(result));
            0
        }
        Ok(Err(message)) => {
            set_error(error, message);
            -1
        }
        Err(_) => {
            set_error(error, "panic while running");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_result_interface(result: *const AutoIpResult) -> *const c_char {
    result
        .as_ref()
        .map_or(ptr::null(), |result| result.interface.as_ptr())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_result_source_mac(result: *const AutoIpResult) -> *const c_char {
    result
        .as_ref()
        .map_or(ptr::null(), |result| result.source_mac.as_ptr())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_result_sent(result: *const AutoIpResult) -> u32 {
    result.as_ref().map_or(0, |result| result.sent)
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_result_reply_count(result: *const AutoIpResult) -> usize {
    result.as_ref().map_or(0, |result| result.replies.len())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_result_board_count(result: *const AutoIpResult) -> usize {
    result.as_ref().map_or(0, |result| result.boards.len())
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_result_reply(
    result: *const AutoIpResult,
    index: usize,
    out_reply: *mut AutoIpReply,
) -> c_int {
    let Some(reply) = result.as_ref().and_then(|result| result.replies.get(index)) else {
        return -1;
    };
    if out_reply.is_null() {
        return -1;
    }
    *out_reply = *reply;
    0
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_result_board(
    result: *const AutoIpResult,
    index: usize,
    out_reply: *mut AutoIpReply,
) -> c_int {
    let Some(reply) = result.as_ref().and_then(|result| result.boards.get(index)) else {
        return -1;
    };
    if out_reply.is_null() {
        return -1;
    }
    *out_reply = *reply;
    0
}

#[no_mangle]
pub unsafe extern "C" fn auto_ip_result_free(result: *mut AutoIpResult) {
    if !result.is_null() {
        drop(Box::from_raw(result));
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
    fn config_init_sets_cli_defaults() {
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
        assert_eq!(config.interval_ms, 1000);
        assert_eq!(config.reply_timeout_ms, 1500);
        assert!(!config.base_ip.is_null());
    }

    #[test]
    fn invalid_ttl_is_rejected() {
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

        let mut result = ptr::null_mut();
        let mut error = ptr::null_mut();
        let status = unsafe { auto_ip_run(&config, &mut result, &mut error) };
        assert_eq!(status, -1);
        assert!(result.is_null());
        assert!(!error.is_null());
        unsafe { auto_ip_string_free(error) };
    }
}
