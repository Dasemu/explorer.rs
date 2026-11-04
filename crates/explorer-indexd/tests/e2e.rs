// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end: daemon + IPC server + client over a Unix socket, with a
//! walked temporary tree. (On Windows the same path runs over named pipes;
//! that is covered by docs/TESTING-INDEX-ON-WINDOWS.md.)
#![cfg(unix)]

use std::io::Write;
use std::time::Duration;

use explorer_core::config::IndexConfig;
use explorer_index::protocol::{read_frame, ErrorCode, Response, MAX_RESPONSE_FRAME};
use explorer_index::transport::{self, Access, Listener};
use explorer_index_client::{ClientOptions, IndexClient, Outcome, QueryFlags};
use explorer_indexd::{server, Daemon};

#[test]
fn daemon_end_to_end() {
    let d = tempfile::tempdir().unwrap();
    let data = d.path().join("data");
    for i in 0..30 {
        let dir = data.join(format!("proj{i}")).join("src");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join(format!("notes-{i}.txt")), "x".repeat(i)).unwrap();
    }
    let cfg = IndexConfig {
        rescan_interval_s: 0,
        max_query_results: 25,
        ..Default::default()
    };
    let daemon = Daemon::new(cfg, d.path().join("state/index.snapshot"));
    daemon.start_roots(&[data.to_string_lossy().to_string()]);
    let ep = d.path().join("indexd.sock").to_string_lossy().to_string();
    let listener = Listener::bind(&ep, Access::CurrentUser).unwrap();
    let srv = {
        let daemon = daemon.clone();
        std::thread::spawn(move || server::serve(daemon, listener))
    };
    let client = IndexClient::new(ClientOptions {
        endpoint: ep.clone(),
        request_timeout: Duration::from_secs(10),
        ..Default::default()
    });
    let wait = Duration::from_secs(10);
    for _ in 0..500 {
        if let Outcome::Ready(s) = client.status().wait(wait) {
            if !s.scanning && s.entries > 0 {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let st = client.status().wait(wait).ok().unwrap();
    assert_eq!(st.entries, 1 + 30 * 4, "{st:?}");

    // Query + paging (limit capped by max_query_results = 25).
    let r = client
        .query("main.rs", 0, 100, QueryFlags::default(), None)
        .wait(wait)
        .ok()
        .unwrap();
    assert_eq!(r.total, 30);
    assert_eq!(r.hits.len(), 25);
    assert!(r.more);
    let r2 = client
        .query("main.rs", 25, 25, QueryFlags::default(), None)
        .wait(wait)
        .ok()
        .unwrap();
    assert_eq!(r2.hits.len(), 5);
    assert!(!r2.more);
    let r = client
        .query(
            "notes ext:txt size:>=29",
            0,
            10,
            QueryFlags::default(),
            None,
        )
        .wait(wait)
        .ok()
        .unwrap();
    assert_eq!(r.hits.len(), 1);
    assert!(r.hits[0].path.ends_with("notes-29.txt"));

    // Bad query is an error, not a crash.
    let bad = client
        .query("size:>huge", 0, 10, QueryFlags::default(), None)
        .wait(wait);
    assert!(matches!(bad, Outcome::Failed(ErrorCode::BadQuery, _)));

    // Garbage on a raw connection: the daemon answers with an error and
    // keeps serving others.
    let mut raw = transport::connect(&ep, Duration::from_secs(1)).unwrap();
    raw.writer
        .write_all(&[5, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
        .unwrap();
    match read_frame::<Response>(&mut raw.reader, MAX_RESPONSE_FRAME).unwrap() {
        Response::Error { code, .. } => assert_eq!(code, ErrorCode::BadRequest),
        other => panic!("unexpected {other:?}"),
    }
    let mut raw = transport::connect(&ep, Duration::from_secs(1)).unwrap();
    raw.writer.write_all(&u32::MAX.to_le_bytes()).unwrap();
    assert!(read_frame::<Response>(&mut raw.reader, MAX_RESPONSE_FRAME).is_ok());

    // Live root management.
    let extra = d.path().join("extra");
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::write(extra.join("unique-needle.bin"), "").unwrap();
    assert_eq!(
        client.add_root(&extra.to_string_lossy()).wait(wait),
        Outcome::Ready(())
    );
    let mut found = false;
    for _ in 0..500 {
        let r = client
            .query("unique-needle", 0, 5, QueryFlags::default(), None)
            .wait(wait);
        if matches!(&r, Outcome::Ready(r) if r.total == 1) {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(found);
    assert_eq!(client.reindex(None).wait(wait), Outcome::Ready(()));
    assert_eq!(
        client.remove_root(&extra.to_string_lossy()).wait(wait),
        Outcome::Ready(())
    );
    assert!(matches!(
        client.remove_root("/not/indexed").wait(wait),
        Outcome::Failed(ErrorCode::NotFound, _)
    ));

    // Shutdown over IPC stops the server loop.
    assert_eq!(client.shutdown_daemon().wait(wait), Outcome::Ready(()));
    srv.join().unwrap();
    daemon.shutdown();
    assert!(d.path().join("state/index.snapshot").exists());
}
