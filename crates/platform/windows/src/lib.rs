use std::io;

#[cfg(windows)]
pub struct WindowsIpcListener {
    path: String,
    /// The next server instance that is already created and waiting for a client.
    pending: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
    is_first: bool,
}

#[cfg(windows)]
impl platform_traits::IpcListener for WindowsIpcListener {
    type Stream = tokio::net::windows::named_pipe::NamedPipeServer;

    async fn accept(&mut self) -> io::Result<Self::Stream> {
        // Take the pre-created instance (or create the very first one).
        let server = match self.pending.take() {
            Some(s) => s,
            None => tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(self.is_first)
                .create(&self.path)?,
        };
        self.is_first = false;

        // Pre-create the NEXT instance immediately so new clients never get
        // ERROR_PIPE_BUSY (231) while we are blocked waiting on this one.
        let next = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(false)
            .create(&self.path)?;
        self.pending = Some(next);

        // Now wait for a client to connect to the current instance.
        server.connect().await?;
        Ok(server)
    }
}

#[cfg(windows)]
pub async fn create_listener(path: &str) -> io::Result<WindowsIpcListener> {
    Ok(WindowsIpcListener {
        path: path.to_string(),
        pending: None,
        is_first: true,
    })
}

#[cfg(windows)]
pub type PlatformStream = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(windows)]
pub type PlatformClientStream = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(windows)]
pub async fn connect_client(path: &str) -> io::Result<PlatformClientStream> {
    use std::time::Duration;
    let start = std::time::Instant::now();
    // Back-off: start at 50ms, increase by 50ms per attempt, cap at 250ms.
    // Total retry window: 10 seconds. This covers the case where the daemon
    // is briefly busy processing another request (rebuild chunk, drain batch commit).
    let mut delay_ms = 50u64;
    loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(path) {
            Ok(client) => return Ok(client),
            Err(e) if e.raw_os_error() == Some(231) => {
                // ERROR_PIPE_BUSY
                if start.elapsed() > Duration::from_secs(10) {
                    return Err(e);
                }
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = (delay_ms + 50).min(250);
            }
            Err(e) => return Err(e),
        }
    }
}

// Unix (WSL) fallback implementation using Unix Domain Sockets
#[cfg(not(windows))]
pub struct UnixIpcListener {
    listener: tokio::net::UnixListener,
}

#[cfg(not(windows))]
impl platform_traits::IpcListener for UnixIpcListener {
    type Stream = tokio::net::UnixStream;

    async fn accept(&mut self) -> io::Result<Self::Stream> {
        let (stream, _) = self.listener.accept().await?;
        Ok(stream)
    }
}

#[cfg(not(windows))]
pub type PlatformStream = tokio::net::UnixStream;
#[cfg(not(windows))]
pub type PlatformClientStream = tokio::net::UnixStream;

#[cfg(not(windows))]
pub async fn create_listener(path: &str) -> io::Result<UnixIpcListener> {
    let sock_path = map_pipe_to_socket(path);
    // Remove the socket file if it already exists
    let _ = std::fs::remove_file(&sock_path);

    // Ensure parent directory exists
    if let Some(parent) = std::path::Path::new(&sock_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let listener = tokio::net::UnixListener::bind(sock_path)?;
    Ok(UnixIpcListener { listener })
}

#[cfg(not(windows))]
pub async fn connect_client(path: &str) -> io::Result<PlatformClientStream> {
    let sock_path = map_pipe_to_socket(path);
    tokio::net::UnixStream::connect(sock_path).await
}

#[cfg(not(windows))]
fn map_pipe_to_socket(pipe_path: &str) -> String {
    // Maps Windows Named Pipe path format: e.g. "\\\\.\\pipe\\disktracker" -> "/tmp/disktracker.sock"
    if let Some(name) = pipe_path.strip_prefix(r"\\.\pipe\") {
        format!("/tmp/{}.sock", name)
    } else {
        "/tmp/disktracker.sock".to_string()
    }
}

// =========================================================================
// NTFS / USN Journal support
// =========================================================================

#[cfg(windows)]
mod win32 {
    use std::ffi::c_void;

    pub const DRIVE_FIXED: u32 = 3;
    pub const GENERIC_READ: u32 = 0x80000000;
    pub const FILE_SHARE_READ: u32 = 1;
    pub const FILE_SHARE_WRITE: u32 = 2;
    pub const OPEN_EXISTING: u32 = 3;
    pub const FSCTL_QUERY_USN_JOURNAL: u32 = 0x000900F4;
    pub const FSCTL_READ_USN_JOURNAL: u32 = 0x000900BB;

    extern "system" {
        pub fn GetDriveTypeW(lprootpathname: *const u16) -> u32;
        pub fn GetLogicalDriveStringsW(nbufferlength: u32, lpbuffer: *mut u16) -> u32;
        pub fn GetVolumeInformationW(
            lprootpathname: *const u16,
            lpvolumenamebuffer: *mut u16,
            nvolumenamesize: u32,
            lpvolumeserialnumber: *mut u32,
            lpmaximumcomponentlength: *mut u32,
            lpfilesystemflags: *mut u32,
            lpfilesystemnamebuffer: *mut u16,
            nfilesystemnamesize: u32,
        ) -> i32;
        pub fn CreateFileW(
            lpfilename: *const u16,
            dwdesiredaccess: u32,
            dwsharemode: u32,
            lpsecurityattributes: *const c_void,
            dwcreationdisposition: u32,
            dwflagsandattributes: u32,
            htemplatefile: isize,
        ) -> isize;
        pub fn DeviceIoControl(
            hdevice: isize,
            dwiocontrolcode: u32,
            lpinbuffer: *const c_void,
            ninbuffersize: u32,
            lpoutbuffer: *mut c_void,
            noutbuffersize: u32,
            lpbytesreturned: *mut u32,
            lpoverlapped: *mut c_void,
        ) -> i32;
        pub fn CloseHandle(hobject: isize) -> i32;
    }
}

#[cfg(windows)]
pub fn detect_ntfs_volumes() -> Vec<String> {
    use win32::{GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW, DRIVE_FIXED};

    let mut volumes = Vec::new();
    let mut buffer = [0u16; 512];
    unsafe {
        let len = GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr());
        if len > 0 && len < buffer.len() as u32 {
            let mut slice = &buffer[..len as usize];
            while !slice.is_empty() {
                if let Some(pos) = slice.iter().position(|&c| c == 0) {
                    let path_u16 = &slice[..pos];
                    slice = &slice[pos + 1..];

                    if path_u16.is_empty() {
                        continue;
                    }

                    // Win32 API expects a null-terminated string
                    let mut path_null_terminated = path_u16.to_vec();
                    path_null_terminated.push(0);

                    let drive_type = GetDriveTypeW(path_null_terminated.as_ptr());
                    if drive_type == DRIVE_FIXED {
                        let mut fs_name = [0u16; 256];
                        if GetVolumeInformationW(
                            path_null_terminated.as_ptr(),
                            std::ptr::null_mut(),
                            0,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            fs_name.as_mut_ptr(),
                            fs_name.len() as u32,
                        ) != 0
                        {
                            let fs_str = String::from_utf16_lossy(&fs_name)
                                .trim_end_matches('\0')
                                .to_string();
                            if fs_str == "NTFS" {
                                let drive_str = String::from_utf16_lossy(path_u16)
                                    .trim_end_matches('\0')
                                    .trim_end_matches('\\')
                                    .to_string();
                                volumes.push(drive_str);
                            }
                        }
                    }
                } else {
                    break;
                }
            }
        }
    }

    if volumes.is_empty() {
        volumes.push("C:".to_string());
    }
    volumes
}

