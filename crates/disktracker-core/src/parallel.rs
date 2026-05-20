use crossbeam_deque::{Injector, Stealer, Worker};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::arena::{PathlessArena, NO_PARENT};
use crate::identity::FsIdentity;
use crate::scan::{ScanConfig, ScanResult};

#[cfg(unix)]
use crate::platform::unix::{ClassifiedEntry, DirMeta, EntryType};

#[cfg(windows)]
use crate::platform::windows::{ClassifiedEntry, DirMeta, EntryType};

pub struct ParallelConfig {
    pub threads: u16,
}

struct DirJob {
    path: PathBuf,
    node_idx: u32,
    depth: u16,
}

struct NodeState {
    parent: u32,
    pending_subdirs: u32,
    accumulated_bytes: u64,
    accumulated_files: u64,
    mtime: i64,
    identity: FsIdentity,
}

struct SharedState {
    arena: PathlessArena,
    node_states: Vec<NodeState>,
    error_count: u32,
    root_device: u64,
}

impl SharedState {
    fn decrement_pending(&mut self, node_idx: u32) {
        let mut curr_idx = node_idx;
        loop {
            let pending = {
                let state = &mut self.node_states[curr_idx as usize];
                state.pending_subdirs -= 1;
                state.pending_subdirs
            };

            if pending == 0 {
                // Node is complete!
                let accum_bytes = self.node_states[curr_idx as usize].accumulated_bytes;
                let accum_files = self.node_states[curr_idx as usize].accumulated_files;
                let mtime = self.node_states[curr_idx as usize].mtime;
                let identity = self.node_states[curr_idx as usize].identity;

                self.arena.hot[curr_idx as usize].total_bytes = accum_bytes;
                self.arena.hot[curr_idx as usize].file_count = accum_files as u32;
                self.arena.cold[curr_idx as usize].mtime = mtime;
                self.arena.cold[curr_idx as usize].identity = identity;

                let parent = self.node_states[curr_idx as usize].parent;
                if parent == NO_PARENT {
                    break;
                } else {
                    self.node_states[parent as usize].accumulated_bytes += accum_bytes;
                    self.node_states[parent as usize].accumulated_files += accum_files;
                    curr_idx = parent;
                }
            } else {
                break;
            }
        }
    }
}

fn path_to_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

fn get_device_of_path(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        crate::platform::unix::get_dev_abs(path.as_os_str().as_bytes())
    }
    #[cfg(windows)]
    {
        crate::platform::windows::get_volume_serial(path)
    }
}

#[cfg(unix)]
fn read_entries(
    path: &Path,
    skip_names: &[Vec<u8>],
    max_depth: Option<u16>,
    depth: u16,
) -> Result<(Vec<ClassifiedEntry>, u64, u32, u32), (u32,)> {
    use std::os::unix::ffi::OsStrExt;
    let path_bytes = path.as_os_str().as_bytes();
    let dir_fd = match crate::platform::unix::open_dir_abs(path_bytes) {
        Ok(fd) => fd,
        Err(_) => return Err((1,)),
    };
    crate::platform::unix::read_dir_entries(
        rustix::fd::AsFd::as_fd(&dir_fd),
        skip_names,
        max_depth,
        depth,
    )
}

#[cfg(windows)]
fn read_entries(
    path: &Path,
    skip_names: &[Vec<u8>],
    max_depth: Option<u16>,
    depth: u16,
) -> Result<(Vec<ClassifiedEntry>, u64, u32, u32), (u32,)> {
    crate::platform::windows::read_dir_entries(path, skip_names, max_depth, depth)
}

#[cfg(unix)]
fn read_meta(path: &Path) -> DirMeta {
    use std::os::unix::ffi::OsStrExt;
    let path_bytes = path.as_os_str().as_bytes();
    if let Ok(dir_fd) = crate::platform::unix::open_dir_abs(path_bytes) {
        crate::platform::unix::read_dir_meta(rustix::fd::AsFd::as_fd(&dir_fd))
    } else {
        DirMeta {
            mtime: 0,
            identity: FsIdentity::UNKNOWN,
        }
    }
}

#[cfg(windows)]
fn read_meta(path: &Path) -> DirMeta {
    crate::platform::windows::read_dir_meta(path)
}

