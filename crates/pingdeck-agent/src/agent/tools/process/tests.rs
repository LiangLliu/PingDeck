//! Tests of the background process registry with real processes (unix).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Stdio;
use std::time::Instant;

use tokio::process::Command;

use super::*;

/// A spawn function for `sh -c script` in its own process group with piped output.
fn sh(script: &str) -> impl FnOnce() -> io::Result<Child> + use<> {
    let script = script.to_owned();
    move || {
        let mut command = Command::new("sh");
        command
            .args(["-c", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        command.spawn()
    }
}

const NOW: Duration = Duration::ZERO;

/// Polls `check` until it holds (the process machinery runs on its own tasks). The 30 s deadline
/// is only ever reached by a failing test.
async fn eventually(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !check() {
        assert!(Instant::now() < deadline, "condition not reached in 30 s");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[tokio::test]
async fn a_started_process_has_a_proc_id_and_its_output_and_exit_code_are_readable() {
    let registry = ProcessRegistry::new();
    let started = registry
        .start("s1", "echo hi; exit 3", None, sh("echo hi; exit 3"))
        .unwrap();
    assert!(
        started.id.starts_with("proc_") && started.id.len() == 17,
        "{}",
        started.id
    );
    assert!(started.id[5..].chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(started.state, ProcessState::Running);
    assert!(started.pid.is_some());

    let out = registry
        .output(Some("s1"), &started.id, 0, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(out.text, "hi\n");
    eventually(|| registry.list(Some("s1"))[0].state == ProcessState::Exited).await;
    let row = &registry.list(Some("s1"))[0];
    assert_eq!(
        (row.exit_code, row.command.as_str()),
        (Some(3), "echo hi; exit 3")
    );
    assert!(row.ended_ms.is_some());

    let again = registry
        .output(Some("s1"), &started.id, out.next_offset, NOW)
        .await
        .unwrap();
    assert_eq!(
        (again.text.as_str(), again.next_offset),
        ("", out.next_offset)
    );
    assert_eq!(again.process.state, ProcessState::Exited);
}

#[tokio::test]
async fn output_waits_for_new_bytes_and_returns_as_soon_as_they_arrive() {
    let registry = ProcessRegistry::new();
    let started = registry
        .start("s1", "sleep", None, sh("sleep 0.3; echo late; sleep 30"))
        .unwrap();
    let begun = Instant::now();
    let out = registry
        .output(Some("s1"), &started.id, 0, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(out.text, "late\n");
    // The wait was 60 s: returning in under half of that proves the bytes woke it.
    assert!(begun.elapsed() < Duration::from_secs(30));
    // Nothing new and a short wait: returns empty after the wait, still running.
    let idle = registry
        .output(
            Some("s1"),
            &started.id,
            out.next_offset,
            Duration::from_millis(100),
        )
        .await
        .unwrap();
    assert_eq!(idle.text, "");
    assert_eq!(idle.process.state, ProcessState::Running);
    registry.kill_all().await;
}

#[tokio::test]
async fn at_most_four_run_per_session_and_eight_in_the_app() {
    let registry = ProcessRegistry::new();
    for _ in 0..MAX_PER_SESSION {
        registry
            .start("a", "sleep 30", None, sh("sleep 30"))
            .unwrap();
    }
    let error = registry
        .start("a", "sleep 30", None, sh("sleep 30"))
        .unwrap_err();
    assert!(matches!(error, AppError::Conflict { .. }), "{error:?}");
    assert!(error.message().contains('4'));
    // Another session still has room until the app-wide cap.
    for _ in 0..MAX_PER_SESSION {
        registry
            .start("b", "sleep 30", None, sh("sleep 30"))
            .unwrap();
    }
    let error = registry
        .start("c", "sleep 30", None, sh("sleep 30"))
        .unwrap_err();
    assert!(error.message().contains('8'), "{error:?}");
    assert_eq!(registry.list(None).len(), MAX_TOTAL);
    assert_eq!(registry.list(Some("a")).len(), 4);

    // A finished process frees its slot.
    let victim = registry.list(Some("a"))[0].id.clone();
    registry.kill(Some("a"), &victim).await.unwrap();
    registry
        .start("a", "sleep 30", None, sh("sleep 30"))
        .unwrap();
    registry.kill_all().await;
}

#[tokio::test]
async fn a_failed_spawn_registers_nothing() {
    let registry = ProcessRegistry::new();
    let error = registry
        .start("s", "x", None, || {
            Err(io::Error::new(io::ErrorKind::NotFound, "no sh"))
        })
        .unwrap_err();
    assert!(error.message().contains("failed to start"));
    assert!(registry.list(None).is_empty());
}

#[tokio::test]
async fn kill_ends_the_whole_process_group_and_reports_it() {
    let registry = ProcessRegistry::new();
    let started = registry
        .start("s1", "group", None, sh("sleep 300 & sleep 300"))
        .unwrap();
    let group = started.pid.unwrap();
    assert!(alive(group));
    let killed = registry.kill(Some("s1"), &started.id).await.unwrap();
    assert_eq!(killed.state, ProcessState::Killed);
    eventually(|| !alive(group)).await;
    // The grandchild `sleep` shared the group and is gone too.
    let leftovers = std::process::Command::new("pgrep")
        .args(["-g", &group.to_string()])
        .output()
        .map(|output| output.stdout)
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "{}",
        String::from_utf8_lossy(&leftovers)
    );
    // Killing again is harmless and returns the same row.
    assert_eq!(
        registry.kill(Some("s1"), &started.id).await.unwrap().state,
        ProcessState::Killed
    );
}

#[tokio::test]
async fn a_process_of_another_session_does_not_exist() {
    let registry = ProcessRegistry::new();
    let started = registry
        .start("mine", "sleep", None, sh("sleep 30"))
        .unwrap();
    let error = registry.kill(Some("other"), &started.id).await.unwrap_err();
    assert_eq!(error, AppError::not_found("No such process"));
    let error = registry
        .output(Some("other"), &started.id, 0, NOW)
        .await
        .unwrap_err();
    assert_eq!(error, AppError::not_found("No such process"));
    assert!(
        registry
            .output(Some("mine"), "proc_nope", 0, NOW)
            .await
            .is_err()
    );
    assert!(registry.list(Some("other")).is_empty());
    registry.kill_all().await;
}

#[tokio::test]
async fn kill_session_ends_and_forgets_only_that_session() {
    let registry = ProcessRegistry::new();
    let gone = registry
        .start("gone", "sleep", None, sh("sleep 30"))
        .unwrap();
    let kept = registry
        .start("kept", "sleep", None, sh("sleep 30"))
        .unwrap();
    registry.kill_session("gone").await;
    assert!(registry.list(Some("gone")).is_empty());
    eventually(|| !alive(gone.pid.unwrap())).await;
    assert_eq!(registry.list(Some("kept"))[0].state, ProcessState::Running);
    assert!(alive(kept.pid.unwrap()));
    registry.kill_all().await;
    assert!(registry.list(None).is_empty());
    eventually(|| !alive(kept.pid.unwrap())).await;
}

#[tokio::test]
async fn finished_processes_are_reaped_after_the_retention_time() {
    // Within the retention time (an hour: no load can outlast it) nothing is reaped.
    let keeping = ProcessRegistry::with_limits(ProcessLimits {
        retain_finished: Duration::from_secs(3600),
        ..ProcessLimits::default()
    });
    keeping.start("s", "true", None, sh("true")).unwrap();
    eventually(|| keeping.list(Some("s"))[0].state == ProcessState::Exited).await;
    assert_eq!(keeping.reap(), 0, "still within the retention time");
    assert_eq!(keeping.list(None).len(), 1);

    let registry = ProcessRegistry::with_limits(ProcessLimits {
        retain_finished: Duration::from_millis(50),
        ..ProcessLimits::default()
    });
    let started = registry.start("s", "true", None, sh("true")).unwrap();
    eventually(|| registry.list(Some("s"))[0].state == ProcessState::Exited).await;
    eventually(|| registry.reap() == 1).await;
    assert!(registry.list(None).is_empty());
    assert!(
        registry
            .output(Some("s"), &started.id, 0, NOW)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_process_is_stopped_at_its_lifetime() {
    let registry = ProcessRegistry::with_limits(ProcessLimits {
        lifetime: Duration::from_millis(300),
        ..ProcessLimits::default()
    });
    let started = registry.start("s", "sleep", None, sh("sleep 30")).unwrap();
    eventually(|| registry.list(Some("s"))[0].state == ProcessState::Expired).await;
    assert!(!alive(started.pid.unwrap()));
    // The slot is free again.
    for _ in 0..MAX_PER_SESSION {
        registry.start("s", "sleep", None, sh("sleep 30")).unwrap();
    }
    registry.kill_all().await;
}

#[tokio::test]
async fn the_output_ring_keeps_the_newest_256_kib_and_reports_what_it_dropped() {
    let registry = ProcessRegistry::new();
    // 600 KiB of 'a' then a marker.
    let script = "head -c 614400 /dev/zero | tr '\\0' a; echo END";
    let started = registry.start("s", "big", None, sh(script)).unwrap();
    eventually(|| registry.list(Some("s"))[0].state == ProcessState::Exited).await;
    let out = registry
        .output(Some("s"), &started.id, 0, NOW)
        .await
        .unwrap();
    assert_eq!(out.text.len(), RING_BYTES);
    assert!(out.text.ends_with("aaaEND\n"));
    assert_eq!(out.dropped, 614_400 + 4 - RING_BYTES as u64);
    assert_eq!(out.next_offset, 614_404);
    // Reading from inside the kept window drops nothing.
    let tail = registry
        .output(Some("s"), &started.id, 614_404 - 4, NOW)
        .await
        .unwrap();
    assert_eq!((tail.text.as_str(), tail.dropped), ("END\n", 0));
}

#[tokio::test]
async fn a_character_split_across_reads_is_held_back_until_whole() {
    let registry = ProcessRegistry::new();
    // 'é' is C3 A9: the child prints the first byte, then waits for the test's gate file (no
    // timing guess) before it prints the second.
    let gate = std::env::temp_dir().join(format!("pingdeck-split-gate-{}", std::process::id()));
    let _ = std::fs::remove_file(&gate);
    let started = registry
        .start(
            "s",
            "split",
            None,
            sh(&format!(
                "printf '\\303'; while [ ! -e '{}' ]; do sleep 0.02; done; printf '\\251\\n'; sleep 30",
                gate.display()
            )),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let first = registry
        .output(Some("s"), &started.id, 0, NOW)
        .await
        .unwrap();
    assert_eq!((first.text.as_str(), first.next_offset), ("", 0));
    std::fs::write(&gate, "").unwrap();
    let second = registry
        .output(Some("s"), &started.id, 0, Duration::from_secs(60))
        .await
        .unwrap();
    let _ = std::fs::remove_file(&gate);
    assert_eq!(second.text, "é\n");
    registry.kill_all().await;
}
