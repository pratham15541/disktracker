use anyhow::Result;
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MmapHeader {
    pub magic: [u8; 8],
    pub version: u32,
    pub entry_count: u32,
    pub path_pool_size: u32,
}

impl MmapHeader {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut magic = [0u8; 8];
        magic.copy_from_slice(&bytes[0..8]);
        let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let entry_count = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let path_pool_size = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        Self {
            magic,
            version,
            entry_count,
            path_pool_size,
        }
    }

    pub fn to_bytes(&self) -> [u8; 20] {
        let mut buf = [0u8; 20];
        buf[0..8].copy_from_slice(&self.magic);
        buf[8..12].copy_from_slice(&self.version.to_le_bytes());
        buf[12..16].copy_from_slice(&self.entry_count.to_le_bytes());
        buf[16..20].copy_from_slice(&self.path_pool_size.to_le_bytes());
        buf
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MmapEntry {
    pub path_offset: u32,
    pub path_len: u32,
    pub total_bytes: i64,
}

impl MmapEntry {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let path_offset = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let path_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let total_bytes = i64::from_le_bytes(bytes[8..16].try_into().unwrap());
        Self {
            path_offset,
            path_len,
            total_bytes,
        }
    }

    pub fn to_bytes(&self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0..4].copy_from_slice(&self.path_offset.to_le_bytes());
        buf[4..8].copy_from_slice(&self.path_len.to_le_bytes());
        buf[8..16].copy_from_slice(&self.total_bytes.to_le_bytes());
        buf
    }
}

pub struct MmapIndex {
    mmap: Mmap,
    header: MmapHeader,
}

impl MmapIndex {
    pub fn new(mmap: Mmap) -> Result<Self, &'static str> {
        if mmap.len() < 20 {
            return Err("File too small to contain header");
        }
        let header = MmapHeader::from_bytes(&mmap[0..20]);
        if header.magic != *b"DSKTRKMM" {
            return Err("Invalid magic identifier");
        }
        if header.version != 1 {
            return Err("Unsupported version");
        }

        let expected_size =
            20 + (header.entry_count as usize * 16) + header.path_pool_size as usize;
        if mmap.len() < expected_size {
            return Err("File size is smaller than header entry_count and path_pool_size require");
        }
        Ok(Self { mmap, header })
    }

    pub fn entry_count(&self) -> usize {
        self.header.entry_count as usize
    }

    pub fn get_entry(&self, index: usize) -> MmapEntry {
        let offset = 20 + index * 16;
        MmapEntry::from_bytes(&self.mmap[offset..offset + 16])
    }

    pub fn get_path(&self, entry: &MmapEntry) -> &[u8] {
        let pool_start = 20 + (self.header.entry_count as usize * 16);
        let path_start = pool_start + entry.path_offset as usize;
        &self.mmap[path_start..path_start + entry.path_len as usize]
    }

    pub fn get_size(&self, path: &[u8]) -> Option<i64> {
        if self.header.entry_count == 0 {
            return None;
        }
        let mut low = 0;
        let mut high = self.header.entry_count as usize;

        while low < high {
            let mid = low + (high - low) / 2;
            let entry = self.get_entry(mid);
            let entry_path = self.get_path(&entry);
            match entry_path.cmp(path) {
                std::cmp::Ordering::Equal => return Some(entry.total_bytes),
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
            }
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlayEntry {
    Modified(i64),
    Deleted,
}

pub struct MmapState {
    pub mmap_index: Option<MmapIndex>,
    pub overlay: HashMap<Vec<u8>, OverlayEntry>,
}

impl MmapState {
    pub fn new(mmap_index: Option<MmapIndex>) -> Self {
        Self {
            mmap_index,
            overlay: HashMap::new(),
        }
    }

    pub fn load_from_file(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let index = MmapIndex::new(mmap).map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(Self::new(Some(index)))
    }

    pub fn reload_from_file(&mut self, path: &Path) -> Result<()> {
        self.mmap_index = None;
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let index = MmapIndex::new(mmap).map_err(|e| anyhow::anyhow!("{}", e))?;
        self.mmap_index = Some(index);
        self.overlay.clear();
        Ok(())
    }

    pub fn contains_key(&self, path: &[u8]) -> bool {
        self.get(path).is_some()
    }

    pub fn get(&self, path: &[u8]) -> Option<i64> {
        if let Some(entry) = self.overlay.get(path) {
            return match entry {
                OverlayEntry::Modified(size) => Some(*size),
                OverlayEntry::Deleted => None,
            };
        }
        if let Some(ref index) = self.mmap_index {
            index.get_size(path)
        } else {
            None
        }
    }

    pub fn insert(&mut self, path: Vec<u8>, size: i64) {
        self.overlay.insert(path, OverlayEntry::Modified(size));
    }

    pub fn remove_subtree(&mut self, prefix: &[u8]) {
        let mut keys_to_delete = Vec::new();
        if let Some(ref index) = self.mmap_index {
            for i in 0..index.entry_count() {
                let entry = index.get_entry(i);
                let path = index.get_path(&entry);
                if is_subpath_or_same(path, prefix) {
                    keys_to_delete.push(path.to_vec());
                }
            }
        }
        for path in keys_to_delete {
            self.overlay.insert(path, OverlayEntry::Deleted);
        }

        for (path, entry) in self.overlay.iter_mut() {
            if is_subpath_or_same(path, prefix) {
                *entry = OverlayEntry::Deleted;
            }
        }
    }

    pub fn get_active_entries(&self) -> Vec<(Vec<u8>, i64)> {
        let mut merged = HashMap::new();

        if let Some(ref index) = self.mmap_index {
            for i in 0..index.entry_count() {
                let entry = index.get_entry(i);
                let path = index.get_path(&entry);
                merged.insert(path.to_vec(), entry.total_bytes);
            }
        }

        for (path, val) in &self.overlay {
            match val {
                OverlayEntry::Modified(size) => {
                    merged.insert(path.clone(), *size);
                }
                OverlayEntry::Deleted => {
                    merged.remove(path);
                }
            }
        }

        let mut result: Vec<(Vec<u8>, i64)> = merged.into_iter().collect();
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    pub fn write_to_file(&mut self, path: &Path) -> Result<()> {
        let active = self.get_active_entries();
        let mut path_pool = Vec::new();
        let mut entries = Vec::with_capacity(active.len());

        for (path_bytes, size) in &active {
            let path_offset = path_pool.len() as u32;
            let path_len = path_bytes.len() as u32;
            path_pool.extend_from_slice(path_bytes);

            entries.push(MmapEntry {
                path_offset,
                path_len,
                total_bytes: *size,
            });
        }

        let header = MmapHeader {
            magic: *b"DSKTRKMM",
            version: 1,
            entry_count: entries.len() as u32,
            path_pool_size: path_pool.len() as u32,
        };

        let tmp_path = path.with_extension("mmap.tmp");

        {
            let mut file = File::create(&tmp_path)?;
            file.write_all(&header.to_bytes())?;
            for entry in &entries {
                file.write_all(&entry.to_bytes())?;
            }
            file.write_all(&path_pool)?;
            file.sync_all()?;
        }

        self.mmap_index = None; // Drop old mapping so rename succeeds on Windows
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.get_active_entries().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub fn is_subpath_or_same(path: &[u8], prefix: &[u8]) -> bool {
    if !path.starts_with(prefix) {
        return false;
    }
    if path.len() == prefix.len() {
        return true;
    }
    let next_byte = path[prefix.len()];
    next_byte == b'/' || next_byte == b'\\'
}