fn process_job(
    job: DirJob,
    local_queue: &Worker<DirJob>,
    shared: &Mutex<SharedState>,
    config: &ScanConfig,
    _injector: &Injector<DirJob>,
) {
    let DirJob {
        path,
        node_idx,
        depth,
    } = job;

    if let Some(ref skip_pred) = config.skip_predicate {
        let path_bytes = path_to_bytes(&path);
        let meta = read_meta(&path);
        if let Some(skip_res) = skip_pred(&path_bytes, meta.mtime, meta.identity) {
            let mut state = shared.lock().unwrap();
            let node_state = &mut state.node_states[node_idx as usize];
            node_state.accumulated_bytes = skip_res.total_bytes;
            node_state.accumulated_files = skip_res.file_count as u64;
            node_state.mtime = meta.mtime;
            node_state.identity = meta.identity;
            node_state.pending_subdirs = 1;
            state.decrement_pending(node_idx);
            return;
        }
    }

    let (entries, file_bytes, file_count, errs) =
        match read_entries(&path, &config.skip_names, config.max_depth, depth) {
            Ok(res) => res,
            Err((e_count,)) => {
                let mut state = shared.lock().unwrap();
                state.error_count += e_count;
                state.node_states[node_idx as usize].pending_subdirs = 1;
                state.decrement_pending(node_idx);
                return;
            }
        };

    let meta = read_meta(&path);

    let mut child_jobs = Vec::new();

    {
        let mut state = shared.lock().unwrap();
        state.error_count += errs;

        let node_state = &mut state.node_states[node_idx as usize];
        node_state.accumulated_bytes += file_bytes;
        node_state.accumulated_files += file_count as u64;
        node_state.mtime = meta.mtime;
        node_state.identity = meta.identity;

        let mut valid_subdirs = 0;

        for entry in entries {
            if entry.entry_type != EntryType::Dir {
                continue;
            }

            let child_path = {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    path.join(std::ffi::OsStr::from_bytes(&entry.name_bytes))
                }
                #[cfg(windows)]
                {
                    let name_str = String::from_utf8_lossy(&entry.name_bytes).into_owned();
                    path.join(name_str)
                }
            };

            let is_same_fs = if config.one_filesystem {
                let root_device = state.root_device;
                if root_device != 0 {
                    let child_dev = get_device_of_path(&child_path).unwrap_or(0);
                    child_dev == root_device
                } else {
                    true
                }
            } else {
                true
            };

            if !is_same_fs {
                continue;
            }

            let sym = state.arena.intern(&entry.name_bytes);
            let child_idx = state.arena.push_node(node_idx, sym, depth + 1);
            state.node_states.push(NodeState {
                parent: node_idx,
                pending_subdirs: 1,
                accumulated_bytes: 0,
                accumulated_files: 0,
                mtime: 0,
                identity: FsIdentity::UNKNOWN,
            });

            child_jobs.push(DirJob {
                path: child_path,
                node_idx: child_idx,
                depth: depth + 1,
            });
            valid_subdirs += 1;
        }

        state.node_states[node_idx as usize].pending_subdirs = valid_subdirs + 1;
        state.decrement_pending(node_idx);
    }

    for child_job in child_jobs {
        local_queue.push(child_job);
    }
}

pub fn scan_parallel(config: &ScanConfig) -> ScanResult {
    let start = std::time::Instant::now();
    let num_threads = if config.parallelism == 0 {
        num_cpus::get().min(8) as usize
    } else {
        config.parallelism as usize
    };

    let mut arena = PathlessArena::with_capacity(65536, 4 * 1024 * 1024);

    let root_bytes = path_to_bytes(&config.root);
    let root_sym = arena.intern(&root_bytes);
    let root_idx = arena.push_node(NO_PARENT, root_sym, 0);

    let root_device = if config.one_filesystem {
        get_device_of_path(&config.root).unwrap_or(0)
    } else {
        0
    };

    let mut node_states = Vec::with_capacity(65536);
    node_states.push(NodeState {
        parent: NO_PARENT,
        pending_subdirs: 1,
        accumulated_bytes: 0,
        accumulated_files: 0,
        mtime: 0,
        identity: FsIdentity::UNKNOWN,
    });

    let shared = Arc::new(Mutex::new(SharedState {
        arena,
        node_states,
        error_count: 0,
        root_device,
    }));

    let workers: Vec<Worker<DirJob>> = (0..num_threads).map(|_| Worker::new_fifo()).collect();
    let stealers: Vec<Stealer<DirJob>> = workers.iter().map(|w| w.stealer()).collect();
    let injector = Injector::new();

    injector.push(DirJob {
        path: config.root.clone(),
        node_idx: root_idx,
        depth: 0,
    });

    crossbeam_utils::thread::scope(|scope| {
        for (thread_id, worker) in workers.into_iter().enumerate() {
            let stealers = stealers.clone();
            let injector = &injector;
            let shared = &shared;

            scope.spawn(move |_| {
                let local_queue = worker;

                loop {
                    if config.is_cancelled() {
                        break;
                    }

                    let job = local_queue.pop().or_else(|| {
                        injector
                            .steal_batch_and_pop(&local_queue)
                            .success()
                            .or_else(|| {
                                let mut idx = thread_id;
                                for _ in 0..stealers.len() {
                                    idx = (idx + 1) % stealers.len();
                                    if idx == thread_id {
                                        continue;
                                    }
                                    if let Some(j) =
                                        stealers[idx].steal_batch_and_pop(&local_queue).success()
                                    {
                                        return Some(j);
                                    }
                                }
                                None
                            })
                    });

                    if let Some(job) = job {
                        process_job(job, &local_queue, shared, config, injector);
                    } else {
                        let root_done = {
                            let state = shared.lock().unwrap();
                            state.node_states[root_idx as usize].pending_subdirs == 0
                        };
                        if root_done {
                            break;
                        }
                        std::thread::yield_now();
                    }
                }
            });
        }
    })
    .unwrap();

    let state = Arc::try_unwrap(shared).ok().unwrap().into_inner().unwrap();

    let total_bytes = state.arena.hot[root_idx as usize].total_bytes;
    let total_files = state.arena.hot[root_idx as usize].file_count as u64;

    ScanResult {
        arena: state.arena,
        total_files,
        total_bytes,
        scan_duration_ms: start.elapsed().as_millis() as u64,
        error_count: state.error_count,
    }
}
