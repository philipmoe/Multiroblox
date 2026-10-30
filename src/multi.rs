//! Holds the Roblox singleton objects so several clients can run side by side.
//!
//! The Roblox client refuses to start when the named object `ROBLOX_singletonEvent`
//! (older builds: `ROBLOX_singletonMutex`) already exists. We create those names first
//! and keep them open; for clients started earlier we close the handle inside the process.


#![allow(non_snake_case, non_camel_case_types)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{null, null_mut};

type HANDLE = *mut c_void;
type BOOL = i32;
type DWORD = u32;

const ERROR_INVALID_HANDLE: DWORD = 6;
const ERROR_ALREADY_EXISTS: DWORD = 183;
const PROCESS_TERMINATE: DWORD = 0x0001;
const PROCESS_DUP_HANDLE: DWORD = 0x0040;
const DUPLICATE_CLOSE_SOURCE: DWORD = 0x1;
const DUPLICATE_SAME_ACCESS: DWORD = 0x2;
const TH32CS_SNAPPROCESS: DWORD = 0x2;
const STATUS_INFO_LENGTH_MISMATCH: u32 = 0xC000_0004;
const SYSTEM_EXTENDED_HANDLE_INFORMATION: u32 = 64;
const OBJECT_NAME_INFORMATION: u32 = 1;

const SINGLETON_NAMES: [&str; 2] = ["ROBLOX_singletonEvent", "ROBLOX_singletonMutex"];
const PLAYER_EXE: &str = "RobloxPlayerBeta.exe";