#[cfg(not(windows))]
pub fn detect_ntfs_volumes() -> Vec<String> {
    vec!["C:".to_string(), "D:".to_string()]
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    pub fn OpenFileById(
        hvolumehint: windows_sys::Win32::Foundation::HANDLE,
        lpfileiddescriptor: *const windows_sys::Win32::Storage::FileSystem::FILE_ID_DESCRIPTOR,
        dwdesiredaccess: u32,
        dwsharemode: u32,
        lpsecurityattributes: *const std::ffi::c_void,
        dwflagsandattributes: u32,
    ) -> windows_sys::Win32::Foundation::HANDLE;
}

#[cfg(windows)]
static VOLUME_HANDLES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, usize>>,
> = std::sync::OnceLock::new();

#[cfg(windows)]
fn get_volume_handle(volume: &str) -> Option<usize> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };

    let cache =
        VOLUME_HANDLES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut map = cache.lock().unwrap();
    if let Some(&handle) = map.get(volume) {
        return Some(handle);
    }

    unsafe {
        let vol_path = format!("\\\\.\\{}", volume);
        let vol_path_w: Vec<u16> = std::ffi::OsStr::new(&vol_path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let handle = win32::CreateFileW(
            vol_path_w.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            0,
        );

        if handle == -1 {
            None
        } else {
            map.insert(volume.to_string(), handle as usize);
            Some(handle as usize)
        }
    }
}

#[cfg(windows)]
pub fn get_file_info_by_id(volume: &str, file_id: u64) -> Option<(u64, u32)> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdType, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };

    let vol_handle = get_volume_handle(volume)?;

    unsafe {
        let mut desc = std::mem::zeroed::<FILE_ID_DESCRIPTOR>();
        desc.dwSize = std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32;
        desc.Type = FileIdType;
        desc.Anonymous.FileId = file_id as i64;

        let file_handle = OpenFileById(
            vol_handle as HANDLE,
            &desc,
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            FILE_FLAG_BACKUP_SEMANTICS,
        );

        if file_handle == INVALID_HANDLE_VALUE || file_handle == 0 {
            return None;
        }

        let mut info = std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>();
        let success = GetFileInformationByHandle(file_handle, &mut info);
        CloseHandle(file_handle);

        if success != 0 {
            let size = ((info.nFileSizeHigh as u64) << 32) | (info.nFileSizeLow as u64);
            let attrs = info.dwFileAttributes;
            Some((size, attrs))
        } else {
            None
        }
    }
}

#[cfg(windows)]
pub fn get_file_size_by_id(volume: &str, file_id: u64) -> Option<u64> {
    get_file_info_by_id(volume, file_id).map(|(size, _)| size)
}

#[cfg(not(windows))]
pub fn get_file_info_by_id(_volume: &str, _file_id: u64) -> Option<(u64, u32)> {
    None
}

#[cfg(not(windows))]
pub fn get_file_size_by_id(_volume: &str, _file_id: u64) -> Option<u64> {
    None
}

