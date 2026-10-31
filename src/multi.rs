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
const PROCESS_SET_INFORMATION: DWORD = 0x0200;
const PROCESS_SET_QUOTA: DWORD = 0x0100;
const JOB_OBJECT_CPU_RATE_CONTROL_INFORMATION_CLASS: i32 = 15;
const JOB_OBJECT_CPU_RATE_CONTROL_ENABLE: u32 = 0x1;
const JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP: u32 = 0x4;

#[repr(C)]
struct JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
    flags: u32,
    rate: u32,
}
const NORMAL_PRIORITY_CLASS: DWORD = 0x0020;
const BELOW_NORMAL_PRIORITY_CLASS: DWORD = 0x4000;
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
    fn SetPriorityClass(h: HANDLE, class: DWORD) -> BOOL;
    fn CreateJobObjectW(attr: *const c_void, name: *const u16) -> HANDLE;
    fn AssignProcessToJobObject(job: HANDLE, process: HANDLE) -> BOOL;
    fn SetInformationJobObject(job: HANDLE, class: i32, info: *const c_void, len: u32) -> BOOL;
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

pub fn kill(pid: u32) -> bool {
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return false;
        }
        let ok = TerminateProcess(h, 0) != 0;
        CloseHandle(h);
        ok
    }
}

pub fn kill_all() -> usize {
    roblox_pids().into_iter().filter(|&pid| kill(pid)).count()
}

/// A Windows job object holding one client, used to cap how much CPU it may use. A process can't
/// leave a job, so turning the limit off just disables the cap on the same job.
pub struct CpuLimit(usize);

impl CpuLimit {
    pub fn attach(pid: u32) -> Option<CpuLimit> {
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return None;
            }
            let limit = CpuLimit(job as usize);
            let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if proc.is_null() {
                return None;
            }
            let ok = AssignProcessToJobObject(job, proc) != 0;
            CloseHandle(proc);
            ok.then_some(limit)
        }
    }

    /// Caps the client at `percent` of the PC's total CPU, or lifts the cap with `None`.
    pub fn set(&self, percent: Option<u32>) -> bool {
        let info = match percent {
            Some(p) => JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
                flags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
                // In hundredths of a percent.
                rate: p.clamp(1, 100) * 100,
            },
            None => JOBOBJECT_CPU_RATE_CONTROL_INFORMATION { flags: 0, rate: 0 },
        };
        unsafe {
            SetInformationJobObject(
                self.0 as HANDLE,
                JOB_OBJECT_CPU_RATE_CONTROL_INFORMATION_CLASS,
                &info as *const _ as *const c_void,
                size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            ) != 0
        }
    }
}

impl Drop for CpuLimit {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0 as HANDLE) };
    }
}