#[repr(C)]
struct PROCESSENTRY32W {
    dwSize: DWORD,
    cntUsage: DWORD,
    th32ProcessID: DWORD,
    th32DefaultHeapID: usize,
    th32ModuleID: DWORD,
    cntThreads: DWORD,
    th32ParentProcessID: DWORD,
    pcPriClassBase: i32,
    dwFlags: DWORD,
    szExeFile: [u16; 260],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HandleEntry {
    object: usize,
    pid: usize,
    handle: usize,
    access: u32,
    backtrace: u16,
    type_index: u16,
    attrs: u32,
    reserved: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(attr: *const c_void, initial_owner: BOOL, name: *const u16) -> HANDLE;
    fn CreateEventW(attr: *const c_void, manual: BOOL, initial: BOOL, name: *const u16) -> HANDLE;
    fn GetLastError() -> DWORD;
    fn CloseHandle(h: HANDLE) -> BOOL;
    fn OpenProcess(access: DWORD, inherit: BOOL, pid: DWORD) -> HANDLE;
    fn DuplicateHandle(
        src_proc: HANDLE,
        src: HANDLE,
        dst_proc: HANDLE,
        dst: *mut HANDLE,
        access: DWORD,
        inherit: BOOL,
        options: DWORD,
    ) -> BOOL;
    fn GetCurrentProcess() -> HANDLE;
    fn GetCurrentProcessId() -> DWORD;
    fn CreateToolhelp32Snapshot(flags: DWORD, pid: DWORD) -> HANDLE;
    fn Process32FirstW(snap: HANDLE, pe: *mut PROCESSENTRY32W) -> BOOL;
    fn Process32NextW(snap: HANDLE, pe: *mut PROCESSENTRY32W) -> BOOL;
    fn TerminateProcess(h: HANDLE, code: u32) -> BOOL;
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(class: u32, buf: *mut c_void, len: u32, ret: *mut u32) -> i32;
    fn NtQueryObject(h: HANDLE, class: u32, buf: *mut c_void, len: u32, ret: *mut u32) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteW(
        hwnd: HANDLE,
        op: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
    ) -> HANDLE;
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    Held,
    AlreadyExisted,
    Failed(DWORD),
}

pub struct Singleton {
    #[allow(dead_code)]
    pub name: &'static str,
    pub handle: HANDLE,
    pub state: State,
}

/// Creates the named object, falling back to an event if the name is already
/// taken by an object of the other type.
fn create_named(name: &'static str) -> Singleton {
    let w = wide(name);
    unsafe {
        let mut h = CreateMutexW(null(), 0, w.as_ptr());
        let mut err = GetLastError();
        if h.is_null() && err == ERROR_INVALID_HANDLE {
            h = CreateEventW(null(), 1, 0, w.as_ptr());
            err = GetLastError();
        }
        let state = if h.is_null() {
            State::Failed(err)
        } else if err == ERROR_ALREADY_EXISTS {
            State::AlreadyExisted
        } else {
            State::Held
        };
        Singleton { name, handle: h, state }
    }
}

pub fn acquire() -> Vec<Singleton> {
    SINGLETON_NAMES.iter().map(|n| create_named(n)).collect()
}

pub fn drop_singletons(list: &mut Vec<Singleton>) {
    for s in list.drain(..) {
        if !s.handle.is_null() {
            unsafe { CloseHandle(s.handle) };
        }
    }
}

pub fn roblox_pids() -> Vec<u32> {
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap as isize == -1 {
            return out;
        }
        let mut pe: PROCESSENTRY32W = std::mem::zeroed();
        pe.dwSize = size_of::<PROCESSENTRY32W>() as DWORD;
        let mut ok = Process32FirstW(snap, &mut pe);
        while ok != 0 {
            let len = pe.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
            let exe = String::from_utf16_lossy(&pe.szExeFile[..len]);
            if exe.eq_ignore_ascii_case(PLAYER_EXE) {
                out.push(pe.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut pe);
        }
        CloseHandle(snap);
    }
    out
}

fn handle_table() -> Option<Vec<u8>> {
    let mut size: usize = 4 << 20;
    for _ in 0..8 {
        let mut buf = vec![0u8; size];
        let mut needed = 0u32;
        let st = unsafe {
            NtQuerySystemInformation(
                SYSTEM_EXTENDED_HANDLE_INFORMATION,
                buf.as_mut_ptr() as *mut c_void,
                size as u32,
                &mut needed,
            )
        };
        if st == 0 {
            return Some(buf);
        }
        if st as u32 != STATUS_INFO_LENGTH_MISMATCH {
            return None;
        }
        size = (size * 2).max(needed as usize + (1 << 20));
    }
    None
}

fn entries(table: &[u8]) -> impl Iterator<Item = HandleEntry> + '_ {
    let count = unsafe { std::ptr::read_unaligned(table.as_ptr() as *const usize) };
    let stride = size_of::<HandleEntry>();
    let max = table.len().saturating_sub(16) / stride;
    (0..count.min(max)).map(move |i| unsafe {
        std::ptr::read_unaligned(table.as_ptr().add(16 + i * stride) as *const HandleEntry)
    })
}

/// Object type indexes differ between Windows builds, so learn them by looking up
/// handles we just created ourselves.
fn mutex_and_event_types(table: &[u8], mutex: HANDLE, event: HANDLE) -> Option<(u16, u16)> {
    let me = unsafe { GetCurrentProcessId() } as usize;
    let (mut mt, mut et) = (None, None);
    for e in entries(table) {
        if e.pid != me {
            continue;
        }
        if e.handle == mutex as usize {
            mt = Some(e.type_index);
        } else if e.handle == event as usize {
            et = Some(e.type_index);
        }
    }
    Some((mt?, et?))
}

fn object_name(h: HANDLE) -> Option<String> {
    let mut buf = vec![0u64; 512];
    let mut ret = 0u32;
    let st = unsafe {
        NtQueryObject(
            h,
            OBJECT_NAME_INFORMATION,
            buf.as_mut_ptr() as *mut c_void,
            (buf.len() * 8) as u32,
            &mut ret,
        )
    };
    if st != 0 {
        return None;
    }
    unsafe {
        // UNICODE_STRING { u16 Length; u16 MaximumLength; PWSTR Buffer } (Buffer at +8 on x64)
        let len = *(buf.as_ptr() as *const u16) as usize / 2;
        let ptr = *((buf.as_ptr() as *const u8).add(8) as *const *const u16);
        if len == 0 || ptr.is_null() {
            return None;
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)))
    }
}

