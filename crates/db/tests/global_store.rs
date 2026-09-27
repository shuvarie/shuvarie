use std::collections::HashSet;
use std::path::Path;

use shuvarie_db::{DbError, GLOBAL_DB_FILE, SESSION_DIR_MAP_FILE, SessionDirMap, Store};

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn canonical(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[test]
fn load_missing_file_yields_empty_map() {
    let dir = tempfile::tempdir().unwrap();
    let map = SessionDirMap::load(&dir.path().join("session-dir.kdl")).unwrap();
    assert!(map.claimed_ids().is_empty());
    assert!(map.sessions_for(Path::new("/nowhere")).is_empty());
}

#[test]
fn claim_unclaim_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let map_path = dir.path().join("session-dir.kdl");
    let map = SessionDirMap::load(&map_path).unwrap();

    let a = uuid::Uuid::now_v7();
    let b = uuid::Uuid::now_v7();
    let workspace = dir.path().join("project-a");
    map.claim(&workspace, a).unwrap();
    map.claim(&workspace, b).unwrap();
    map.claim(&workspace, a).unwrap();
    let claimed: HashSet<_> = map.sessions_for(&workspace).into_iter().collect();
    assert_eq!(claimed, HashSet::from([a, b]), "claims dedupe");

    // Another directory sees nothing.
    assert!(map.sessions_for(&dir.path().join("project-b")).is_empty());
    assert_eq!(map.claimed_ids(), HashSet::from([a, b]));

    // A clone shares the state.
    let clone = map.clone();
    assert_eq!(clone.sessions_for(&workspace).len(), 2);

    map.unclaim(a).unwrap();
    assert_eq!(map.sessions_for(&workspace), vec![b]);
    map.unclaim(b).unwrap();
    assert!(map.sessions_for(&workspace).is_empty());
    // Unclaiming an unknown id is a no-op.
    map.unclaim(uuid::Uuid::now_v7()).unwrap();
}

#[test]
fn claim_writes_the_documented_shape() {
    let dir = tempfile::tempdir().unwrap();
    let map_path = dir.path().join("session-dir.kdl");
    let map = SessionDirMap::load(&map_path).unwrap();

    let id = uuid::Uuid::now_v7();
    map.claim(Path::new("/path/to/a/project"), id).unwrap();
    map.claim(Path::new("/another/path"), uuid::Uuid::now_v7())
        .unwrap();

    let text = std::fs::read_to_string(&map_path).unwrap();
    assert!(
        text.contains(&format!(
            "  dir \"/path/to/a/project\" {{\n    session \"{id}\"\n  }}"
        )),
        "the documented shape must be kept: {text}"
    );
    assert!(text.starts_with("maps {\n"), "{text}");
    assert!(text.ends_with("}\n"), "{text}");

    // Reloading the written file gives the same claims.
    let reloaded = SessionDirMap::load(&map_path).unwrap();
    assert_eq!(
        reloaded.sessions_for(Path::new("/path/to/a/project")),
        vec![id]
    );
}

#[test]
fn empty_map_writes_an_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let map_path = dir.path().join("session-dir.kdl");
    let map = SessionDirMap::load(&map_path).unwrap();
    map.claim(dir.path(), uuid::Uuid::now_v7()).unwrap();
    map.unclaim(uuid::Uuid::now_v7()).unwrap();
    // A claim from another source still stands; drop it to empty the file.
    let id = map.sessions_for(dir.path())[0];
    map.unclaim(id).unwrap();
    assert_eq!(std::fs::read_to_string(&map_path).unwrap(), "");
}

#[test]
fn prune_missing_drops_dead_claims() {
    let dir = tempfile::tempdir().unwrap();
    let map_path = dir.path().join("session-dir.kdl");
    let map = SessionDirMap::load(&map_path).unwrap();
    let alive = uuid::Uuid::now_v7();
    let gone = uuid::Uuid::now_v7();
    map.claim(dir.path(), alive).unwrap();
    map.claim(dir.path(), gone).unwrap();

    map.prune_missing(&HashSet::from([alive])).unwrap();
    assert_eq!(map.sessions_for(dir.path()), vec![alive]);
}

