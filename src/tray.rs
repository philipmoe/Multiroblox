//! A tray icon, used to show Windows notifications: joins with the server's location, and friends
//! who start playing. It lives on its own thread with a hidden message-only window.

#![allow(non_snake_case)]

use crate::multi::wide;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

type HANDLE = *mut c_void;

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIF_ICON: u32 = 0x2;
const NIF_TIP: u32 = 0x4;
const NIF_INFO: u32 = 0x10;
const NIIF_USER: u32 = 0x4;
const NIIF_LARGE_ICON: u32 = 0x20;
const PM_REMOVE: u32 = 1;
/// Parent for message-only windows, which never receive broadcast messages.
const HWND_MESSAGE: isize = -3;

#[repr(C)]
struct NOTIFYICONDATAW {
    cb_size: u32,
    hwnd: HANDLE,
    id: u32,
    flags: u32,
    callback_message: u32,
    icon: HANDLE,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    timeout_or_version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: [u32; 4],
    balloon_icon: HANDLE,
}

#[repr(C)]
struct MSG {
    hwnd: HANDLE,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: [i32; 2],
    private: u32,
}

#[link(name = "shell32")]
extern "system" {
    fn Shell_NotifyIconW(message: u32, data: *mut NOTIFYICONDATAW) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: HANDLE,
        menu: HANDLE,
        instance: HANDLE,
        param: *const c_void,
    ) -> HANDLE;
    fn PeekMessageW(msg: *mut MSG, hwnd: HANDLE, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(msg: *const MSG) -> i32;
    fn DispatchMessageW(msg: *const MSG) -> isize;
    fn CreateIcon(
        instance: HANDLE,
        w: i32,
        h: i32,
        planes: u8,
        bits_per_pixel: u8,
        and_bits: *const u8,
        xor_bits: *const u8,
    ) -> HANDLE;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> HANDLE;
}

fn copy_into(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

/// Makes a Windows icon from RGBA pixels.
fn make_icon(rgba: &[u8], size: u32) -> HANDLE {
    let bgra: Vec<u8> = rgba.chunks(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
    // The AND mask is unused for 32-bit icons (alpha decides), but must be there.
    let mask = vec![0u8; (size * size / 8) as usize];
    unsafe {
        CreateIcon(GetModuleHandleW(null()), size as i32, size as i32, 1, 32, mask.as_ptr(), bgra.as_ptr())
    }
}

enum Cmd {
    Notify(String, String),
    Quit,
}

pub struct Tray {
    tx: Sender<Cmd>,
}

impl Tray {
    pub fn start() -> Tray {
        let (tx, rx) = channel();
        thread::spawn(move || unsafe {
            let class = wide("STATIC");
            let name = wide("Multi Roblox");
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE as HANDLE,
                null_mut(),
                null_mut(),
                null(),
            );
            let mut data: NOTIFYICONDATAW = std::mem::zeroed();
            data.cb_size = size_of::<NOTIFYICONDATAW>() as u32;
            data.hwnd = hwnd;
            data.id = 1;
            data.flags = NIF_ICON | NIF_TIP;
            data.icon = make_icon(&crate::icon::rgba(32), 32);
            copy_into(&mut data.tip, "Multi Roblox");
            Shell_NotifyIconW(NIM_ADD, &mut data);

            loop {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Cmd::Notify(title, body)) => {
                        data.flags = NIF_ICON | NIF_TIP | NIF_INFO;
                        data.info_flags = NIIF_USER | NIIF_LARGE_ICON;
                        copy_into(&mut data.info_title, &title);
                        copy_into(&mut data.info, &body);
                        Shell_NotifyIconW(NIM_MODIFY, &mut data);
                    }
                    Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => {
                        Shell_NotifyIconW(NIM_DELETE, &mut data);
                        return;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        });
        Tray { tx }
    }

    pub fn notify(&self, title: impl Into<String>, body: impl Into<String>) {
        let _ = self.tx.send(Cmd::Notify(title.into(), body.into()));
    }

    /// Removes the tray icon; otherwise it lingers until the mouse passes over it.
    pub fn close(&self) {
        let _ = self.tx.send(Cmd::Quit);
        thread::sleep(Duration::from_millis(250));
    }
}
