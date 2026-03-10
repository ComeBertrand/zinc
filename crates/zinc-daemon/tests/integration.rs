use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use zinc_daemon::daemon::Daemon;
use zinc_proto::{Request, Response};

/// Send a request and read the response over a Unix socket.
async fn send(stream: &mut UnixStream, request: &Request) -> Response {
    let mut json = serde_json::to_string(request).unwrap();
    json.push('\n');
    stream.write_all(json.as_bytes()).await.unwrap();

    let mut reader = BufReader::new(&mut *stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

/// Start a daemon on a temp socket and return the socket path.
/// The daemon runs in a background task and is dropped when the test ends.
async fn start_daemon(dir: &std::path::Path) -> PathBuf {
    let sock = dir.join("test.sock");
    let daemon = Daemon::new(sock.clone());
    tokio::spawn(async move {
        let _ = daemon.run().await;
    });
    // Wait for socket to appear
    for _ in 0..40 {
        if UnixStream::connect(&sock).await.is_ok() {
            return sock;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("daemon did not start");
}

#[tokio::test]
async fn list_empty() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let resp = send(&mut stream, &Request::List).await;
    match resp {
        Response::Agents { agents } => assert!(agents.is_empty()),
        other => panic!("expected Agents, got {:?}", other),
    }
}

#[tokio::test]
async fn spawn_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Spawn a long-lived process
    let resp = send(
        &mut stream,
        &Request::Spawn {
            provider: "sleep".into(),
            dir: PathBuf::from("/tmp"),
            id: Some("test-agent".into()),
            args: vec!["3600".into()],
        },
    )
    .await;
    match &resp {
        Response::Spawned { id } => assert_eq!(id, "test-agent"),
        other => panic!("expected Spawned, got {:?}", other),
    }

    // List should show the agent
    let resp = send(&mut stream, &Request::List).await;
    match resp {
        Response::Agents { agents } => {
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0].id, "test-agent");
            assert_eq!(agents[0].provider, "sleep");
        }
        other => panic!("expected Agents, got {:?}", other),
    }
}

#[tokio::test]
async fn spawn_duplicate_id() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let req = Request::Spawn {
        provider: "sleep".into(),
        dir: PathBuf::from("/tmp"),
        id: Some("dupe".into()),
        args: vec!["3600".into()],
    };

    let resp = send(&mut stream, &req).await;
    assert!(matches!(resp, Response::Spawned { .. }));

    let resp = send(&mut stream, &req).await;
    match resp {
        Response::Error { message } => assert!(message.contains("already exists")),
        other => panic!("expected Error, got {:?}", other),
    }
}

#[tokio::test]
async fn spawn_invalid_directory() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let resp = send(
        &mut stream,
        &Request::Spawn {
            provider: "sleep".into(),
            dir: PathBuf::from("/nonexistent/path/that/does/not/exist"),
            id: Some("bad-dir".into()),
            args: vec!["1".into()],
        },
    )
    .await;
    match resp {
        Response::Error { message } => assert!(message.contains("directory"), "{}", message),
        other => panic!("expected Error, got {:?}", other),
    }
}

#[tokio::test]
async fn kill_agent() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send(
        &mut stream,
        &Request::Spawn {
            provider: "sleep".into(),
            dir: PathBuf::from("/tmp"),
            id: Some("to-kill".into()),
            args: vec!["3600".into()],
        },
    )
    .await;

    let resp = send(
        &mut stream,
        &Request::Kill {
            id: "to-kill".into(),
        },
    )
    .await;
    assert!(matches!(resp, Response::Ok));

    let resp = send(&mut stream, &Request::List).await;
    match resp {
        Response::Agents { agents } => assert!(agents.is_empty()),
        other => panic!("expected empty Agents, got {:?}", other),
    }
}

#[tokio::test]
async fn kill_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let resp = send(&mut stream, &Request::Kill { id: "nope".into() }).await;
    match resp {
        Response::Error { message } => assert!(message.contains("not found")),
        other => panic!("expected Error, got {:?}", other),
    }
}

#[tokio::test]
async fn auto_generated_ids() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let resp = send(
        &mut stream,
        &Request::Spawn {
            provider: "sleep".into(),
            dir: PathBuf::from("/tmp"),
            id: None,
            args: vec!["3600".into()],
        },
    )
    .await;
    match &resp {
        Response::Spawned { id } => assert_eq!(id, "agent-1"),
        other => panic!("expected Spawned, got {:?}", other),
    }

    let resp = send(
        &mut stream,
        &Request::Spawn {
            provider: "sleep".into(),
            dir: PathBuf::from("/tmp"),
            id: None,
            args: vec!["3600".into()],
        },
    )
    .await;
    match &resp {
        Response::Spawned { id } => assert_eq!(id, "agent-2"),
        other => panic!("expected Spawned, got {:?}", other),
    }
}

#[tokio::test]
async fn malformed_json() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    stream.write_all(b"this is not json\n").await.unwrap();

    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: Response = serde_json::from_str(line.trim()).unwrap();
    match resp {
        Response::Error { message } => assert!(message.contains("invalid request")),
        other => panic!("expected Error, got {:?}", other),
    }
}

