//! DNS rebinding: a guarded executor connects only to addresses its guard validated.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::net::TcpListener;

use crate::executor::send::execute_request;
use crate::executor::types::{ExecutionError, ExecutorOptions, ResolveGuard};
use spanbay_model::Request;

/// A listener on a loopback port that counts the connections it receives.
async fn counting_listener() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback listener should bind");
    let port = listener.local_addr().expect("bound address").port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buffer = [0_u8; 1024];
                let _ = stream.read(&mut buffer).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok",
                    )
                    .await;
            });
        }
    });
    (port, accepted)
}

fn request_to(host: &str, port: u16) -> Request {
    Request::new(
        "req_rebind",
        "ws_billing",
        "Rebind",
        format!("http://{host}:{port}/"),
    )
    .expect("request should be valid")
}

/// A guard whose name lookup returns `answers` in order (the last one repeats) and counts calls.
fn guard_answering(
    private_hosts: Option<Vec<&str>>,
    answers: Vec<IpAddr>,
) -> (ResolveGuard, Arc<AtomicUsize>) {
    let lookups = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&lookups);
    let guard = ResolveGuard {
        private_hosts: private_hosts
            .map(|hosts| Arc::new(hosts.into_iter().map(str::to_owned).collect::<Vec<_>>())),
        lookup: Some(Arc::new(move |_| {
            let index = counter
                .fetch_add(1, Ordering::SeqCst)
                .min(answers.len() - 1);
            vec![answers[index]]
        })),
    };
    (guard, lookups)
}

fn ip(text: &str) -> IpAddr {
    text.parse().expect("a literal address")
}

#[tokio::test]
async fn resolve_guard_pins_validated_lookup_and_refuses_metadata_and_undeclared_private_hosts() {
    let (port, accepted) = counting_listener().await;

    let (guard, lookups) = guard_answering(Some(vec!["rebind.test"]), vec![ip("169.254.169.254")]);
    let err = execute_request(
        &request_to("rebind.test", port),
        None,
        ExecutorOptions {
            resolve_guard: Some(guard),
            ..ExecutorOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, ExecutionError::GuardRefused { host } if host == "rebind.test")
            && err.to_string().contains("refusing to connect")
    );
    assert_eq!(
        (
            lookups.load(Ordering::SeqCst),
            accepted.load(Ordering::SeqCst)
        ),
        (1, 0)
    );

    let (guard, _) = guard_answering(Some(vec!["other.test"]), vec![ip("127.0.0.1")]);
    let err = execute_request(
        &request_to("rebind.test", port),
        None,
        ExecutorOptions {
            resolve_guard: Some(guard),
            ..ExecutorOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("refusing to connect") && accepted.load(Ordering::SeqCst) == 0
    );

    let (guard, lookups) = guard_answering(
        Some(vec!["rebind-ok.test"]),
        vec![ip("127.0.0.1"), ip("169.254.169.254")],
    );
    let ok = execute_request(
        &request_to("rebind-ok.test", port),
        None,
        ExecutorOptions {
            resolve_guard: Some(guard),
            ..ExecutorOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        (
            ok.status_code,
            lookups.load(Ordering::SeqCst),
            accepted.load(Ordering::SeqCst)
        ),
        (200, 1, 1)
    );

    let strict = ResolveGuard {
        private_hosts: Some(Arc::new(Vec::new())),
        ..ResolveGuard::default()
    };
    assert!(
        execute_request(
            &request_to("127.0.0.1", port),
            None,
            ExecutorOptions {
                resolve_guard: Some(strict),
                ..ExecutorOptions::default()
            }
        )
        .await
        .is_err()
    );

    let mut retry_name = request_to("rebind.test", port);
    retry_name.network_options.retry_count = 3;
    let (guard, lookups) = guard_answering(None, vec![ip("169.254.169.254")]);
    let err = execute_request(
        &retry_name,
        None,
        ExecutorOptions {
            resolve_guard: Some(guard),
            ..ExecutorOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, ExecutionError::GuardRefused { host } if host == "rebind.test")
            && lookups.load(Ordering::SeqCst) == 1
    );

    let mut literal = request_to("169.254.169.254", 80);
    literal.network_options.retry_count = 3;
    let err = execute_request(
        &literal,
        None,
        ExecutorOptions {
            resolve_guard: Some(ResolveGuard::default()),
            ..ExecutorOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, ExecutionError::GuardRefused { host } if host == "169.254.169.254")
            && err.attempt_count() == 1
    );

    let unguarded = execute_request(
        &request_to("127.0.0.1", port),
        None,
        ExecutorOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        (unguarded.status_code, accepted.load(Ordering::SeqCst)),
        (200, 2)
    );
}