/// Closes the singleton handle inside every running Roblox client.
/// Returns how many handles were closed.
pub fn release_in_running_clients() -> usize {
    let pids = roblox_pids();
    if pids.is_empty() {
        return 0;
    }
    let (probe_m, probe_e) = unsafe {
        (
            CreateMutexW(null(), 0, null()),
            CreateEventW(null(), 1, 0, null()),
        )
    };
    let table = handle_table();
    let types = table
        .as_deref()
        .and_then(|t| mutex_and_event_types(t, probe_m, probe_e));
    unsafe {
        CloseHandle(probe_m);
        CloseHandle(probe_e);
    }
    let (Some(table), Some((mt, et))) = (table, types) else {
        return 0;
    };

    let mut procs: HashMap<usize, HANDLE> = HashMap::new();
    let mut closed = 0;
    for e in entries(&table) {
        if !pids.contains(&(e.pid as u32)) || (e.type_index != mt && e.type_index != et) {
            continue;
        }
        let proc = *procs
            .entry(e.pid)
            .or_insert_with(|| unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, e.pid as u32) });
        if proc.is_null() {
            continue;
        }
        unsafe {
            let mut dup: HANDLE = null_mut();
            if DuplicateHandle(
                proc,
                e.handle as HANDLE,
                GetCurrentProcess(),
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            ) == 0
            {
                continue;
            }
            let hit = object_name(dup)
                .map(|n| SINGLETON_NAMES.iter().any(|s| n.ends_with(s)))
                .unwrap_or(false);
            CloseHandle(dup);
            if hit
                && DuplicateHandle(
                    proc,
                    e.handle as HANDLE,
                    null_mut(),
                    null_mut(),
                    0,
                    0,
                    DUPLICATE_CLOSE_SOURCE,
                ) != 0
            {
                closed += 1;
            }
        }
    }
    for (_, p) in procs {
        if !p.is_null() {
            unsafe { CloseHandle(p) };
        }
    }
    closed
}

pub fn kill_all() -> usize {
    let mut n = 0;
    for pid in roblox_pids() {
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !h.is_null() {
                if TerminateProcess(h, 0) != 0 {
                    n += 1;
                }
                CloseHandle(h);
            }
        }
    }
    n
}

pub fn shell_open(uri: &str) -> bool {
    let op = wide("open");
    let file = wide(uri);
    let r = unsafe { ShellExecuteW(null_mut(), op.as_ptr(), file.as_ptr(), null(), null(), 1) };
    r as usize > 32
}


/// True when this is the only copy of the app running. The handle is kept for the life of the process.
pub fn claim_app_guard() -> bool {
    let w = wide(r"MultiRoblox_rs_singleton");
    unsafe {
        let h = CreateMutexW(null(), 0, w.as_ptr());
        !h.is_null() && GetLastError() != ERROR_ALREADY_EXISTS
    }
}

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(hwnd: HANDLE, text: *const u16, caption: *const u16, kind: u32) -> i32;
}

pub fn message_box(title: &str, text: &str) {
    let (t, c) = (wide(text), wide(title));
    unsafe { MessageBoxW(null_mut(), t.as_ptr(), c.as_ptr(), 0x40) };
}

#[repr(C)]
#[derive(Default)]
struct SYSTEMTIME {
    year: u16,
    month: u16,
    dow: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    millis: u16,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLocalTime(st: *mut SYSTEMTIME);
}

/// Local wall-clock time as "HH:MM:SS".
pub fn clock() -> String {
    let mut st = SYSTEMTIME::default();
    unsafe { GetLocalTime(&mut st) };
    format!("{:02}:{:02}:{:02}", st.hour, st.minute, st.second)
}