/// Runs a client at below-normal priority so the main client gets the CPU first.
pub fn set_low_priority(pid: u32, low: bool) -> bool {
    let class = if low { BELOW_NORMAL_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS };
    unsafe {
        let h = OpenProcess(PROCESS_SET_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let ok = SetPriorityClass(h, class) != 0;
        CloseHandle(h);
        ok
    }
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

const GW_OWNER: u32 = 4;
const WM_SETTEXT: u32 = 0x000C;
const SMTO_ABORTIFHUNG: u32 = 0x2;
const SW_MINIMIZE: i32 = 6;
const SW_RESTORE: i32 = 9;
const INPUT_MOUSE: u32 = 0;
const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_KEYUP: u32 = 0x2;
const KEYEVENTF_SCANCODE: u32 = 0x8;
const MOUSEEVENTF_MOVE: u32 = 0x1;
const SCAN_SPACE: u16 = 0x39;
const ES_CONTINUOUS: u32 = 0x8000_0000;
const ES_SYSTEM_REQUIRED: u32 = 0x1;

#[repr(C)]
#[derive(Clone, Copy)]
struct MOUSEINPUT {
    dx: i32,
    dy: i32,
    data: u32,
    flags: u32,
    time: u32,
    extra: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KEYBDINPUT {
    vk: u16,
    scan: u16,
    flags: u32,
    time: u32,
    extra: usize,
}

#[repr(C)]
union InputData {
    mi: MOUSEINPUT,
    ki: KEYBDINPUT,
}

#[repr(C)]
struct INPUT {
    kind: u32,
    data: InputData,
}

#[repr(C)]
struct LASTINPUTINFO {
    size: u32,
    time: u32,
}

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(hwnd: HANDLE, text: *const u16, caption: *const u16, kind: u32) -> i32;
    fn EnumWindows(cb: extern "system" fn(HANDLE, isize) -> BOOL, lparam: isize) -> BOOL;
    fn GetWindowThreadProcessId(hwnd: HANDLE, pid: *mut DWORD) -> DWORD;
    fn IsWindowVisible(hwnd: HANDLE) -> BOOL;
    fn GetWindow(hwnd: HANDLE, cmd: u32) -> HANDLE;
    fn GetWindowTextW(hwnd: HANDLE, buf: *mut u16, max: i32) -> i32;
    fn SendMessageTimeoutW(
        hwnd: HANDLE,
        msg: u32,
        wparam: usize,
        lparam: isize,
        flags: u32,
        timeout_ms: u32,
        result: *mut usize,
    ) -> isize;
    fn ShowWindowAsync(hwnd: HANDLE, cmd: i32) -> BOOL;
    fn IsIconic(hwnd: HANDLE) -> BOOL;
    fn GetForegroundWindow() -> HANDLE;
    fn SetForegroundWindow(hwnd: HANDLE) -> BOOL;
    fn BringWindowToTop(hwnd: HANDLE) -> BOOL;
    fn AttachThreadInput(attach: DWORD, to: DWORD, on: BOOL) -> BOOL;
    fn SendInput(count: u32, inputs: *const INPUT, size: i32) -> u32;
    fn GetLastInputInfo(info: *mut LASTINPUTINFO) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> DWORD;
    fn GetTickCount() -> DWORD;
    fn SetThreadExecutionState(flags: u32) -> u32;
}

pub fn message_box(title: &str, text: &str) {
    let (t, c) = (wide(text), wide(title));
    unsafe { MessageBoxW(null_mut(), t.as_ptr(), c.as_ptr(), 0x40) };
}

extern "system" fn collect_window(hwnd: HANDLE, lparam: isize) -> BOOL {
    let (pid, found) = unsafe { &mut *(lparam as *mut (DWORD, Vec<HANDLE>)) };
    let mut owner = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut owner);
        if owner == *pid && IsWindowVisible(hwnd) != 0 && GetWindow(hwnd, GW_OWNER).is_null() {
            found.push(hwnd);
        }
    }
    1
}

/// Visible top-level windows of a process.
fn windows_of(pid: u32) -> Vec<HANDLE> {
    let mut state: (DWORD, Vec<HANDLE>) = (pid, Vec::new());
    unsafe { EnumWindows(collect_window, &mut state as *mut _ as isize) };
    state.1
}

fn window_text(hwnd: HANDLE) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Renames a client's window. Uses a timeout so a client that is still loading can't block us.
pub fn set_client_title(pid: u32, title: &str) {
    let w = wide(title);
    for hwnd in windows_of(pid) {
        if window_text(hwnd) != title {
            let mut result = 0usize;
            unsafe {
                SendMessageTimeoutW(hwnd, WM_SETTEXT, 0, w.as_ptr() as isize, SMTO_ABORTIFHUNG, 500, &mut result)
            };
        }
    }
}

/// Returns how many windows were minimized; 0 when the client has no window yet.
pub fn minimize_client(pid: u32) -> usize {
    let windows = windows_of(pid);
    for &hwnd in &windows {
        unsafe { ShowWindowAsync(hwnd, SW_MINIMIZE) };
    }
    windows.len()
}

const HKEY_CURRENT_USER: usize = 0x8000_0001;
const KEY_QUERY_VALUE: u32 = 0x1;
const KEY_SET_VALUE: u32 = 0x2;
const REG_SZ: u32 = 1;
const ERROR_FILE_NOT_FOUND: i32 = 2;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "MultiRoblox";

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(key: HANDLE, sub: *const u16, options: u32, access: u32, out: *mut HANDLE) -> i32;
    fn RegSetValueExW(key: HANDLE, name: *const u16, reserved: u32, kind: u32, data: *const u8, len: u32) -> i32;
    fn RegQueryValueExW(
        key: HANDLE,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        len: *mut u32,
    ) -> i32;
    fn RegDeleteValueW(key: HANDLE, name: *const u16) -> i32;
    fn RegCloseKey(key: HANDLE) -> i32;
}

/// The current user's "run at sign-in" registry key, the list Task Manager's Startup tab shows.
fn run_key(access: u32) -> Option<HANDLE> {
    let sub = wide(RUN_KEY);
    let mut key: HANDLE = null_mut();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER as HANDLE, sub.as_ptr(), 0, access, &mut key) };
    (r == 0).then_some(key)
}