#[cfg(windows)]
pub fn ensure_usn_journal_active(volume: &str) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use win32::{
        CloseHandle, CreateFileW, DeviceIoControl, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FSCTL_QUERY_USN_JOURNAL, GENERIC_READ, OPEN_EXISTING,
    };

    const FSCTL_CREATE_USN_JOURNAL: u32 = 0x00090057;

    #[repr(C)]
    struct CreateUsnJournalData {
        maximum_size: u64,
        allocation_delta: u64,
    }

    let vol_path = format!("\\\\.\\{}", volume);
    let vol_path_w: Vec<u16> = std::ffi::OsStr::new(&vol_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let handle = CreateFileW(
            vol_path_w.as_ptr(),
            GENERIC_READ | 0x40000000, // GENERIC_WRITE
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        );

        if handle == -1 {
            return Err(std::io::Error::last_os_error());
        }

        #[repr(C)]
        struct UsnJournalData {
            usn_journal_id: u64,
            first_usn: i64,
            next_usn: i64,
            lowest_valid_usn: i64,
            max_usn: i64,
            maximum_size: u64,
            allocation_delta: u64,
        }

        let mut journal_data = std::mem::zeroed::<UsnJournalData>();
        let mut bytes_returned = 0u32;
        let query_success = DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null(),
            0,
            &mut journal_data as *mut _ as *mut _,
            std::mem::size_of::<UsnJournalData>() as u32,
            &mut bytes_returned,
            std::ptr::null_mut(),
        );

        if query_success != 0 {
            CloseHandle(handle);
            return Ok(()); // Already active
        }

        let create_data = CreateUsnJournalData {
            maximum_size: 33554432,    // 32MB
            allocation_delta: 4194304, // 4MB
        };

        let create_success = DeviceIoControl(
            handle,
            FSCTL_CREATE_USN_JOURNAL,
            &create_data as *const _ as *const _,
            std::mem::size_of::<CreateUsnJournalData>() as u32,
            std::ptr::null_mut(),
            0,
            &mut bytes_returned,
            std::ptr::null_mut(),
        );

        CloseHandle(handle);

        if create_success != 0 {
            println!(
                "[USN] Successfully activated/started USN journal for {}",
                volume
            );
            return Ok(());
        }
    }

    // Fallback to fsutil command
    let output = std::process::Command::new("fsutil")
        .args(["usn", "createjournal", "m=33554432", "a=4194304", volume])
        .output();

    match output {
        Ok(out) => {
            if out.status.success() {
                println!(
                    "[USN] fsutil successfully activated USN journal for {}",
                    volume
                );
                Ok(())
            } else {
                let err_msg = String::from_utf8_lossy(&out.stderr).to_string();
                Err(std::io::Error::other(format!(
                    "fsutil failed to create USN journal: {}",
                    err_msg
                )))
            }
        }
        Err(e) => Err(std::io::Error::other(format!(
            "Failed to execute fsutil fallback: {}",
            e
        ))),
    }
}

#[cfg(not(windows))]
pub fn ensure_usn_journal_active(_volume: &str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn get_usn_cursor(volume: &str) -> std::io::Result<u64> {
    let _ = ensure_usn_journal_active(volume);
    use std::os::windows::ffi::OsStrExt;
    use win32::{
        CloseHandle, CreateFileW, DeviceIoControl, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FSCTL_QUERY_USN_JOURNAL, GENERIC_READ, OPEN_EXISTING,
    };

    let vol_path = format!("\\\\.\\{}", volume);
    let vol_path_w: Vec<u16> = std::ffi::OsStr::new(&vol_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let handle = CreateFileW(
            vol_path_w.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        );

        if handle == -1 {
            return Err(std::io::Error::last_os_error());
        }

        #[repr(C)]
        struct UsnJournalData {
            usn_journal_id: u64,
            first_usn: i64,
            next_usn: i64,
            lowest_valid_usn: i64,
            max_usn: i64,
            maximum_size: u64,
            allocation_delta: u64,
        }

        let mut journal_data = std::mem::zeroed::<UsnJournalData>();
        let mut bytes_returned = 0u32;

        let success = DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null(),
            0,
            &mut journal_data as *mut _ as *mut _,
            std::mem::size_of::<UsnJournalData>() as u32,
            &mut bytes_returned,
            std::ptr::null_mut(),
        );

        CloseHandle(handle);

        if success == 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(journal_data.next_usn as u64)
    }
}

#[cfg(not(windows))]
pub fn get_usn_cursor(_volume: &str) -> std::io::Result<u64> {
    Ok(0)
}