#[tokio::test]
async fn shutdown_kills_all() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Spawn two agents
    send(
        &mut stream,
        &Request::Spawn {
            provider: "sleep".into(),
            dir: PathBuf::from("/tmp"),
            id: Some("a".into()),
            args: vec!["3600".into()],
        },
    )
    .await;
    send(
        &mut stream,
        &Request::Spawn {
            provider: "sleep".into(),
            dir: PathBuf::from("/tmp"),
            id: Some("b".into()),
            args: vec!["3600".into()],
        },
    )
    .await;

    let resp = send(&mut stream, &Request::Shutdown).await;
    assert!(matches!(resp, Response::Ok));
}

#[tokio::test]
async fn exited_process_is_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // 'true' exits immediately with code 0
    send(
        &mut stream,
        &Request::Spawn {
            provider: "true".into(),
            dir: PathBuf::from("/tmp"),
            id: Some("quick".into()),
            args: vec![],
        },
    )
    .await;

    // Give it a moment to exit
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Exited agents are removed — exit is an event, not a state
    let resp = send(&mut stream, &Request::List).await;
    match resp {
        Response::Agents { agents } => assert!(agents.is_empty()),
        other => panic!("expected empty Agents, got {:?}", other),
    }
}

#[tokio::test]
async fn failed_process_is_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // 'false' exits immediately with code 1
    send(
        &mut stream,
        &Request::Spawn {
            provider: "false".into(),
            dir: PathBuf::from("/tmp"),
            id: Some("fail".into()),
            args: vec![],
        },
    )
    .await;

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Exited agents are removed regardless of exit code
    let resp = send(&mut stream, &Request::List).await;
    match resp {
        Response::Agents { agents } => assert!(agents.is_empty()),
        other => panic!("expected empty Agents, got {:?}", other),
    }
}

#[tokio::test]
async fn attach_nonexistent_agent() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let resp = send(
        &mut stream,
        &Request::Attach {
            id: "nope".into(),
            cols: 80,
            rows: 24,
        },
    )
    .await;
    match resp {
        Response::Error { message } => assert!(message.contains("not found")),
        other => panic!("expected Error, got {:?}", other),
    }
}

#[tokio::test]
async fn attach_receives_scrollback() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Spawn 'echo hello' — it will produce output then exit
    send(
        &mut stream,
        &Request::Spawn {
            provider: "bash".into(),
            dir: PathBuf::from("/tmp"),
            id: Some("echo-test".into()),
            args: vec!["-c".into(), "echo hello".into()],
        },
    )
    .await;

    // Wait for output to be captured in scrollback
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Attach on a fresh connection (the current one will switch to raw mode)
    let mut attach_stream = UnixStream::connect(&sock).await.unwrap();
    let mut json = serde_json::to_string(&Request::Attach {
        id: "echo-test".into(),
        cols: 80,
        rows: 24,
    })
    .unwrap();
    json.push('\n');
    attach_stream.write_all(json.as_bytes()).await.unwrap();

    // Read the JSON response line
    let mut reader = BufReader::new(&mut attach_stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: Response = serde_json::from_str(line.trim()).unwrap();
    assert!(matches!(resp, Response::Attached));

    // Read scrollback — should contain "hello"
    let mut buf = [0u8; 4096];
    let n = tokio::time::timeout(std::time::Duration::from_secs(2), reader.read(&mut buf))
        .await
        .expect("timed out reading scrollback")
        .expect("read failed");

    let output = String::from_utf8_lossy(&buf[..n]);
    assert!(output.contains("hello"), "scrollback was: {:?}", output);
}

#[tokio::test]
async fn attach_relays_input_and_output() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_daemon(dir.path()).await;
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Spawn 'cat' — it echoes stdin to stdout
    send(
        &mut stream,
        &Request::Spawn {
            provider: "cat".into(),
            dir: PathBuf::from("/tmp"),
            id: Some("cat-test".into()),
            args: vec![],
        },
    )
    .await;

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Attach on a fresh connection
    let mut attach_stream = UnixStream::connect(&sock).await.unwrap();
    let mut json = serde_json::to_string(&Request::Attach {
        id: "cat-test".into(),
        cols: 80,
        rows: 24,
    })
    .unwrap();
    json.push('\n');
    attach_stream.write_all(json.as_bytes()).await.unwrap();

    // Read attached response
    let mut reader = BufReader::new(&mut attach_stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: Response = serde_json::from_str(line.trim()).unwrap();
    assert!(matches!(resp, Response::Attached));

    // Send some input (write directly to the underlying stream)
    let stream_ref = reader.get_mut();
    stream_ref.write_all(b"ping\n").await.unwrap();

    // Read back the echoed output (PTY echo + cat echo)
    let mut buf = [0u8; 4096];
    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);

    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), reader.read(&mut buf))
            .await
        {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                collected.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&collected);
                if text.contains("ping") {
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => break, // timeout
        }
    }

    let output = String::from_utf8_lossy(&collected);
    assert!(
        output.contains("ping"),
        "expected 'ping' in output, got: {:?}",
        output
    );
}
