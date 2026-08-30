use phora::source::{
    GitBackend, MirrorKey, SourceName, SourceStore, WorktreeAdminId, WorktreeMirrorAddress,
    WorktreeObservationLevel, WorktreeObservationLock, WorktreeObservationRequest,
    WorktreeObservationResult,
};

#[test]
fn missing_observation_lock_is_stale_without_creating_cache_state() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cache_git_root = temp.path().join("cache");
    let backend = GitBackend::new(cache_git_root.clone());
    let address = WorktreeMirrorAddress {
        cache_git_root: cache_git_root.clone(),
        key: "a1b2c3d4e5f60708".parse::<MirrorKey>().expect("mirror key"),
    };
    let request = WorktreeObservationRequest {
        source: "dots".parse::<SourceName>().expect("source name"),
        address,
        admin_id: "0123456789abcdef"
            .parse::<WorktreeAdminId>()
            .expect("worktree admin id"),
        deploy_root: temp.path().join("deployment"),
        lock: WorktreeObservationLock::Try,
        level: WorktreeObservationLevel::Cheap,
    };

    assert_eq!(
        SourceStore::observe_worktree(&backend, &request).expect("non-blocking observation"),
        WorktreeObservationResult::Stale,
        "a missing lock is stale for non-blocking observation"
    );
    assert!(
        !cache_git_root.exists(),
        "observation must not create cache or lock state"
    );

    let mut wait_request = request;
    wait_request.lock = WorktreeObservationLock::Wait;
    assert_eq!(
        SourceStore::observe_worktree(&backend, &wait_request).expect("waiting observation"),
        WorktreeObservationResult::Stale,
        "a missing lock is stale for waiting observation"
    );
    assert!(
        !cache_git_root.exists(),
        "waiting observation must not create cache or lock state"
    );
}