#[cfg(windows)]
pub async fn watch_usn_journal(volume: &str, start_usn: u64) -> std::io::Result<()> {
    let _ = ensure_usn_journal_active(volume);
    use std::os::windows::ffi::OsStrExt;
    use win32::{
        CloseHandle, CreateFileW, DeviceIoControl, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, GENERIC_READ, OPEN_EXISTING,
    };

    const USN_REASON_FILE_CREATE: u32 = 0x00000100;
    const USN_REASON_FILE_DELETE: u32 = 0x00000200;
    const USN_REASON_RENAME_OLD_NAME: u32 = 0x00001000;
    const USN_REASON_RENAME_NEW_NAME: u32 = 0x00002000;

    let mut conn = match storage::get_db_connection() {
        Ok(c) => c,
        Err(e) => return Err(std::io::Error::other(e.to_string())),
    };

    let tracker = core_types::get_volume_tracker(volume);
    tracker
        .usn_start
        .store(start_usn, std::sync::atomic::Ordering::Relaxed);

    println!(
        "[Watcher - {}] Initializing USN journal watch from {}",
        volume, start_usn
    );

    let vol_path = format!("\\\\.\\{}", volume);
    let vol_path_w: Vec<u16> = std::ffi::OsStr::new(&vol_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let handle = CreateFileW(
            vol_path_w.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        );

        println!("[Watcher - {}] CreateFileW handle: {}", volume, handle);
        if handle == -1 {
            let err = std::io::Error::last_os_error();
            eprintln!("[Watcher - {}] CreateFileW failed: {:?}", volume, err);
            return Err(err);
        }

        #[repr(C)]
        struct UsnJournalData {
            usn_journal_id: u64,
            first_usn: i64,
            next_usn: i64,
            lowest_valid_usn: i64,
            max_usn: i64,
            maximum_size: u64,
            allocation_delta: u64,
        }

        let mut journal_data = std::mem::zeroed::<UsnJournalData>();
        let mut bytes_returned = 0u32;
        let success = DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null(),
            0,
            &mut journal_data as *mut _ as *mut _,
            std::mem::size_of::<UsnJournalData>() as u32,
            &mut bytes_returned,
            std::ptr::null_mut(),
        );

        println!(
            "[Watcher - {}] FSCTL_QUERY_USN_JOURNAL success: {}, journal_id: {}",
            volume, success, journal_data.usn_journal_id
        );
        if success == 0 {
            let err = std::io::Error::last_os_error();
            eprintln!(
                "[Watcher - {}] FSCTL_QUERY_USN_JOURNAL failed: {:?}",
                volume, err
            );
            CloseHandle(handle);
            return Err(err);
        }

        #[repr(C)]
        struct ReadUsnJournalData {
            start_usn: i64,
            reason_mask: u32,
            return_only_on_close: u32,
            timeout: u64,
            bytes_to_wait: u64,
            usn_journal_id: u64,
            min_major_version: u16,
            max_major_version: u16,
        }

        let mut read_data = ReadUsnJournalData {
            start_usn: start_usn as i64,
            reason_mask: 0xFFFFFFFF,
            return_only_on_close: 0,
            timeout: 0,
            bytes_to_wait: 0,
            usn_journal_id: journal_data.usn_journal_id,
            min_major_version: 2,
            max_major_version: 3,
        };

        let mut buffer = vec![0u8; 8192];

        println!("[Watcher - {}] Starting main USN read loop", volume);

        loop {
            let mut bytes_returned = 0u32;
            let success = DeviceIoControl(
                handle,
                FSCTL_READ_USN_JOURNAL,
                &read_data as *const _ as *const _,
                std::mem::size_of::<ReadUsnJournalData>() as u32,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as u32,
                &mut bytes_returned,
                std::ptr::null_mut(),
            );

            if success == 0 {
                let err = std::io::Error::last_os_error();
                eprintln!("[Watcher - {}] Error reading journal: {:?}", volume, err);
                break;
            }

            if bytes_returned <= 8 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }

            let mut offset = 8usize;
            let next_usn = u64::from_ne_bytes(buffer[0..8].try_into().unwrap());
            read_data.start_usn = next_usn as i64;

            let tx_res = (|| {
                let tx =
                    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                {
                    let mut stmt = tx.prepare_cached(
                        "INSERT INTO mutation_log (volume, file_id, parent_file_id, name, kind, is_directory, size_delta, at, source)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
                    )?;

                    while offset < bytes_returned as usize {
                        if offset + 4 > bytes_returned as usize {
                            break;
                        }

                        let record_len =
                            u32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap())
                                as usize;
                        if record_len == 0 {
                            break;
                        }

                        if offset + record_len > bytes_returned as usize {
                            break;
                        }

                        let major_version =
                            u16::from_ne_bytes(buffer[offset + 4..offset + 6].try_into().unwrap());
                        if major_version == 2 {
                            let file_ref = u64::from_ne_bytes(
                                buffer[offset + 8..offset + 16].try_into().unwrap(),
                            );
                            let parent_ref = u64::from_ne_bytes(
                                buffer[offset + 16..offset + 24].try_into().unwrap(),
                            );
                            let record_usn = i64::from_ne_bytes(
                                buffer[offset + 24..offset + 32].try_into().unwrap(),
                            );
                            let reason = u32::from_ne_bytes(
                                buffer[offset + 40..offset + 44].try_into().unwrap(),
                            );
                            let file_attributes = u32::from_ne_bytes(
                                buffer[offset + 48..offset + 52].try_into().unwrap(),
                            );

                            let name_len = u16::from_ne_bytes(
                                buffer[offset + 52..offset + 54].try_into().unwrap(),
                            ) as usize;
                            let name_offset = u16::from_ne_bytes(
                                buffer[offset + 54..offset + 56].try_into().unwrap(),
                            ) as usize;

                            let name_start = offset + name_offset;
                            let name_end = name_start + name_len;
                            let name_bytes = &buffer[name_start..name_end];

                            let name_u16: Vec<u16> = name_bytes
                                .chunks_exact(2)
                                .map(|chunk| u16::from_ne_bytes(chunk.try_into().unwrap()))
                                .collect();
                            let name_str = String::from_utf16_lossy(&name_u16);

                            println!(
                                "[Watcher - {}] USN: {}, Name: {}, FileRef: {}, ParentRef: {}, Reason: {:#X}, Attr: {:#X}",
                                volume, record_usn, name_str, file_ref, parent_ref, reason, file_attributes
                            );

                            let kind = if (reason & USN_REASON_FILE_DELETE) != 0 {
                                "Deleted"
                            } else if (reason & USN_REASON_FILE_CREATE) != 0 {
                                "Created"
                            } else if (reason
                                & (USN_REASON_RENAME_OLD_NAME | USN_REASON_RENAME_NEW_NAME))
                                != 0
                            {
                                "Renamed"
                            } else {
                                "Modified"
                            };

                            let is_dir = (file_attributes & 0x10) != 0;
                            let timestamp_str = chrono::Utc::now().to_rfc3339();

                            tracker
                                .events_buffered
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                            stmt.execute(rusqlite::params![
                                volume,
                                file_ref,
                                parent_ref,
                                name_str,
                                kind,
                                if is_dir { 1 } else { 0 },
                                0, // size_delta
                                timestamp_str,
                                "Watcher",
                            ])?;
                        } else if major_version == 3 {
                            let file_ref = u64::from_ne_bytes(
                                buffer[offset + 8..offset + 16].try_into().unwrap(),
                            );
                            let parent_ref = u64::from_ne_bytes(
                                buffer[offset + 24..offset + 32].try_into().unwrap(),
                            );
                            let record_usn = i64::from_ne_bytes(
                                buffer[offset + 40..offset + 48].try_into().unwrap(),
                            );
                            let reason = u32::from_ne_bytes(
                                buffer[offset + 56..offset + 60].try_into().unwrap(),
                            );
                            let file_attributes = u32::from_ne_bytes(
                                buffer[offset + 68..offset + 72].try_into().unwrap(),
                            );

                            let name_len = u16::from_ne_bytes(
                                buffer[offset + 72..offset + 74].try_into().unwrap(),
                            ) as usize;
                            let name_offset = u16::from_ne_bytes(
                                buffer[offset + 74..offset + 76].try_into().unwrap(),
                            ) as usize;

                            let name_start = offset + name_offset;
                            let name_end = name_start + name_len;
                            let name_bytes = &buffer[name_start..name_end];

                            let name_u16: Vec<u16> = name_bytes
                                .chunks_exact(2)
                                .map(|chunk| u16::from_ne_bytes(chunk.try_into().unwrap()))
                                .collect();
                            let name_str = String::from_utf16_lossy(&name_u16);

                            println!(
                                "[Watcher - {}] USN: {}, Name: {}, FileRef: {} (V3), ParentRef: {} (V3), Reason: {:#X}, Attr: {:#X}",
                                volume, record_usn, name_str, file_ref, parent_ref, reason, file_attributes
                            );

                            let kind = if (reason & USN_REASON_FILE_DELETE) != 0 {
                                "Deleted"
                            } else if (reason & USN_REASON_FILE_CREATE) != 0 {
                                "Created"
                            } else if (reason
                                & (USN_REASON_RENAME_OLD_NAME | USN_REASON_RENAME_NEW_NAME))
                                != 0
                            {
                                "Renamed"
                            } else {
                                "Modified"
                            };

                            let is_dir = (file_attributes & 0x10) != 0;
                            let timestamp_str = chrono::Utc::now().to_rfc3339();

                            stmt.execute(rusqlite::params![
                                volume,
                                file_ref,
                                parent_ref,
                                name_str,
                                kind,
                                if is_dir { 1 } else { 0 },
                                0, // size_delta
                                timestamp_str,
                                "Watcher",
                            ])?;
                        } else {
                            println!(
                                "[Watcher - {}] Warning: Unsupported USN Record Version: {}",
                                volume, major_version
                            );
                        }

                        offset += record_len;
                    }
                }
                tx.commit()?;
                Ok::<(), rusqlite::Error>(())
            })();

            if let Err(e) = tx_res {
                eprintln!(
                    "[Watcher - {}] DB error inserting USN batch: {:?}",
                    volume, e
                );
            }
        }

        CloseHandle(handle);
    }
    Ok(())
}

