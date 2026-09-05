use std::mem::size_of;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

const GAME_PROCESS_NAME: &str = "helldivers2.exe";

pub(super) fn ensure_input_access() -> Result<()> {
    let helper_is_elevated = process_is_elevated(unsafe { GetCurrentProcess() })
        .context("failed to inspect HD2 Preset Helper permissions")?;
    if helper_is_elevated {
        return Ok(());
    }

    let process_id = find_game_process_id().context("failed to identify the Helldivers process")?;
    let process = OwnedHandle(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }
            .context("failed to open the Helldivers process for permission inspection")?,
    );

    if process_is_elevated(process.0).context("failed to inspect Helldivers permissions")? {
        bail!(
            "Helldivers is running with higher privileges. Restart HD2 Preset Helper as administrator, or run the game normally."
        );
    }

    Ok(())
}

fn find_game_process_id() -> Result<u32> {
    let snapshot = OwnedHandle(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .context("CreateToolhelp32Snapshot failed")?,
    );
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe { Process32FirstW(snapshot.0, &mut entry) }.context("Process32FirstW failed")?;

    loop {
        let name_len = entry
            .szExeFile
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(entry.szExeFile.len());
        if String::from_utf16_lossy(&entry.szExeFile[..name_len])
            .eq_ignore_ascii_case(GAME_PROCESS_NAME)
        {
            return Ok(entry.th32ProcessID);
        }
        if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
            bail!("process {GAME_PROCESS_NAME:?} is not running");
        }
    }
}

fn process_is_elevated(process: HANDLE) -> Result<bool> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }
        .context("OpenProcessToken failed")?;
    let token = OwnedHandle(token);

    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned_size = 0;
    unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned_size,
        )
    }
    .context("GetTokenInformation(TokenElevation) failed")?;
    Ok(elevation.TokenIsElevated != 0)
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
