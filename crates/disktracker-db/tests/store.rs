use chrono::{TimeZone, Utc};
use disktracker_db::open_db;
use disktracker_db::store::{insert_snapshot, list_snapshots, resolve_snapshot_ref};
use tempfile::tempdir;

#[test]
fn resolve_snapshot_ref_by_id_and_date() {
    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("db.sqlite")).unwrap();

    let now = Utc::now().timestamp();
    let snap_id = insert_snapshot(&conn, "/tmp", now, now, 1, 10, 0).unwrap();

    let resolved_id = resolve_snapshot_ref(&conn, &snap_id.to_string()).unwrap();
    assert_eq!(resolved_id, snap_id);

    let date = Utc
        .timestamp_opt(now, 0)
        .single()
        .unwrap()
        .date_naive()
        .to_string();
    let resolved_date = resolve_snapshot_ref(&conn, &date).unwrap();
    assert_eq!(resolved_date, snap_id);

    let snapshots = list_snapshots(&conn).unwrap();
    assert_eq!(snapshots.len(), 1);
}