#[cfg(not(windows))]
pub async fn watch_usn_journal(volume: &str, start_usn: u64) -> std::io::Result<()> {
    let conn = match storage::get_db_connection() {
        Ok(c) => c,
        Err(e) => return Err(std::io::Error::other(e.to_string())),
    };

    let tracker = core_types::get_volume_tracker(volume);
    tracker
        .usn_start
        .store(start_usn, std::sync::atomic::Ordering::Relaxed);

    // Query mock root directory's real file_id (inode) so facts foreign key constraint is satisfied
    let root_path = format!("/tmp/disktracker_mock_{}", volume);
    let parent_file_id = if let Ok(meta) = std::fs::metadata(&root_path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            meta.ino()
        }
        #[cfg(not(unix))]
        {
            0
        }
    } else {
        0
    };

    println!(
        "[Watcher - {}] (Mock) Started watching USN journal from {}",
        volume, start_usn
    );
    let mut event_count = 0;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        event_count += 1;
        let file_id = 1000 + event_count;
        let name_str = format!("test_file_{}.txt", event_count);
        let kind = "Created";
        let is_dir = false;
        let timestamp_str = chrono::Utc::now().to_rfc3339();

        println!(
            "[Watcher - {}] (Mock) USN: {}, Name: {}, FileRef: {}, ParentRef: {}, Reason: 0x1 (Created)",
            volume, start_usn + event_count, name_str, file_id, parent_file_id
        );

        tracker
            .events_buffered
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let _ = conn.execute(
            "INSERT INTO mutation_log (volume, file_id, parent_file_id, name, kind, is_directory, size_delta, at, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                volume,
                file_id,
                parent_file_id,
                name_str,
                kind,
                if is_dir { 1 } else { 0 },
                0, // size_delta
                timestamp_str,
                "Watcher",
            ],
        );
    }
}

