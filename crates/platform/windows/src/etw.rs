use std::thread;
use std::time::Duration;

#[cfg(windows)]
use ferrisetw::{
    parser::Parser,
    provider::Provider,
    trace::UserTrace,
};
#[cfg(windows)]
use std::collections::{HashMap, HashSet};
#[cfg(windows)]
use std::sync::{Mutex, OnceLock};

#[cfg(windows)]
struct InstallerSession {
    app_name: Option<String>,
    installer_pids: HashSet<u32>,
    written_files: HashSet<String>,
}

#[cfg(windows)]
static ACTIVE_INSTALLERS: OnceLock<Mutex<HashMap<u32, InstallerSession>>> = OnceLock::new();

#[cfg(windows)]
fn get_active_installers() -> &'static Mutex<HashMap<u32, InstallerSession>> {
    ACTIVE_INSTALLERS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(windows)]
pub fn start_etw_engine() {
    thread::spawn(|| {
        println!("[ETW Engine] Starting real Windows ETW trace session...");
        if let Err(e) = run_windows_etw() {
            eprintln!("[ETW Engine] Real Windows ETW error: {}", e);
        }
    });
}

#[cfg(windows)]
fn run_windows_etw() -> Result<(), String> {
    let process_provider = Provider::by_guid("22fb2cd6-0e7b-422b-a0c7-2fad1fd0e716") // Microsoft-Windows-Kernel-Process
        .add_callback(|record, schema_locator| {
            if let Ok(schema) = schema_locator.event_schema(record) {
                let event_id = record.event_id();
                let parser = Parser::create(record, &schema);

                if event_id == 1 {
                    // Process Start
                    if let (Ok(pid), Ok(parent_pid), Ok(image_name)) = (
                        parser.try_parse::<u32>("ProcessId"),
                        parser.try_parse::<u32>("ParentProcessID"),
                        parser.try_parse::<String>("ImageFileName"),
                    ) {
                        let name_lower = image_name.to_lowercase();
                        let is_installer = name_lower.contains("setup")
                            || name_lower.contains("install")
                            || name_lower.contains("msiexec")
                            || name_lower.contains("update");

                        let mut active = get_active_installers().lock().unwrap();
                        let mut found_parent_key = None;
                        for (&key_pid, session) in active.iter() {
                            if session.installer_pids.contains(&parent_pid) {
                                found_parent_key = Some(key_pid);
                                break;
                            }
                        }

                        if let Some(key_pid) = found_parent_key {
                            if let Some(session) = active.get_mut(&key_pid) {
                                session.installer_pids.insert(pid);
                                println!("[ETW - Process] Added child installer PID {} to session {}", pid, key_pid);
                            }
                        } else if is_installer {
                            let mut pids = HashSet::new();
                            pids.insert(pid);
                            active.insert(pid, InstallerSession {
                                app_name: None,
                                installer_pids: pids,
                                written_files: HashSet::new(),
                            });
                            println!("[ETW - Process] Started new installer session for PID {} ({})", pid, image_name);
                        }
                    }
                } else if event_id == 2 {
                    // Process Stop
                    if let Ok(pid) = parser.try_parse::<u32>("ProcessId") {
                        let mut active = get_active_installers().lock().unwrap();
                        let mut sessions_to_close = Vec::new();

                        for (&key_pid, session) in active.iter_mut() {
                            if session.installer_pids.remove(&pid) {
                                println!("[ETW - Process] Installer PID {} exited in session {}", pid, key_pid);
                                if session.installer_pids.is_empty() {
                                    sessions_to_close.push(key_pid);
                                }
                            }
                        }

                        for key_pid in sessions_to_close {
                            if let Some(session) = active.remove(&key_pid) {
                                let app = session.app_name.clone().unwrap_or_else(|| format!("installer_{}", key_pid));
                                println!("[ETW - Process] Installer session {} completed. Committing {} files for app '{}'", key_pid, session.written_files.len(), app);
                                for file in session.written_files {
                                    let _ = storage::insert_app_install_footprint(&app, &file);
                                }
                            }
                        }
                    }
                }
            }
        })
        .build();

    let registry_provider = Provider::by_guid("70eb4f03-c1de-4fc1-b045-cef40fa85d06") // Microsoft-Windows-Kernel-Registry
        .add_callback(|record, schema_locator| {
            if let Ok(schema) = schema_locator.event_schema(record) {
                let parser = Parser::create(record, &schema);
                let pid = record.process_id();

                let mut is_installer = false;
                {
                    let active = get_active_installers().lock().unwrap();
                    for session in active.values() {
                        if session.installer_pids.contains(&pid) {
                            is_installer = true;
                            break;
                        }
                    }
                }

                if is_installer {
                    if let (Ok(key_name), Ok(value_name), Ok(value_data)) = (
                        parser.try_parse::<String>("KeyName"),
                        parser.try_parse::<String>("ValueName"),
                        parser.try_parse::<String>("Data"),
                    ) {
                        if key_name.contains("CurrentVersion\\Uninstall") && value_name == "DisplayName" {
                            let mut active = get_active_installers().lock().unwrap();
                            for session in active.values_mut() {
                                if session.installer_pids.contains(&pid) {
                                    session.app_name = Some(value_data.clone());
                                    println!("[ETW - Registry] Associated installer process {} with app '{}'", pid, value_data);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        })
        .build();

    let file_provider = Provider::by_guid("edd08927-c37b-4de6-b105-8161b20db7e8") // Microsoft-Windows-Kernel-File
        .add_callback(|record, schema_locator| {
            if let Ok(schema) = schema_locator.event_schema(record) {
                let pid = record.process_id();
                let event_id = record.event_id();

                if event_id == 12 || event_id == 15 {
                    let parser = Parser::create(record, &schema);
                    if let Ok(file_name) = parser.try_parse::<String>("FileName") {
                        let mut active = get_active_installers().lock().unwrap();
                        let mut found_session = false;
                        for session in active.values_mut() {
                            if session.installer_pids.contains(&pid) {
                                session.written_files.insert(file_name.clone());
                                found_session = true;
                                break;
                            }
                        }

                        if !found_session && event_id == 15 {
                            let file_lower = file_name.to_lowercase();
                            if file_lower.contains("appdata\\local")
                                || file_lower.contains("appdata\\roaming")
                                || file_lower.contains("documents\\")
                            {
                                if let Some(proc_name) = get_process_name_by_pid(pid) {
                                    let proc_lower = proc_name.to_lowercase();
                                    if !proc_lower.contains("explorer.exe")
                                        && !proc_lower.contains("svchost.exe")
                                        && !proc_lower.contains("chrome.exe")
                                        && !proc_lower.contains("firefox.exe")
                                        && !proc_lower.contains("msedge.exe")
                                    {
                                        let _ = storage::insert_app_runtime_artifact(&proc_name, &file_name);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
        .build();

    let _trace = UserTrace::new()
        .named("disktracker-etw-session".to_string())
        .enable(process_provider)
        .enable(registry_provider)
        .enable(file_provider)
        .start()
        .map_err(|e| format!("Trace start error: {:?}", e))?;

    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

#[cfg(windows)]
fn get_process_name_by_pid(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 || handle == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let success = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size);
        CloseHandle(handle);

        if success != 0 {
            let path = String::from_utf16_lossy(&buffer[..size as usize]);
            if let Some(filename) = std::path::Path::new(&path).file_name() {
                return Some(filename.to_string_lossy().to_string());
            }
        }
        None
    }
}

#[cfg(not(windows))]
pub fn start_etw_engine() {
    thread::spawn(|| {
        println!("[ETW Mock] Starting mock ETW tracking engine...");
        prepopulate_mock_telemetry_data();

        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    });
}

#[cfg(not(windows))]
fn prepopulate_mock_telemetry_data() {
    let _ = storage::insert_app_install_footprint(
        "Hollow Knight",
        "C:/Program Files (x86)/Steam/steamapps/common/Hollow Knight/hollow_knight.exe"
    );
    let _ = storage::insert_app_install_footprint(
        "Hollow Knight",
        "C:/Program Files (x86)/Steam/steamapps/common/Hollow Knight/hollow_knight_Data/boot.config"
    );
    let _ = storage::insert_app_install_footprint(
        "Hollow Knight",
        "C:/Program Files (x86)/Steam/steamapps/common/Hollow Knight/hollow_knight_Data/level0"
    );

    let _ = storage::insert_app_runtime_artifact(
        "hollow_knight.exe",
        "C:/Users/you/AppData/LocalLow/Team Cherry/Hollow Knight/user1.dat"
    );
    let _ = storage::insert_app_runtime_artifact(
        "hollow_knight.exe",
        "C:/Users/you/AppData/LocalLow/Team Cherry/Hollow Knight/Player.log"
    );

    let _ = storage::insert_app_install_footprint(
        "Epic Games Launcher",
        "C:/Program Files (x86)/Epic Games/Launcher/Portal/Binaries/Win64/EpicGamesLauncher.exe"
    );

    let _ = storage::insert_app_runtime_artifact(
        "EpicGamesLauncher.exe",
        "C:/Users/you/AppData/Local/EpicGamesLauncher/Saved/Config/Windows/GameUserSettings.ini"
    );
    let _ = storage::insert_app_runtime_artifact(
        "EpicGamesLauncher.exe",
        "C:/Users/you/AppData/Local/UnrealEngine/Common/DerivedDataCache"
    );
    println!("[ETW Mock] Pre-populated database with mock app telemetry data.");
}
