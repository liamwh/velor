//! Deterministic, network-free fixture agent for `velor-core` execution tests.
//!
//! Behaviour is selected entirely by the first argument (a scenario name) plus
//! `--flag value` options. Nothing here contacts a real provider. Integration
//! tests locate this binary via `CARGO_BIN_EXE_velor-test-agent`.
//!
//! This is a `[[bin]]` of `velor-core` (source kept under `tests/fixtures/`) so
//! tests get a stable `CARGO_BIN_EXE_*` path without the unstable `bindeps`
//! cargo feature.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::process::ExitCode;
use std::time::Duration;

/// Parsed `--flag value` options.
#[derive(Default)]
struct Options {
    bytes: Option<usize>,
    count: Option<u32>,
    interval_ms: Option<u64>,
    duration: Option<u64>,
    children: Option<u32>,
    lockfile: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
}

impl Options {
    fn parse(args: &[String]) -> Self {
        let mut opts = Self::default();
        let mut i = 0;
        while i < args.len() {
            let key = args[i].as_str();
            if let Some(flag) = key.strip_prefix("--") {
                let value = args.get(i + 1);
                match (flag, value) {
                    ("bytes", Some(v)) => opts.bytes = v.parse().ok(),
                    ("count", Some(v)) => opts.count = v.parse().ok(),
                    ("interval-ms", Some(v)) => opts.interval_ms = v.parse().ok(),
                    ("duration", Some(v)) => opts.duration = parse_duration_secs(v),
                    ("children", Some(v)) => opts.children = v.parse().ok(),
                    ("lockfile", Some(v)) => opts.lockfile = Some(v.clone()),
                    ("stdout", Some(v)) => opts.stdout = Some(v.clone()),
                    ("stderr", Some(v)) => opts.stderr = Some(v.clone()),
                    _ => {}
                }
                i += 2;
            } else {
                // Non-flag token (e.g. the scenario name or a positional); skip
                // just this token so it cannot swallow the next flag as a value.
                i += 1;
            }
        }
        opts
    }
}

fn parse_duration_secs(v: &str) -> Option<u64> {
    if let Some(secs) = v.strip_suffix('s').and_then(|n| n.parse::<u64>().ok()) {
        return Some(secs);
    }
    if let Some(mins) = v.strip_suffix('m').and_then(|n| n.parse::<u64>().ok()) {
        return Some(mins * 60);
    }
    v.parse::<u64>().ok()
}

fn print_stdout(s: &str) {
    let _ = std::io::stdout().write_all(s.as_bytes());
    let _ = std::io::stdout().flush();
}

fn print_stderr(s: &str) {
    let _ = std::io::stderr().write_all(s.as_bytes());
    let _ = std::io::stderr().flush();
}

/// All recognised scenario names (used to locate the scenario token anywhere
/// in argv, so the fixture can be driven even when a caller prepends its own
/// flags — e.g. the Claude adapter's `--permission-mode ...` args).
const SCENARIOS: &[&str] = &[
    "overload-529",
    "econnreset",
    "invalid-key",
    "context-too-long",
    "rate-limit-429",
    "stderr-only",
    "split-output",
    "success",
    "success-quiet",
    "echo-stdin",
    "large-output",
    "long-line",
    "stdout-lines",
    "sleep",
    "close-stdout-early",
    "exit-before-stdin",
    "invalid-utf8",
    "fork-tree",
    "fork-grandchild",
    "ignore-sigterm",
    "exit-code",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    // Locate the scenario token anywhere in argv (after the program path).
    let scenario = args
        .iter()
        .skip(1)
        .find(|a| SCENARIOS.iter().any(|s| *s == a.as_str()))
        .cloned();
    let Some(scenario) = scenario else {
        eprintln!("velor-test-agent: no recognised scenario in args");
        return ExitCode::from(2);
    };
    // Options are scanned across all args; unknown flags are ignored.
    let opts = Options::parse(&args[1..]);
    match scenario.as_str() {
        "overload-529" => {
            print_stdout("API Error: 529 [1305][The service may be temporarily overloaded. Please retry later.]\n");
            ExitCode::from(1)
        }
        "econnreset" => {
            print_stdout("API Error: Unable to connect to API (ECONNRESET)\n");
            ExitCode::from(1)
        }
        "invalid-key" => {
            print_stdout("API Error: 401 invalid x-api-key. Please check your authentication.\n");
            ExitCode::from(1)
        }
        "context-too-long" => {
            print_stdout("Error: prompt is too long: context_length_exceeded (tokens > limit)\n");
            ExitCode::from(1)
        }
        "rate-limit-429" => {
            print_stdout("API Error: 429 Too Many Requests\nRetry-After: 5\n");
            ExitCode::from(1)
        }
        "stderr-only" => {
            print_stderr("this error went only to stderr\n");
            ExitCode::from(1)
        }
        "split-output" => {
            print_stdout("API Error: 529 [1305] overloaded\n");
            print_stderr("additional detail on stderr\n");
            ExitCode::from(1)
        }
        "success" => {
            print_stdout("{\"type\":\"system\",\"session_id\":\"sess-123\"}\n");
            print_stdout("{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}\n");
            ExitCode::SUCCESS
        }
        "success-quiet" => ExitCode::SUCCESS,
        "echo-stdin" => echo_stdin(),
        "large-output" => large_output(&opts),
        "long-line" => long_line(&opts),
        "stdout-lines" => stdout_lines(&opts),
        "sleep" => sleep_scenario(&opts),
        "close-stdout-early" => close_stdout_early(&opts),
        "exit-before-stdin" => ExitCode::from(1),
        "invalid-utf8" => invalid_utf8(),
        "fork-tree" => fork_tree(&opts),
        "fork-grandchild" => fork_grandchild(&opts),
        "ignore-sigterm" => ignore_sigterm(&opts),
        "exit-code" => exit_code_scenario(&args, &opts),
        other => {
            eprintln!("velor-test-agent: unknown scenario '{other}'");
            ExitCode::from(2)
        }
    }
}