#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct FastFileInfo {
    pub file_id: u64,
    pub parent_file_id: u64,
    pub name: String,
    pub is_directory: bool,
    pub size: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub modified_at: chrono::DateTime<chrono::Utc>,
    pub attributes: u32,
}

#[cfg(windows)]
fn filetime_to_datetime(filetime: i64) -> chrono::DateTime<chrono::Utc> {
    let secs = (filetime / 10_000_000) - 11_644_473_600;
    let nsecs = ((filetime % 10_000_000) * 100) as u32;
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nsecs).unwrap_or_default()
}

#[cfg(windows)]
fn walk_dir_recursive<F>(
    dir_path: &std::path::Path,
    parent_file_id: u64,
    volume: &str,
    callback: &mut F,
) -> io::Result<()>
where
    F: FnMut(FastFileInfo),
{
    use crate::win32::{CloseHandle, CreateFileW};
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NO_MORE_FILES};
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, GetFileInformationByHandleEx,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let dir_path_w: Vec<u16> = dir_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let handle = CreateFileW(
            dir_path_w.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            0,
        );

        if handle == -1 {
            return Err(io::Error::last_os_error());
        }

        const BUF_SIZE: usize = 65536;
        let mut buffer = vec![0u8; BUF_SIZE];

        let mut is_first = true;

        loop {
            let class = if is_first {
                FileIdBothDirectoryRestartInfo
            } else {
                FileIdBothDirectoryInfo
            };

            let success = GetFileInformationByHandleEx(
                handle,
                class,
                buffer.as_mut_ptr() as *mut c_void,
                BUF_SIZE as u32,
            );

            if success == 0 {
                let err = GetLastError();
                if err == ERROR_NO_MORE_FILES {
                    break;
                } else {
                    CloseHandle(handle);
                    return Err(io::Error::from_raw_os_error(err as i32));
                }
            }

            is_first = false;

            let mut offset = 0;
            loop {
                let entry_ptr = buffer.as_ptr().add(offset) as *const FILE_ID_BOTH_DIR_INFO;

                let next_entry_offset =
                    std::ptr::read_unaligned(std::ptr::addr_of!((*entry_ptr).NextEntryOffset));
                let file_attributes =
                    std::ptr::read_unaligned(std::ptr::addr_of!((*entry_ptr).FileAttributes));
                let file_id =
                    std::ptr::read_unaligned(std::ptr::addr_of!((*entry_ptr).FileId)) as u64;
                let creation_time =
                    std::ptr::read_unaligned(std::ptr::addr_of!((*entry_ptr).CreationTime));
                let last_write_time =
                    std::ptr::read_unaligned(std::ptr::addr_of!((*entry_ptr).LastWriteTime));
                let end_of_file =
                    std::ptr::read_unaligned(std::ptr::addr_of!((*entry_ptr).EndOfFile)) as u64;
                let file_name_length =
                    std::ptr::read_unaligned(std::ptr::addr_of!((*entry_ptr).FileNameLength))
                        as usize;

                let name_len = file_name_length / 2;
                let name_ptr = std::ptr::addr_of!((*entry_ptr).FileName) as *const u16;
                let name_slice = std::slice::from_raw_parts(name_ptr, name_len);
                let name = String::from_utf16_lossy(name_slice);

                if name != "." && name != ".." {
                    let created_at = filetime_to_datetime(creation_time);
                    let modified_at = filetime_to_datetime(last_write_time);
                    let is_reparse = (file_attributes
                        & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT)
                        != 0;
                    let is_dir = (file_attributes
                        & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY)
                        != 0;

                    let info = FastFileInfo {
                        file_id,
                        parent_file_id,
                        name: name.clone(),
                        is_directory: is_dir,
                        size: end_of_file,
                        created_at,
                        modified_at,
                        attributes: file_attributes,
                    };

                    callback(info);

                    if is_dir && !is_reparse {
                        let sub_path = dir_path.join(&name);
                        if let Err(e) = walk_dir_recursive(&sub_path, file_id, volume, callback) {
                            eprintln!(
                                "[Scanner - {}] Warning: failed to crawl subfolder {:?}: {:?}",
                                volume, sub_path, e
                            );
                        }
                    }
                }

                if next_entry_offset == 0 {
                    break;
                }
                offset += next_entry_offset as usize;
            }
        }

        CloseHandle(handle);
    }
    Ok(())
}

#[cfg(windows)]
pub fn walk_directory_fast<F>(root_path: &str, volume: &str, mut callback: F) -> io::Result<()>
where
    F: FnMut(FastFileInfo),
{
    use crate::win32::{CloseHandle, CreateFileW};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let path_w: Vec<u16> = std::path::Path::new(root_path)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let (root_file_id, root_attrs) = unsafe {
        let handle = CreateFileW(
            path_w.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            0,
        );

        if handle == -1 {
            return Err(io::Error::last_os_error());
        }

        let mut info = std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>();
        let success = GetFileInformationByHandle(handle, &mut info);
        CloseHandle(handle);

        if success != 0 {
            (
                ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64),
                info.dwFileAttributes,
            )
        } else {
            return Err(io::Error::last_os_error());
        }
    };

    let root_metadata = std::fs::metadata(root_path)?;
    let root_created = root_metadata
        .created()
        .ok()
        .map(chrono::DateTime::from)
        .unwrap_or_else(chrono::Utc::now);
    let root_modified = root_metadata
        .modified()
        .ok()
        .map(chrono::DateTime::from)
        .unwrap_or_else(chrono::Utc::now);

    callback(FastFileInfo {
        file_id: root_file_id,
        parent_file_id: root_file_id,
        name: volume.to_string(),
        is_directory: true,
        size: 0,
        created_at: root_created,
        modified_at: root_modified,
        attributes: root_attrs,
    });

    walk_dir_recursive(
        std::path::Path::new(root_path),
        root_file_id,
        volume,
        &mut callback,
    )
}