pub fn starts_with_windows() -> bool {
    let Some(key) = run_key(KEY_QUERY_VALUE) else { return false };
    let name = wide(RUN_VALUE);
    let mut len = 0u32;
    let r = unsafe { RegQueryValueExW(key, name.as_ptr(), null_mut(), null_mut(), null_mut(), &mut len) };
    unsafe { RegCloseKey(key) };
    r == 0
}

/// Adds or removes Multi Roblox from the programs Windows starts at sign-in. It starts minimized.
pub fn set_start_with_windows(on: bool) -> bool {
    let Some(key) = run_key(KEY_SET_VALUE) else { return false };
    let name = wide(RUN_VALUE);
    let ok = if on {
        let Ok(exe) = std::env::current_exe() else {
            unsafe { RegCloseKey(key) };
            return false;
        };
        let cmd = wide(&format!("\"{}\" --startup", exe.display()));
        let bytes = (cmd.len() * 2) as u32;
        unsafe { RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, cmd.as_ptr() as *const u8, bytes) == 0 }
    } else {
        let r = unsafe { RegDeleteValueW(key, name.as_ptr()) };
        r == 0 || r == ERROR_FILE_NOT_FOUND
    };
    unsafe { RegCloseKey(key) };
    ok
}

/// Seconds since the user last touched the keyboard or mouse.
pub fn user_idle_secs() -> u32 {
    let mut info = LASTINPUTINFO { size: size_of::<LASTINPUTINFO>() as u32, time: 0 };
    unsafe {
        if GetLastInputInfo(&mut info) == 0 {
            return 0;
        }
        GetTickCount().wrapping_sub(info.time) / 1000
    }
}

/// Stops Windows from sleeping while `on`. Must be called from a thread that lives as long as the app.
pub fn keep_awake(on: bool) {
    let flags = if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS };
    unsafe { SetThreadExecutionState(flags) };
}

/// Brings a window to the front. Windows only lets the foreground app hand over focus, so we
/// briefly share input state with it.
fn focus(hwnd: HANDLE) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg == hwnd {
            return true;
        }
        let fg_thread = GetWindowThreadProcessId(fg, null_mut());
        let me = GetCurrentThreadId();
        let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, 1) != 0;
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        if attached {
            AttachThreadInput(me, fg_thread, 0);
        }
        GetForegroundWindow() == hwnd
    }
}

fn key(scan: u16, up: bool) -> INPUT {
    let flags = KEYEVENTF_SCANCODE | if up { KEYEVENTF_KEYUP } else { 0 };
    INPUT { kind: INPUT_KEYBOARD, data: InputData { ki: KEYBDINPUT { vk: 0, scan, flags, time: 0, extra: 0 } } }
}

fn mouse_move(dx: i32, dy: i32) -> INPUT {
    let mi = MOUSEINPUT { dx, dy, data: 0, flags: MOUSEEVENTF_MOVE, time: 0, extra: 0 };
    INPUT { kind: INPUT_MOUSE, data: InputData { mi } }
}

fn send(input: INPUT) {
    unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
}

fn pause(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

/// Gives each client's window a moment of input so Roblox doesn't count the player as idle, then
/// puts back the window that was in front. Input is only sent while a Roblox window really has
/// focus, so it can never land in another program. Returns how many windows were nudged.
pub fn nudge_clients(pids: &[u32], jump: bool) -> usize {
    let previous = unsafe { GetForegroundWindow() };
    let mut nudged = 0;
    for &pid in pids {
        for hwnd in windows_of(pid) {
            let minimized = unsafe { IsIconic(hwnd) } != 0;
            if minimized {
                unsafe { ShowWindowAsync(hwnd, SW_RESTORE) };
                pause(300);
            }
            if focus(hwnd) {
                pause(150);
                if unsafe { GetForegroundWindow() } == hwnd {
                    if jump {
                        send(key(SCAN_SPACE, false));
                        pause(60);
                        send(key(SCAN_SPACE, true));
                    } else {
                        send(mouse_move(12, 0));
                        pause(40);
                        send(mouse_move(-12, 0));
                    }
                    pause(100);
                    nudged += 1;
                }
            }
            if minimized {
                unsafe { ShowWindowAsync(hwnd, SW_MINIMIZE) };
            }
        }
    }
    if !previous.is_null() {
        focus(previous);
    }
    nudged
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
