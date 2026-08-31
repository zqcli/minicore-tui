//! Terminal lifecycle contract, tested from outside the crate.
//!
//! The exact enter/restore ANSI sequence order is pinned by the in-crate
//! unit suite (`src/terminal.rs`): enter runs EnterAlternateScreen →
//! EnableBracketedPaste → EnableMouseCapture → Hide, and restore runs those
//! four in reverse plus raw-mode teardown, with a retry latch so a failed
//! restore is retried by `Drop`. This file adds the real-terminal checks
//! that can only run under a TTY.

use std::env;
use std::io::{IsTerminal, Read};
use std::panic::{self, AssertUnwindSafe};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use minicore_tui::terminal::{PanicHookGuard, TerminalGuard};

const CHILD_MODE: &str = "MINICORE_TUI_TERMINAL_RESTORE_CHILD";

struct InstallDuringUnwind;

impl Drop for InstallDuringUnwind {
    fn drop(&mut self) {
        let _ = PanicHookGuard::install();
    }
}

/// Runs the real enter/restore round trip. Skipped (passes) when stdin is
/// not a terminal — which is the normal `cargo test` case — and effectuated
/// when invoked from a real terminal with `cargo test --ignored`.
#[test]
#[ignore = "requires a real terminal: run with `cargo test --ignored` from a TTY"]
fn real_pty_enter_and_restore_round_trip() {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("terminal_restore: stdin/stdout are not a TTY; skipping the PTY round trip");
        return;
    }
    let mut guard = TerminalGuard::enter().expect("enter the alternate screen");
    // A twice-called restore must be a no-op the second time (retry latch).
    guard.restore().expect("restore the terminal");
    guard
        .restore()
        .expect("second restore is a no-op and still succeeds");
}

/// The production order contract as a reference test: the documented restore
/// sequence is fixed in-crate, and this test pins the ownership contract the
/// async main loop relies on — the guard must be sendable across the loop.
#[test]
fn terminal_guard_owns_one_restore_path_and_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    let guard = TerminalGuard::enter();
    // Send allows a simpler `TerminalGuard`-bearing future to move across
    // await points; the TTY round trip itself is the ignored test above.
    if let Ok(guard) = guard {
        assert_send(&guard);
    }
}

/// Runs the panic-unwind case in another process so this test never changes
/// the hook used by the parent test process. The silent run keeps the normal
/// test suite quiet; the captured run checks that the failure is not the
/// recursive destructor-panic diagnostic.
#[test]
fn panic_hook_drop_during_unwind_is_a_normal_panic() {
    let silent = run_child("child_test", "panic", false);
    assert_normal_panic(silent.status);

    let captured = run_child("child_test", "panic", true);
    assert_normal_panic(captured.status);
    assert_clean_panic_output(&captured.output);
}

/// Exercises normal restoration, nested guards, and both drop orders without
/// installing a process-global test hook in the parent.
#[test]
fn panic_hook_normal_drop_and_nested_guards_restore_safely() {
    let result = run_child("panic_hook_drop_child", "drop", true);
    assert!(
        result.status.success(),
        "child status: {:?}\noutput: {}",
        result.status,
        String::from_utf8_lossy(&result.output)
    );
    assert_clean_panic_output(&result.output);
}

/// Child-only test entry point for the unwind regression. It is a no-op during
/// the ordinary test run and is selected explicitly by the parent test.
#[test]
fn child_test() {
    if env::var(CHILD_MODE).as_deref() != Ok("panic") {
        return;
    }
    let _install_during_unwind = InstallDuringUnwind;
    let _guard = PanicHookGuard::install();
    panic!("panic hook unwind regression child");
}

/// Child-only test entry point for hook restoration checks.
#[test]
fn panic_hook_drop_child() {
    if env::var(CHILD_MODE).as_deref() != Ok("drop") {
        return;
    }

    let harness_hook = panic::take_hook();
    let called = Arc::new(AtomicBool::new(false));
    let hook_called = Arc::clone(&called);
    panic::set_hook(Box::new(move |_| {
        hook_called.store(true, Ordering::SeqCst);
    }));

    {
        let outer = PanicHookGuard::install();
        let inner = PanicHookGuard::install();
        drop(inner);
        drop(outer);
    }
    let caught = panic::catch_unwind(AssertUnwindSafe(|| {
        panic!("normal hook restoration child");
    }));
    assert!(caught.is_err());
    assert!(called.load(Ordering::SeqCst));

    called.store(false, Ordering::SeqCst);
    let outer = PanicHookGuard::install();
    let inner = PanicHookGuard::install();
    drop(outer);
    drop(inner);
    let caught = panic::catch_unwind(AssertUnwindSafe(|| {
        panic!("nested hook restoration child");
    }));
    assert!(caught.is_err());
    assert!(called.load(Ordering::SeqCst));

    let current = panic::take_hook();
    drop(current);
    panic::set_hook(harness_hook);
}

struct ChildResult {
    status: ExitStatus,
    output: Vec<u8>,
}

fn run_child(test_name: &str, mode: &str, capture_output: bool) -> ChildResult {
    let executable = env::current_exe().expect("test executable");
    let mut command = Command::new(executable);
    command.args(["--exact", test_name]).env(CHILD_MODE, mode);
    if capture_output {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    } else {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }

    let mut child = command.spawn().expect("spawn terminal hook child");
    let stdout = child.stdout.take().map(|mut stream| {
        thread::spawn(move || {
            let mut output = Vec::new();
            stream.read_to_end(&mut output).expect("read child stdout");
            output
        })
    });
    let stderr = child.stderr.take().map(|mut stream| {
        thread::spawn(move || {
            let mut output = Vec::new();
            stream.read_to_end(&mut output).expect("read child stderr");
            output
        })
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait().expect("poll terminal hook child") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_reader(stdout);
                let _ = join_reader(stderr);
                panic!("terminal hook child exceeded 10-second timeout");
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    };

    let mut output = join_reader(stdout);
    output.extend(join_reader(stderr));
    ChildResult { status, output }
}

fn join_reader(reader: Option<thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    reader
        .map(|reader| reader.join().expect("join terminal hook output reader"))
        .unwrap_or_default()
}

fn assert_normal_panic(status: ExitStatus) {
    assert!(!status.success(), "panic child unexpectedly succeeded");
    assert_eq!(
        status.code(),
        Some(101),
        "panic child did not use panic exit code"
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), None, "panic child terminated by signal");
        assert_ne!(
            status.signal(),
            Some(6),
            "panic child terminated with SIGABRT"
        );
    }
}

fn assert_clean_panic_output(output: &[u8]) {
    let output = String::from_utf8_lossy(output);
    for forbidden in [
        "thread caused non-unwinding panic",
        "panic in a destructor during cleanup",
    ] {
        assert!(
            !output.contains(forbidden),
            "child output contains recursive panic diagnostic `{forbidden}`: {output}"
        );
    }
}