#[cfg(not(windows))]
#[derive(Debug, Clone)]
pub struct FastFileInfo {
    pub file_id: u64,
    pub parent_file_id: u64,
    pub name: String,
    pub is_directory: bool,
    pub size: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub modified_at: chrono::DateTime<chrono::Utc>,
    pub attributes: u32,
}

#[cfg(not(windows))]
pub fn walk_directory_fast<F>(_root_path: &str, _volume: &str, _callback: F) -> io::Result<()>
where
    F: FnMut(FastFileInfo),
{
    Ok(())
}

#[cfg(windows)]
pub fn is_elevated() -> bool {
    extern "system" {
        fn IsUserAnAdmin() -> i32;
    }
    unsafe { IsUserAnAdmin() != 0 }
}

#[cfg(windows)]
pub fn check_volume_usn(volume: &str) -> std::io::Result<()> {
    get_usn_cursor(volume).map(|_| ())
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|out| {
            String::from_utf8(out.stdout)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
        })
        .map(|uid| uid == 0)
        .unwrap_or(false)
}

/// Re-launch the current process with Administrator privileges using the Windows
/// UAC "runas" verb via ShellExecuteW.
///
/// - `extra_args`: additional CLI arguments to forward (the subcommand + its flags).
///
/// On success the UAC dialog is shown and a new elevated process starts; this function
/// returns `Ok(())` and the caller should exit immediately so the two instances don't
/// both try to do the same work.
///
/// On failure (user clicked "No" in the UAC dialog, or the exe path cannot be resolved)
/// it returns `Err`.
#[cfg(windows)]
pub fn relaunch_as_admin(extra_args: &[&str]) -> std::io::Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    // Encode a wide (UTF-16) string, NUL-terminated.
    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0u16))
            .collect()
    }

    let exe = std::env::current_exe()?;
    let exe_str = exe.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "exe path is not valid UTF-8",
        )
    })?;

    let verb = wide("runas");
    let file = wide(exe_str);
    let params_str = extra_args.join(" ");
    let params = wide(&params_str);

    let result = unsafe {
        ShellExecuteW(
            0, // hwnd
            verb.as_ptr(),
            file.as_ptr(),
            if params_str.is_empty() {
                std::ptr::null()
            } else {
                params.as_ptr()
            },
            std::ptr::null(), // lpDirectory (inherit)
            SW_SHOWNORMAL,
        )
    };

    // ShellExecuteW returns a value > 32 on success.
    if result as usize > 32 {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("ShellExecuteW(runas) failed with code {}", result as usize),
        ))
    }
}

#[cfg(not(windows))]
pub fn relaunch_as_admin(_extra_args: &[&str]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "relaunch_as_admin is only supported on Windows",
    ))
}

#[cfg(not(windows))]
pub fn check_volume_usn(_volume: &str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn kill_process_by_pid(pid: u32) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let success = TerminateProcess(handle, 1);
        CloseHandle(handle);
        if success == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn kill_process_by_pid(pid: u32) -> std::io::Result<()> {
    let output = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

// =========================================================================
// Windows Service Support
// =========================================================================

#[cfg(windows)]
type ServerRunnerFn = Box<dyn FnOnce(tokio::sync::oneshot::Receiver<()>) + Send>;

#[cfg(windows)]
static SERVER_RUNNER: std::sync::Mutex<Option<ServerRunnerFn>> = std::sync::Mutex::new(None);

#[cfg(windows)]
static SHUTDOWN_TX: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>> =
    std::sync::Mutex::new(None);

#[cfg(windows)]
static mut SERVICE_STATUS_HANDLE: isize = 0;

#[cfg(windows)]
static mut SERVICE_STATUS: windows_sys::Win32::System::Services::SERVICE_STATUS =
    windows_sys::Win32::System::Services::SERVICE_STATUS {
        dwServiceType: windows_sys::Win32::System::Services::SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: windows_sys::Win32::System::Services::SERVICE_START_PENDING,
        dwControlsAccepted: 0,
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };

#[cfg(windows)]
unsafe extern "system" fn service_ctrl_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut std::ffi::c_void,
    _context: *mut std::ffi::c_void,
) -> u32 {
    use windows_sys::Win32::System::Services::{
        SetServiceStatus, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_STOP_PENDING,
    };
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            SERVICE_STATUS.dwCurrentState = SERVICE_STOP_PENDING;
            SetServiceStatus(SERVICE_STATUS_HANDLE, std::ptr::addr_of!(SERVICE_STATUS));

            if let Ok(mut guard) = SHUTDOWN_TX.lock() {
                if let Some(tx) = guard.take() {
                    let _ = tx.send(());
                }
            }
            0
        }
        _ => 0,
    }
}

