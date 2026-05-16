use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum FsEventKind {
    Create = 0,
    Delete = 1,
    Modify = 2,
    Rename = 3,
    Overflow = 4,
    Other = 5,
}

impl FsEventKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Create,
            1 => Self::Delete,
            2 => Self::Modify,
            3 => Self::Rename,
            4 => Self::Overflow,
            _ => Self::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Delete => "delete",
            Self::Modify => "modify",
            Self::Rename => "rename",
            Self::Overflow => "overflow",
            Self::Other => "other",
        }
    }
}

/// A normalized filesystem event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsEvent {
    pub timestamp: i64,
    pub kind: FsEventKind,
    /// Raw OS bytes of the affected path.
    pub path: Vec<u8>,
    pub is_dir: bool,
}

impl FsEvent {
    pub fn path_utf8(&self) -> Option<&str> {
        std::str::from_utf8(&self.path).ok()
    }
}
