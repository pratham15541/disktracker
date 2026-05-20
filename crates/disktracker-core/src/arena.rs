use crate::identity::FsIdentity;
use std::collections::HashMap;

/// Interned byte symbol — index into a flat byte pool.
/// Stores a raw filename (NOT a full path). OS bytes, not UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteSymbol(pub u32);

/// Hot traversal data. 24 bytes. Kept tight for cache efficiency.
/// Array index IS the node ID.
/// During scan: parent, name, depth are set on push; total_bytes and
/// file_count are patched after child enumeration completes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NodeHot {
    /// Parent index (0-based). u32::MAX = root (no parent).
    pub parent: u32,
    /// Interned raw filename bytes (not full path).
    pub name: ByteSymbol,
    /// Recursive size in bytes (sum of all descendant file sizes).
    pub total_bytes: u64,
    /// Number of direct children files (not subdirs).
    pub file_count: u32,
    /// Depth from scan root (0 = root itself).
    pub depth: u16,
    /// Padding for alignment.
    pub _pad: u16,
}

/// Cold metadata. 24 bytes. Same index as NodeHot. Loaded on demand.
/// Contains data needed for identity, warm scan, and DB serialization
/// but NOT for hot-path traversal or aggregation.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NodeCold {
    /// Filesystem identity: (dev, ino). UNKNOWN if not populated.
    pub identity: FsIdentity,
    /// Last modified time as Unix timestamp seconds.
    pub mtime: i64,
}

impl Default for NodeCold {
    fn default() -> Self {
        Self {
            identity: FsIdentity::UNKNOWN,
            mtime: 0,
        }
    }
}

/// Sentinel value for "no parent" (root node).
pub const NO_PARENT: u32 = u32::MAX;

/// The arena: parallel arrays of NodeHot + NodeCold + a byte pool for names.
///
/// Node ID is the index into both arrays. Parent references use raw u32 indices
/// (NO_PARENT for roots) — all indices are globally valid, even across parallel
/// worker segments.
pub struct PathlessArena {
    pub hot: Vec<NodeHot>,
    pub cold: Vec<NodeCold>,
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
            hot: Vec::with_capacity(node_cap),
            cold: Vec::with_capacity(node_cap),
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

    /// Reconstruct full path string by walking parent chain.
    /// ONLY called at query time, never during scan.
    pub fn materialize_path(&self, node_idx: u32) -> Vec<u8> {
        let mut segments: Vec<&[u8]> = Vec::new();
        let mut idx = node_idx;
        loop {
            let node = &self.hot[idx as usize];
            let (off, len) = self.symbols[node.name.0 as usize];
            segments.push(&self.byte_pool[off as usize..(off + len) as usize]);
            if node.parent == NO_PARENT {
                break;
            }
            idx = node.parent;
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
    pub fn push_node(&mut self, parent: u32, name: ByteSymbol, depth: u16) -> u32 {
        let idx = self.hot.len() as u32;
        self.hot.push(NodeHot {
            parent,
            name,
            total_bytes: 0,
            file_count: 0,
            depth,
            _pad: 0,
        });
        self.cold.push(NodeCold::default());
        idx
    }

    /// Number of nodes in the arena.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.hot.len()
    }

    /// Resolve a name symbol to its raw bytes.
    #[inline]
    pub fn resolve_name(&self, sym: ByteSymbol) -> &[u8] {
        let (off, len) = self.symbols[sym.0 as usize];
        &self.byte_pool[off as usize..(off + len) as usize]
    }
}

/// Backward-compatible type alias. Callers that used DirNode in tests
/// can construct a combined view.
#[derive(Debug, Clone)]
pub struct DirNode {
    pub parent: u32,
    pub name: ByteSymbol,
    pub total_bytes: u64,
    pub file_count: u32,
    pub mtime: i64,
    pub depth: u16,
    pub identity: FsIdentity,
}

impl DirNode {
    /// Push this DirNode into the arena, splitting into hot + cold.
    pub fn push_into(self, arena: &mut PathlessArena) -> u32 {
        let idx = arena.hot.len() as u32;
        arena.hot.push(NodeHot {
            parent: self.parent,
            name: self.name,
            total_bytes: self.total_bytes,
            file_count: self.file_count,
            depth: self.depth,
            _pad: 0,
        });
        arena.cold.push(NodeCold {
            identity: self.identity,
            mtime: self.mtime,
        });
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
        let root_idx = arena.push_node(NO_PARENT, root_name, 0);
        let path = arena.materialize_path(root_idx);
        assert_eq!(path, b"/");
    }

