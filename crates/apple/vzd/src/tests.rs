use crate::server::VzdServer;

#[test]
fn first_handle_id_is_one() {
    let server = VzdServer::new();
    assert_eq!(server.next_handle_id().0, 1);
}

#[test]
fn next_handle_id_increments() {
    let server = VzdServer::new();

    let id1 = server.next_handle_id();
    let id2 = server.next_handle_id();
    let id3 = server.next_handle_id();

    assert_eq!(id1.0, 1);
    assert_eq!(id2.0, 2);
    assert_eq!(id3.0, 3);
}

#[test]
fn next_handle_id_is_thread_safe() {
    let server = VzdServer::new();
    let server1 = server.clone();
    let server2 = server.clone();

    let handle1 = std::thread::spawn(move || {
        (0..100)
            .map(|_| server1.next_handle_id().0)
            .collect::<Vec<_>>()
    });
    let handle2 = std::thread::spawn(move || {
        (0..100)
            .map(|_| server2.next_handle_id().0)
            .collect::<Vec<_>>()
    });

    let ids1 = handle1.join().unwrap();
    let ids2 = handle2.join().unwrap();

    let mut all_ids: Vec<_> = ids1.into_iter().chain(ids2).collect();
    all_ids.sort();
    all_ids.dedup();
    assert_eq!(all_ids.len(), 200);
}