#[test]
fn malformed_maps_error() {
    let cases = [
        "bogus {}",
        "maps { dir }",
        "maps { dir 42 }",
        "maps { dir \"/x\" { session \"not-a-uuid\" } }",
        "maps { dir \"/a\" {} dir \"/a\" {} }",
        "maps { maps {} }",
    ];
    for contents in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-dir.kdl");
        std::fs::write(&path, contents).unwrap();
        assert!(
            SessionDirMap::load(&path).is_err(),
            "`{contents}` must not parse"
        );
    }
}

#[tokio::test]
async fn open_global_purges_unclaimed_sessions() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    // Pre-existing sessions, created through a plain (non-global) store: the
    // shared DB may already hold rows before `global-store` was ever enabled.
    let mut pre = Store::open(&data.path().join(GLOBAL_DB_FILE))
        .await
        .unwrap();
    let claimed = pre
        .create_session("claimed", None, None, None)
        .await
        .unwrap();
    let orphan = pre
        .create_session("orphan", None, None, None)
        .await
        .unwrap();
    let locked = pre
        .create_session("locked", None, None, None)
        .await
        .unwrap();
    let worker_of_claimed = pre
        .create_worker_session("w", None, None, claimed)
        .await
        .unwrap();
    let worker_of_orphan = pre
        .create_worker_session("w", None, None, orphan)
        .await
        .unwrap();
    let dangling_worker = pre
        .create_worker_session("dangling", None, None, uuid::Uuid::now_v7())
        .await
        .unwrap();
    let mut pre = pre.with_client_id("other-instance");
    pre.acquire_session_lock(locked, now_ms()).await.unwrap();

    let ws = canonical(workspace.path());
    std::fs::write(
        data.path().join(SESSION_DIR_MAP_FILE),
        format!("maps {{\n  dir \"{ws}\" {{\n    session \"{claimed}\"\n  }}\n}}\n"),
    )
    .unwrap();

    let mut store = Store::open_global_in(data.path(), workspace.path())
        .await
        .unwrap();

    // Claimed session (and its worker) survive.
    store.load_session(claimed).await.unwrap();
    store.load_session(worker_of_claimed).await.unwrap();

    // Unclaimed main is purged, along with its worker.
    assert!(matches!(
        store.load_session(orphan).await.unwrap_err(),
        DbError::NotFound { .. }
    ));
    assert!(matches!(
        store.load_session(worker_of_orphan).await.unwrap_err(),
        DbError::NotFound { .. }
    ));
    assert!(matches!(
        store.load_session(dangling_worker).await.unwrap_err(),
        DbError::NotFound { .. }
    ));

    // An in-use (locked) orphan is spared until its holder lets go.
    store.load_session(locked).await.unwrap();

    // The map keeps the valid claim; nothing was pruned.
    let map = SessionDirMap::load(&data.path().join(SESSION_DIR_MAP_FILE)).unwrap();
    assert_eq!(map.sessions_for(workspace.path()), vec![claimed]);
}

#[tokio::test]
async fn open_global_prunes_claims_of_missing_sessions() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let mut pre = Store::open(&data.path().join(GLOBAL_DB_FILE))
        .await
        .unwrap();
    let deleted_earlier = pre.create_session("gone", None, None, None).await.unwrap();
    pre.delete_session(deleted_earlier).await.unwrap();
    let alive = pre.create_session("alive", None, None, None).await.unwrap();
    drop(pre);

    let ws = canonical(workspace.path());
    std::fs::write(
        data.path().join(SESSION_DIR_MAP_FILE),
        format!(
            "maps {{\n  dir \"{ws}\" {{\n    session \"{deleted_earlier}\"\n    session \"{alive}\"\n  }}\n}}\n"
        ),
    )
    .unwrap();

    Store::open_global_in(data.path(), workspace.path())
        .await
        .unwrap();
    let map = SessionDirMap::load(&data.path().join(SESSION_DIR_MAP_FILE)).unwrap();
    assert_eq!(
        map.sessions_for(workspace.path()),
        vec![alive],
        "the stale claim must be pruned at launch"
    );
}

