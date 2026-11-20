use neochat::storage::{Connection, OptionalExtension};
use std::time::Duration;

#[test]
fn writer_lock_rollback_foreign_keys_and_connection_local_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.db");
    let mut a = Connection::open(&path).unwrap();
    let b = Connection::open(&path).unwrap();
    b.busy_timeout(Duration::from_millis(20)).unwrap();
    a.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE parent(id INTEGER PRIMARY KEY); CREATE TABLE child(pid INTEGER REFERENCES parent(id) ON DELETE CASCADE);").unwrap();
    b.execute("INSERT INTO parent VALUES (12)", []).unwrap();
    {
        let tx = a.transaction().unwrap();
        tx.execute("INSERT INTO parent DEFAULT VALUES", []).unwrap();
        assert_eq!(tx.last_insert_rowid(), 13);
        assert_eq!(b.last_insert_rowid(), 12);
        assert!(matches!(b.execute("INSERT INTO parent VALUES (99)", []), Err(turso::Error::Busy(_))));
        assert!(matches!(tx.execute("INSERT INTO child VALUES (99)", []), Err(turso::Error::Constraint(_))));
    }
    assert_eq!(b.query_row("SELECT id FROM parent WHERE id=13", [], |r| r.get::<_,u64>(0)).optional().unwrap(), None);
    a.execute_batch("INSERT INTO child VALUES (12); DELETE FROM parent WHERE id=12;").unwrap();
    assert_eq!(a.query_row("SELECT COUNT(*) FROM child", [], |r| r.get::<_,u64>(0)).unwrap(), 0);
    assert!(a.query_row("SELECT -1", [], |r| r.get::<_,u64>(0)).optional().is_err());
}