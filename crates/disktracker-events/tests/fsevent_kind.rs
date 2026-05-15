use disktracker_events::FsEventKind;

#[test]
fn from_u8_maps_known_values() {
    assert_eq!(FsEventKind::from_u8(0), FsEventKind::Create);
    assert_eq!(FsEventKind::from_u8(1), FsEventKind::Delete);
    assert_eq!(FsEventKind::from_u8(2), FsEventKind::Modify);
    assert_eq!(FsEventKind::from_u8(3), FsEventKind::Rename);
    assert_eq!(FsEventKind::from_u8(4), FsEventKind::Overflow);
    assert_eq!(FsEventKind::from_u8(250), FsEventKind::Other);
}
