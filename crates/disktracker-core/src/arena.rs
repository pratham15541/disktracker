use std::collections::HashMap;
use std::num::NonZeroU32;

/// Interned byte symbol — index into a flat byte pool.
/// Stores a raw filename (NOT a full path). OS bytes, not UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteSymbol(pub u32);

/// A single directory node. No heap allocation per node.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct DirNode {
    /// Parent stored as 1-based NonZeroU32: real index = value - 1. None = root.
    pub parent: Option<NonZeroU32>,
    /// Interned raw filename bytes (not full path).
    pub name: ByteSymbol,
    /// Recursive size in bytes (sum of all descendant file sizes).
    pub total_bytes: u64,
    /// Number of direct children files (not subdirs).
    pub file_count: u32,
    /// Last modified time as Unix timestamp seconds.
    pub mtime: i64,
    /// Depth from scan root (0 = root itself).
    pub depth: u16,
}

/// The arena: flat Vec of DirNodes + a byte pool for names.
pub struct PathlessArena {
    pub nodes: Vec<DirNode>,
    /// Flat pool of raw filename bytes.
    pub byte_pool: Vec<u8>,
    /// (offset, length) pairs into byte_pool indexed by ByteSymbol.
    pub symbols: Vec<(u32, u32)>,
    /// Map from bytes to symbol index for deduplication.
    intern_map: HashMap<Vec<u8>, ByteSymbol>,
}

impl PathlessArena {
    pub fn with_capacity(node_cap: usize, byte_cap: usize) -> Self {
        Self {
            nodes: Vec::with_capacity(node_cap),
            byte_pool: Vec::with_capacity(byte_cap),
            symbols: Vec::new(),
            intern_map: HashMap::new(),
        }
    }

    /// Intern raw OS bytes. Returns existing symbol if already stored.
    pub fn intern(&mut self, bytes: &[u8]) -> ByteSymbol {
        if let Some(&sym) = self.intern_map.get(bytes) {
            return sym;
        }
        let offset = self.byte_pool.len() as u32;
        let len = bytes.len() as u32;
        self.byte_pool.extend_from_slice(bytes);
        let sym = ByteSymbol(self.symbols.len() as u32);
        self.symbols.push((offset, len));
        self.intern_map.insert(bytes.to_vec(), sym);
        sym
    }

    /// Encode a 0-based parent index as a NonZeroU32 (1-based).
    /// Panics if idx would overflow NonZeroU32 (> u32::MAX - 1).
    pub fn encode_parent(idx: u32) -> Option<NonZeroU32> {
        NonZeroU32::new(idx + 1)
    }

    /// Reconstruct full path string by walking parent chain.
    /// ONLY called at query time, never during scan.
    pub fn materialize_path(&self, node_idx: u32) -> Vec<u8> {
        let mut segments: Vec<&[u8]> = Vec::new();
        let mut idx = node_idx;
        loop {
            let node = &self.nodes[idx as usize];
            let (off, len) = self.symbols[node.name.0 as usize];
            segments.push(&self.byte_pool[off as usize..(off + len) as usize]);
            match node.parent {
                // parent is stored 1-based; decode to 0-based
                None => break,
                Some(p) => idx = p.get() - 1,
            }
        }
        segments.reverse();

        #[cfg(windows)]
        const SEP: u8 = b'\\';
        #[cfg(not(windows))]
        const SEP: u8 = b'/';

        let mut path = Vec::new();
        for (i, seg) in segments.iter().enumerate() {
            if i == 0 {
                path.extend_from_slice(seg);
            } else {
                if !path.ends_with(&[SEP]) {
                    path.push(SEP);
                }
                path.extend_from_slice(seg);
            }
        }
        path
    }

    /// Add a directory node. Returns its 0-based index.
    pub fn push(&mut self, node: DirNode) -> u32 {
        let idx = self.nodes.len() as u32;
        self.nodes.push(node);
        idx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intern_dedup() {
        let mut arena = PathlessArena::with_capacity(16, 256);
        let s1 = arena.intern(b"home");
        let s2 = arena.intern(b"home");
        assert_eq!(s1, s2);
        let s3 = arena.intern(b"user");
        assert_ne!(s1, s3);
    }

    #[test]
    fn test_materialize_path_root() {
        let mut arena = PathlessArena::with_capacity(16, 256);
        let root_name = arena.intern(b"/");
        let root_idx = arena.push(DirNode {
            parent: None,
            name: root_name,
            total_bytes: 1000,
            file_count: 5,
            mtime: 0,
            depth: 0,
        });
        let path = arena.materialize_path(root_idx);
        assert_eq!(path, b"/");
    }

    #[test]
    fn test_materialize_path_child() {
        let mut arena = PathlessArena::with_capacity(16, 256);
        let root_name = arena.intern(b"/home/user");
        let root_idx = arena.push(DirNode {
            parent: None,
            name: root_name,
            total_bytes: 5000,
            file_count: 0,
            mtime: 0,
            depth: 0,
        });
        let child_name = arena.intern(b"documents");
        let child_idx = arena.push(DirNode {
            parent: PathlessArena::encode_parent(root_idx),
            name: child_name,
            total_bytes: 2000,
            file_count: 10,
            mtime: 0,
            depth: 1,
        });
        let path = arena.materialize_path(child_idx);
        assert_eq!(path, b"/home/user/documents");
    }

    #[test]
    fn test_materialize_nested() {
        let mut arena = PathlessArena::with_capacity(16, 256);
        let root_name = arena.intern(b"/");
        let root_idx = arena.push(DirNode {
            parent: None,
            name: root_name,
            total_bytes: 0,
            file_count: 0,
            mtime: 0,
            depth: 0,
        });
        let a_name = arena.intern(b"var");
        let a_idx = arena.push(DirNode {
            parent: PathlessArena::encode_parent(root_idx),
            name: a_name,
            total_bytes: 0,
            file_count: 0,
            mtime: 0,
            depth: 1,
        });
        let b_name = arena.intern(b"log");
        let b_idx = arena.push(DirNode {
            parent: PathlessArena::encode_parent(a_idx),
            name: b_name,
            total_bytes: 500,
            file_count: 3,
            mtime: 0,
            depth: 2,
        });
        let path = arena.materialize_path(b_idx);
        assert_eq!(path, b"/var/log");
    }
}