#[tokio::test]
async fn malformed_map_aborts_the_purge() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let mut pre = Store::open(&data.path().join(GLOBAL_DB_FILE))
        .await
        .unwrap();
    let orphan = pre
        .create_session("orphan", None, None, None)
        .await
        .unwrap();
    drop(pre);

    std::fs::write(data.path().join(SESSION_DIR_MAP_FILE), "bogus {}").unwrap();

    let err = match Store::open_global_in(data.path(), workspace.path()).await {
        Err(err) => err,
        Ok(_) => panic!("a malformed map must abort the open"),
    };
    assert!(matches!(err, DbError::Map(_)), "{err:?}");

    // Nothing was purged: a malformed map must never be treated as empty.
    let mut check = Store::open(&data.path().join(GLOBAL_DB_FILE))
        .await
        .unwrap();
    check.load_session(orphan).await.unwrap();
}

#[tokio::test]
async fn sessions_are_claimed_and_listed_per_workspace() {
    let data = tempfile::tempdir().unwrap();
    let workspace_a = tempfile::tempdir().unwrap();
    let workspace_b = tempfile::tempdir().unwrap();

    let mut a = Store::open_global_in(data.path(), workspace_a.path())
        .await
        .unwrap();
    let first = a.create_session("first", None, None, None).await.unwrap();
    let second = a.create_session("second", None, None, None).await.unwrap();

    // Listing is scoped to this workspace.
    let list = a.list_sessions().await.unwrap();
    assert_eq!(
        list.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![second, first]
    );
    let recent = a.most_recent_session().await.unwrap().unwrap();
    assert_eq!(recent.id, second);

    // A different workspace sees none of them.
    let mut b = Store::open_global_in(data.path(), workspace_b.path())
        .await
        .unwrap();
    assert!(b.list_sessions().await.unwrap().is_empty());
    assert!(b.most_recent_session().await.unwrap().is_none());

    // Deleting unclaims.
    a.delete_session(first).await.unwrap();
    let map = SessionDirMap::load(&data.path().join(SESSION_DIR_MAP_FILE)).unwrap();
    assert_eq!(map.sessions_for(workspace_a.path()), vec![second]);

    // The workspace-local store is untouched by global-mode runs: no
    // workspace `data.db` was ever created here.
    assert!(
        !std::env::current_dir()
            .unwrap()
            .join(shuvarie_db::WORKSPACE_DIR_NAME)
            .join("data.db")
            .exists()
    );
}

#[tokio::test]
async fn imports_are_claimed_for_the_workspace() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let mut local = Store::open_in_memory().await.unwrap();
    let id = local
        .create_session("exported", None, None, None)
        .await
        .unwrap();
    let stored = local.load_session(id).await.unwrap();
    let file = shuvarie_db::SessionFile::from_stored(&stored);

    let mut global = Store::open_global_in(data.path(), workspace.path())
        .await
        .unwrap();
    let imported = global.import_session(&file).await.unwrap();

    let map = SessionDirMap::load(&data.path().join(SESSION_DIR_MAP_FILE)).unwrap();
    assert_eq!(map.sessions_for(workspace.path()), vec![imported]);
    assert_eq!(global.list_sessions().await.unwrap().len(), 1);
}

#[tokio::test]
async fn local_mode_never_touches_the_data_dir() {
    let data = tempfile::tempdir().unwrap();
    let mut store = Store::open_in_memory().await.unwrap();
    store.create_session("t", None, None, None).await.unwrap();
    assert_eq!(store.list_sessions().await.unwrap().len(), 1);
    assert_eq!(
        store.most_recent_session().await.unwrap().unwrap().title,
        "t"
    );
    // No data-dir file was created or touched by the workspace-local store.
    assert!(data.path().read_dir().unwrap().next().is_none());
}