#[cfg(windows)]
unsafe extern "system" fn service_main(_dw_argc: u32, _lpsz_argv: *mut *mut u16) {
    use windows_sys::Win32::System::Services::{
        RegisterServiceCtrlHandlerExW, SetServiceStatus, SERVICE_ACCEPT_SHUTDOWN,
        SERVICE_ACCEPT_STOP, SERVICE_RUNNING, SERVICE_STOPPED,
    };
    let service_name: Vec<u16> = "DiskTracker\0".encode_utf16().collect();
    SERVICE_STATUS_HANDLE = RegisterServiceCtrlHandlerExW(
        service_name.as_ptr(),
        Some(service_ctrl_handler),
        std::ptr::null_mut(),
    );

    if SERVICE_STATUS_HANDLE == 0 {
        return;
    }

    SERVICE_STATUS.dwCurrentState = SERVICE_RUNNING;
    SERVICE_STATUS.dwControlsAccepted = SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN;
    SetServiceStatus(SERVICE_STATUS_HANDLE, std::ptr::addr_of!(SERVICE_STATUS));

    let runner = if let Ok(mut guard) = SERVER_RUNNER.lock() {
        guard.take()
    } else {
        None
    };

    if let Some(run_fn) = runner {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        if let Ok(mut guard) = SHUTDOWN_TX.lock() {
            *guard = Some(tx);
        }
        run_fn(rx);
    }

    SERVICE_STATUS.dwCurrentState = SERVICE_STOPPED;
    SetServiceStatus(SERVICE_STATUS_HANDLE, std::ptr::addr_of!(SERVICE_STATUS));
}

#[cfg(windows)]
fn run_service_dispatcher() -> std::io::Result<()> {
    use windows_sys::Win32::System::Services::{StartServiceCtrlDispatcherW, SERVICE_TABLE_ENTRYW};
    let service_name: Vec<u16> = "DiskTracker\0".encode_utf16().collect();
    let service_table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: service_name.as_ptr() as *mut u16,
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];

    unsafe {
        let success = StartServiceCtrlDispatcherW(service_table.as_ptr());
        if success == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn run_as_service<F>(run_server: F) -> std::io::Result<()>
where
    F: FnOnce(tokio::sync::oneshot::Receiver<()>) + Send + 'static,
{
    if let Ok(mut guard) = SERVER_RUNNER.lock() {
        *guard = Some(Box::new(run_server));
    }
    run_service_dispatcher()
}

#[cfg(not(windows))]
pub fn run_as_service<F>(run_server: F) -> std::io::Result<()>
where
    F: FnOnce(tokio::sync::oneshot::Receiver<()>) + Send + 'static,
{
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    rt.block_on(async {
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = tx.send(());
        });
        run_server(rx);
    });
    Ok(())
}

#[cfg(windows)]
pub fn register_service() -> std::io::Result<()> {
    let current_exe = std::env::current_exe()?;
    let current_exe_str = current_exe.to_string_lossy();
    let bin_path = format!("\"{}\" daemon --service", current_exe_str);

    let output = std::process::Command::new("sc.exe")
        .args([
            "create",
            "DiskTracker",
            "binPath=",
            &bin_path,
            "start=",
            "auto",
            "DisplayName=",
            "DiskTracker Daemon",
        ])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        let err_msg = String::from_utf8_lossy(&output.stderr).to_string();
        let out_msg = String::from_utf8_lossy(&output.stdout).to_string();
        Err(std::io::Error::other(format!(
            "sc create failed: {}\n{}",
            err_msg, out_msg
        )))
    }
}

#[cfg(not(windows))]
pub fn register_service() -> std::io::Result<()> {
    println!("[Unix Mock] Service registered.");
    Ok(())
}

#[cfg(windows)]
pub fn unregister_service() -> std::io::Result<()> {
    let output = std::process::Command::new("sc.exe")
        .args(["delete", "DiskTracker"])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        let err_msg = String::from_utf8_lossy(&output.stderr).to_string();
        let out_msg = String::from_utf8_lossy(&output.stdout).to_string();
        Err(std::io::Error::other(format!(
            "sc delete failed: {}\n{}",
            err_msg, out_msg
        )))
    }
}

#[cfg(not(windows))]
pub fn unregister_service() -> std::io::Result<()> {
    println!("[Unix Mock] Service unregistered.");
    Ok(())
}

#[cfg(windows)]
pub fn start_service() -> std::io::Result<()> {
    let output = std::process::Command::new("sc.exe")
        .args(["start", "DiskTracker"])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        let err_msg = String::from_utf8_lossy(&output.stderr).to_string();
        let out_msg = String::from_utf8_lossy(&output.stdout).to_string();
        Err(std::io::Error::other(format!(
            "sc start failed: {}\n{}",
            err_msg, out_msg
        )))
    }
}

#[cfg(not(windows))]
pub fn start_service() -> std::io::Result<()> {
    println!("[Unix Mock] Service started.");
    Ok(())
}

#[cfg(windows)]
pub fn stop_service() -> std::io::Result<()> {
    let output = std::process::Command::new("sc.exe")
        .args(["stop", "DiskTracker"])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        let err_msg = String::from_utf8_lossy(&output.stderr).to_string();
        let out_msg = String::from_utf8_lossy(&output.stdout).to_string();
        Err(std::io::Error::other(format!(
            "sc stop failed: {}\n{}",
            err_msg, out_msg
        )))
    }
}

#[cfg(not(windows))]
pub fn stop_service() -> std::io::Result<()> {
    println!("[Unix Mock] Service stopped.");
    Ok(())
}

mod etw;
pub use etw::start_etw_engine;