fn echo_stdin() -> ExitCode {
    let mut buf = Vec::new();
    if std::io::stdin().read_to_end(&mut buf).is_err() {
        return ExitCode::from(1);
    }
    let head: Vec<u8> = buf.into_iter().take(16).collect();
    let mut out = b"ECHO:".to_vec();
    out.extend_from_slice(&head);
    out.push(b'\n');
    let _ = std::io::stdout().write_all(&out);
    let _ = std::io::stdout().flush();
    ExitCode::SUCCESS
}

fn large_output(opts: &Options) -> ExitCode {
    let n = opts.bytes.unwrap_or(1024);
    let chunk = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let mut written = 0usize;
    while written < n {
        let take = std::cmp::min(chunk.len(), n - written);
        if handle.write_all(&chunk[..take]).is_err() {
            return ExitCode::from(1);
        }
        written += take;
    }
    let _ = handle.flush();
    ExitCode::SUCCESS
}

fn long_line(opts: &Options) -> ExitCode {
    let n = opts.bytes.unwrap_or(1024);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    for _ in 0..n {
        if handle.write_all(b"x").is_err() {
            return ExitCode::from(1);
        }
    }
    // Deliberately no trailing newline.
    let _ = handle.flush();
    ExitCode::SUCCESS
}

fn stdout_lines(opts: &Options) -> ExitCode {
    let count = opts.count.unwrap_or(3);
    let interval = Duration::from_millis(opts.interval_ms.unwrap_or(0));
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    for i in 0..count {
        let line = format!("line {i}\n");
        if handle.write_all(line.as_bytes()).is_err() {
            return ExitCode::from(1);
        }
        let _ = handle.flush();
        // Sleep *between* lines (not before the first) so idle tests are deterministic.
        if !interval.is_zero() && i + 1 < count {
            std::thread::sleep(interval);
        }
    }
    ExitCode::SUCCESS
}

fn sleep_scenario(opts: &Options) -> ExitCode {
    let secs = opts.duration.unwrap_or(30);
    std::thread::sleep(Duration::from_secs(secs));
    ExitCode::SUCCESS
}

fn close_stdout_early(opts: &Options) -> ExitCode {
    drop(std::io::stdout());
    let secs = opts.duration.unwrap_or(5);
    std::thread::sleep(Duration::from_secs(secs));
    ExitCode::SUCCESS
}

fn invalid_utf8() -> ExitCode {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = handle.write_all(&[0xFF, 0xFE, 0xFD, b'\n']);
    let _ = handle.flush();
    ExitCode::SUCCESS
}

fn fork_tree(opts: &Options) -> ExitCode {
    let children = opts.children.unwrap_or(2);
    let duration = opts.duration.unwrap_or(30);
    let Some(lockfile) = opts.lockfile.clone() else {
        eprintln!("fork-tree: --lockfile is required");
        return ExitCode::from(2);
    };
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fork-tree: cannot resolve current exe: {e}");
            return ExitCode::from(1);
        }
    };
    let mut spawned = 0u32;
    for _ in 0..children {
        let status = std::process::Command::new(&exe)
            .arg("fork-grandchild")
            .arg("--lockfile")
            .arg(&lockfile)
            .arg("--duration")
            .arg(duration.to_string())
            .spawn();
        if status.is_ok() {
            spawned += 1;
        }
    }
    print_stdout(&format!("forked {spawned} grandchild(ren)\n"));
    // Stay alive so the process group persists until the supervisor kills it.
    std::thread::sleep(Duration::from_secs(duration));
    ExitCode::SUCCESS
}

/// Holds an exclusive `flock` on the lockfile and sleeps. The lock is tied to the
/// open file descriptor: it is released only when this process dies (and the fd
/// closes), which is the robust signal that the grandchild was actually killed
/// (no PID-reuse race).
fn fork_grandchild(opts: &Options) -> ExitCode {
    let Some(lockfile) = opts.lockfile.clone() else {
        eprintln!("fork-grandchild: --lockfile is required");
        return ExitCode::from(2);
    };
    let duration = opts.duration.unwrap_or(30);
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lockfile)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("fork-grandchild: cannot open lockfile: {e}");
            return ExitCode::from(1);
        }
    };
    let fd = file.as_raw_fd();
    // Blocking exclusive flock; held until the process exits or is killed.
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
    if rc != 0 {
        eprintln!("fork-grandchild: flock failed");
        return ExitCode::from(1);
    }
    std::thread::sleep(Duration::from_secs(duration));
    // Lock auto-released when `file` (and its fd) drops at return.
    ExitCode::SUCCESS
}

fn ignore_sigterm(opts: &Options) -> ExitCode {
    let duration = opts.duration.unwrap_or(600);
    // Install a SIGTERM handler that is explicitly ignored so the supervisor's
    // graceful termination must escalate to SIGKILL.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
    std::thread::sleep(Duration::from_secs(duration));
    ExitCode::SUCCESS
}

fn exit_code_scenario(args: &[String], opts: &Options) -> ExitCode {
    let code: u8 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
    if let Some(s) = &opts.stdout {
        print_stdout(s);
    }
    if let Some(s) = &opts.stderr {
        print_stderr(s);
    }
    ExitCode::from(code)
}
