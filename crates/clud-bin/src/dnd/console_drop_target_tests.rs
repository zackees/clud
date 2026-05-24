use super::*;
use crate::dnd::dropfiles::DROPFILES_HEADER_SIZE;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

fn make_dropfiles_wide(paths: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(DROPFILES_HEADER_SIZE as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    out.extend_from_slice(&0u32.to_le_bytes()); // fNC
    out.extend_from_slice(&1u32.to_le_bytes()); // fWide = TRUE
    for path in paths {
        for unit in path.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

// ─── dispatch_dropfiles_to_injector — existing tests ───────────────

#[test]
fn dispatch_forwards_parsed_paths_to_injector() {
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);
    let injector: DropInjector = Box::new(move |paths: &[String]| {
        captured_clone.lock().unwrap().extend_from_slice(paths);
    });
    let bytes = make_dropfiles_wide(&[r"C:\test\a.txt", r"C:\test\b.txt"]);

    dispatch_dropfiles_to_injector(&bytes, &injector);

    let got = captured.lock().unwrap().clone();
    assert_eq!(got, vec![r"C:\test\a.txt", r"C:\test\b.txt"]);
}

#[test]
fn dispatch_with_empty_buffer_does_not_invoke_injector() {
    let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let calls_clone = Arc::clone(&calls);
    let injector: DropInjector = Box::new(move |_paths: &[String]| {
        *calls_clone.lock().unwrap() += 1;
    });

    dispatch_dropfiles_to_injector(&[], &injector);

    assert_eq!(*calls.lock().unwrap(), 0);
}

#[test]
fn dispatch_with_malformed_buffer_does_not_invoke_injector() {
    // Truncated header — parse_dropfiles_buffer returns empty,
    // so the injector must not fire (avoids a zero-path "drop").
    let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let calls_clone = Arc::clone(&calls);
    let injector: DropInjector = Box::new(move |_paths: &[String]| {
        *calls_clone.lock().unwrap() += 1;
    });

    let truncated = vec![0u8; DROPFILES_HEADER_SIZE - 1];
    dispatch_dropfiles_to_injector(&truncated, &injector);

    assert_eq!(*calls.lock().unwrap(), 0);
}

#[test]
fn dispatch_normalizes_paths_before_injection() {
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);
    let injector: DropInjector = Box::new(move |paths: &[String]| {
        captured_clone.lock().unwrap().extend_from_slice(paths);
    });

    let bytes = make_dropfiles_wide(&[r"C:\Users\me\Документы\file.txt"]);
    dispatch_dropfiles_to_injector(&bytes, &injector);

    let got = captured.lock().unwrap().clone();
    assert_eq!(got, vec![r"C:\Users\me\Документы\file.txt"]);
}

#[test]
fn unsupported_platform_or_not_implemented_for_windows() {
    // Smoke test of the public API. On non-Windows hosts we get
    // UnsupportedPlatform; on Windows the unit-test process may
    // or may not have a console (CI runners typically don't),
    // so we just assert that the call returns *some* result
    // without panicking.
    let injector: DropInjector = Box::new(|_| {});
    let _ = register_console_drop_target(injector, RefreshConfig::immediate_no_refresh());
}

// ─── RefreshConfig shape ───────────────────────────────────────────

#[test]
fn refresh_config_default_uses_2s_initial_3s_refresh() {
    let cfg = RefreshConfig::default_displacement();
    assert_eq!(cfg.initial_delay, Duration::from_secs(2));
    assert_eq!(cfg.refresh_interval, Duration::from_secs(3));
}

#[test]
fn refresh_config_immediate_no_refresh_zero() {
    let cfg = RefreshConfig::immediate_no_refresh();
    assert_eq!(cfg.initial_delay, Duration::ZERO);
    assert_eq!(cfg.refresh_interval, Duration::ZERO);
}

#[test]
fn refresh_config_default_trait_matches_displacement() {
    let a = RefreshConfig::default();
    let b = RefreshConfig::default_displacement();
    assert_eq!(a.initial_delay, b.initial_delay);
    assert_eq!(a.refresh_interval, b.refresh_interval);
}

// ─── Worker-loop behavior via MockRegistrar ────────────────────────

struct MockRegistrar {
    register_calls: Arc<AtomicUsize>,
    fail_initial: bool,
}

impl DragDropRegistrar for MockRegistrar {
    fn register(&self) -> Result<(), i32> {
        let n = self.register_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_initial && n == 0 {
            return Err(0x8000_4005u32 as i32); // E_FAIL
        }
        Ok(())
    }
    fn revoke(&self) -> Result<(), i32> {
        Ok(())
    }
}

#[test]
fn registration_loop_calls_register_once_when_refresh_disabled() {
    let registrar = MockRegistrar {
        register_calls: Arc::new(AtomicUsize::new(0)),
        fail_initial: false,
    };
    let calls = Arc::clone(&registrar.register_calls);
    let shutdown = RefreshShutdown::new();
    run_registration_loop(&registrar, RefreshConfig::immediate_no_refresh(), &shutdown)
        .expect("ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn registration_loop_surfaces_initial_failure() {
    let registrar = MockRegistrar {
        register_calls: Arc::new(AtomicUsize::new(0)),
        fail_initial: true,
    };
    let shutdown = RefreshShutdown::new();
    let err = run_registration_loop(&registrar, RefreshConfig::immediate_no_refresh(), &shutdown)
        .expect_err("must surface initial register failure");
    assert_eq!(err, 0x8000_4005u32 as i32);
}

#[test]
fn registration_loop_refreshes_periodically() {
    let registrar = Arc::new(MockRegistrar {
        register_calls: Arc::new(AtomicUsize::new(0)),
        fail_initial: false,
    });
    let calls = Arc::clone(&registrar.register_calls);
    let shutdown = Arc::new(RefreshShutdown::new());
    let cfg = RefreshConfig {
        initial_delay: Duration::ZERO,
        refresh_interval: Duration::from_millis(50),
    };

    let worker_shutdown = Arc::clone(&shutdown);
    let worker_registrar = Arc::clone(&registrar);
    let handle = std::thread::spawn(move || {
        run_registration_loop(&*worker_registrar, cfg, &worker_shutdown)
    });

    // Poll for the loop to tick 1 initial + ≥2 refreshes. A fixed sleep
    // was flaky on slow runners (e.g. macOS ARM under load only managed
    // 2 calls in 220 ms, see run 25886391903). Polling with a generous
    // deadline keeps the test deterministic on any platform.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while calls.load(Ordering::SeqCst) < 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    shutdown.signal();
    let result = handle.join().expect("worker panicked");
    result.expect("loop ok");

    let n = calls.load(Ordering::SeqCst);
    assert!(
        n >= 3,
        "expected at least 3 register calls, got {n} (initial + ≥2 refreshes) within 5s budget",
    );
}

#[test]
fn registration_loop_exits_promptly_on_shutdown_during_initial_delay() {
    let registrar = Arc::new(MockRegistrar {
        register_calls: Arc::new(AtomicUsize::new(0)),
        fail_initial: false,
    });
    let calls = Arc::clone(&registrar.register_calls);
    let shutdown = Arc::new(RefreshShutdown::new());
    let cfg = RefreshConfig {
        initial_delay: Duration::from_secs(60), // way longer than the test waits
        refresh_interval: Duration::from_secs(60),
    };

    let worker_shutdown = Arc::clone(&shutdown);
    let worker_registrar = Arc::clone(&registrar);
    let handle = std::thread::spawn(move || {
        run_registration_loop(&*worker_registrar, cfg, &worker_shutdown)
    });

    // Signal shutdown during the initial-delay phase.
    std::thread::sleep(Duration::from_millis(50));
    shutdown.signal();

    let start = std::time::Instant::now();
    let _ = handle.join().expect("worker panicked");
    let elapsed = start.elapsed();

    // Worker should exit within 500ms (responsive_sleep wakes
    // every 100ms).
    assert!(
        elapsed < Duration::from_millis(500),
        "worker took {:?} to exit after shutdown",
        elapsed
    );
    // Register should NOT have been called — we shut down
    // before the initial delay elapsed.
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn responsive_sleep_returns_false_when_shutdown_already_signaled() {
    let shutdown = RefreshShutdown::new();
    shutdown.signal();
    // Already-signaled shutdown — should return false even
    // with zero duration or a very long one.
    assert!(!responsive_sleep(Duration::ZERO, &shutdown));
    assert!(!responsive_sleep(Duration::from_secs(60), &shutdown));
}

#[test]
fn responsive_sleep_returns_true_for_zero_duration_no_shutdown() {
    let shutdown = RefreshShutdown::new();
    assert!(responsive_sleep(Duration::ZERO, &shutdown));
}