    #[test]
    fn test_materialize_path_child() {
        let mut arena = PathlessArena::with_capacity(16, 256);
        let root_name = arena.intern(b"/home/user");
        let root_idx = arena.push_node(NO_PARENT, root_name, 0);

        let child_name = arena.intern(b"documents");
        let child_idx = arena.push_node(root_idx, child_name, 1);
        arena.hot[child_idx as usize].total_bytes = 2000;
        arena.hot[child_idx as usize].file_count = 10;

        let path = arena.materialize_path(child_idx);
        assert_eq!(path, b"/home/user/documents");
    }

    #[test]
    fn test_materialize_nested() {
        let mut arena = PathlessArena::with_capacity(16, 256);
        let root_name = arena.intern(b"/");
        let root_idx = arena.push_node(NO_PARENT, root_name, 0);

        let a_name = arena.intern(b"var");
        let a_idx = arena.push_node(root_idx, a_name, 1);

        let b_name = arena.intern(b"log");
        let b_idx = arena.push_node(a_idx, b_name, 2);
        arena.hot[b_idx as usize].total_bytes = 500;
        arena.hot[b_idx as usize].file_count = 3;

        let path = arena.materialize_path(b_idx);
        assert_eq!(path, b"/var/log");
    }

    #[test]
    fn test_hot_cold_split() {
        let mut arena = PathlessArena::with_capacity(16, 256);
        let name = arena.intern(b"test");
        let idx = arena.push_node(NO_PARENT, name, 0);

        // Hot data
        arena.hot[idx as usize].total_bytes = 42;
        arena.hot[idx as usize].file_count = 5;

        // Cold data
        arena.cold[idx as usize].mtime = 1234567890;
        arena.cold[idx as usize].identity = FsIdentity { dev: 1, ino: 99 };

        assert_eq!(arena.hot[idx as usize].total_bytes, 42);
        assert_eq!(arena.cold[idx as usize].mtime, 1234567890);
        assert_eq!(arena.cold[idx as usize].identity.ino, 99);
    }

    #[test]
    fn test_dir_node_compat() {
        let mut arena = PathlessArena::with_capacity(16, 256);
        let name = arena.intern(b"compat");
        let idx = DirNode {
            parent: NO_PARENT,
            name,
            total_bytes: 100,
            file_count: 3,
            mtime: 999,
            depth: 0,
            identity: FsIdentity { dev: 1, ino: 50 },
        }
        .push_into(&mut arena);

        assert_eq!(arena.hot[idx as usize].total_bytes, 100);
        assert_eq!(arena.cold[idx as usize].mtime, 999);
        assert_eq!(arena.cold[idx as usize].identity.ino, 50);
    }

    #[test]
    fn test_node_count() {
        let mut arena = PathlessArena::with_capacity(16, 256);
        assert_eq!(arena.node_count(), 0);
        let name = arena.intern(b"test");
        arena.push_node(NO_PARENT, name, 0);
        assert_eq!(arena.node_count(), 1);
        arena.push_node(0, name, 1);
        assert_eq!(arena.node_count(), 2);
    }

    #[test]
    fn test_resolve_name() {
        let mut arena = PathlessArena::with_capacity(16, 256);
        let sym = arena.intern(b"hello");
        assert_eq!(arena.resolve_name(sym), b"hello");
    }
}
