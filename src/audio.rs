//! Mutes clients in the Windows volume mixer. Each client has its own audio session per output
//! device, found by process id.

use windows::core::Interface;
use windows::Win32::Media::Audio::{
    eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
    MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

/// Sets mute on the audio sessions of the given processes, on every active output device.
/// A client only has a session once it has played sound, so callers repeat this.
pub fn set_mute(targets: &[(u32, bool)]) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = apply(targets);
    }
}

unsafe fn apply(targets: &[(u32, bool)]) -> windows::core::Result<()> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
    for d in 0..devices.GetCount()? {
        let Ok(manager) = devices.Item(d)?.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
            continue;
        };
        let sessions = manager.GetSessionEnumerator()?;
        for s in 0..sessions.GetCount()? {
            let Ok(control) = sessions.GetSession(s) else { continue };
            let Ok(pid) = control.cast::<IAudioSessionControl2>().and_then(|c| c.GetProcessId()) else {
                continue;
            };
            if let Some(&(_, mute)) = targets.iter().find(|(p, _)| *p == pid) {
                if let Ok(volume) = control.cast::<ISimpleAudioVolume>() {
                    let _ = volume.SetMute(mute, std::ptr::null());
                }
            }
        }
    }
    Ok(())
}
