//! Own the local Harness process and its one-time browser sign-in URL.
//!
//! Child output is deliberately not persisted: Harness prints a credential in its
//! readiness URL. The desktop log contains only lifecycle events we create.

use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use url::Url;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(7);
const OUTPUT_LINE_LIMIT: u64 = 16 * 1024;

// Do not derive Debug: the URL contains an ephemeral authentication credential.
#[derive(Clone, Serialize)]
pub struct BackendReady {
    pub url: String,
    pub startup_ms: u64,
    pub pid: u32,
}

pub struct Backend {
    runtime_dir: PathBuf,
    data_dir: PathBuf,
    child: Option<Child>,
    parent_guard: Option<ParentGuard>,
    ready: Option<BackendReady>,
    shutdown: Arc<AtomicBool>,
    startup_timeout: Duration,
    shutdown_timeout: Duration,
}

impl Backend {
    pub fn new(runtime_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            runtime_dir,
            data_dir,
            child: None,
            parent_guard: None,
            ready: None,
            shutdown: Arc::new(AtomicBool::new(false)),
            startup_timeout: STARTUP_TIMEOUT,
            shutdown_timeout: SHUTDOWN_TIMEOUT,
        }
    }

    /// Retain this signal outside the backend mutex. Setting it interrupts a
    /// pending start promptly so application exit never waits for its timeout.
    /// The signal is sticky: a shutting-down app cannot launch another process.
    pub fn shutdown_signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    pub fn start(&mut self) -> Result<BackendReady, String> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err("The application is closing.".into());
        }
        if self.is_running() {
            if let Some(ready) = &self.ready {
                return Ok(ready.clone());
            }
        }
        self.stop();

        let node = self.runtime_dir.join("node");
        let entry = self
            .runtime_dir
            .join("node_modules/@deepseek-ai/dsh/lib/bin.js");
        if !node.is_file() || !entry.is_file() {
            return Err("The bundled Harness runtime is missing. Prepare the runtime and rebuild the application.".into());
        }

        let home = self.data_dir.join("harness");
        for directory in [&self.data_dir, &home] {
            fs::create_dir_all(directory)
                .and_then(|()| fs::set_permissions(directory, fs::Permissions::from_mode(0o700)))
                .map_err(|_| {
                    "The application cannot create its private data folder.".to_string()
                })?;
        }
        let started = Instant::now();
        let mut command = Command::new(node);
        command
            .arg(entry)
            .args(["web", "--no-open", "--host", "127.0.0.1", "--port", "0"])
            .env("DSH_HOME", &home)
            .current_dir(&self.data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        configure_parent_death(&mut command);
        let (mut child, parent_guard) = spawn_guarded(command)
            .map_err(|error| format!("The bundled Harness could not start: {error}"))?;
        self.parent_guard = Some(parent_guard);
        let pid = child.id();
        self.log(&format!("Harness process started (pid {pid})."));

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                // Bound memory even if a plugin writes an unusually long line.
                match reader
                    .by_ref()
                    .take(OUTPUT_LINE_LIMIT)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if let Ok(line) = std::str::from_utf8(&line) {
                            if let Some(url) = launch_url(line) {
                                let _ = sender.send(url);
                            }
                        }
                    }
                }
            }
        });
        // Drain stderr to prevent a full pipe from blocking startup. Do not log
        // upstream text, which can include credentials from third-party plugins.
        thread::spawn(move || {
            let _ = std::io::copy(&mut BufReader::new(stderr), &mut std::io::sink());
        });
        self.child = Some(child);

        let result = loop {
            if self.shutdown.load(Ordering::SeqCst) {
                break Err("The application is closing.".into());
            }
            match self.child.as_mut().expect("owned child").try_wait() {
                Ok(Some(status)) => {
                    break Err(format!(
                        "Harness stopped before it was ready ({status}). Try starting it again."
                    ));
                }
                Err(_) => break Err("The application could not check the Harness process.".into()),
                Ok(None) => {}
            }
            if started.elapsed() >= self.startup_timeout {
                break Err(
                    "Harness took too long to start. Its process has been stopped; try again."
                        .into(),
                );
            }
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(url) => {
                    let ready = BackendReady {
                        url,
                        startup_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                        pid,
                    };
                    self.log(&format!("Harness ready after {} ms.", ready.startup_ms));
                    self.ready = Some(ready.clone());
                    break Ok(ready);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    break Err(
                        "Harness closed its startup connection before it was ready. Try again."
                            .into(),
                    );
                }
            }
        };

        if result.is_err() {
            self.log("Harness startup failed; cleaning up its process group.");
            self.stop();
        }
        result
    }

    pub fn is_running(&mut self) -> bool {
        let running = self
            .child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
        if !running && self.child.is_some() {
            self.stop();
        }
        running
    }

    pub fn stop(&mut self) {
        self.ready = None;
        let Some(mut child) = self.child.take() else {
            self.parent_guard.take();
            return;
        };
        let group = child.id() as i32;
        signal_group(group, libc::SIGTERM);
        let deadline = Instant::now() + self.shutdown_timeout;
        loop {
            let finished = matches!(child.try_wait(), Ok(Some(_)));
            if finished && !group_exists(group) {
                break;
            }
            if Instant::now() >= deadline {
                signal_group(group, libc::SIGKILL);
                let _ = child.wait();
                self.log("Harness shutdown exceeded its grace period; stopped its process group.");
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        // Reap the direct child even when it exited before the first signal.
        let _ = child.wait();
        self.parent_guard.take();
        self.log("Harness process stopped.");
    }

    fn log(&self, message: &str) {
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.data_dir.join("desktop.log"))
        {
            let _ = writeln!(file, "{message}");
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Linux's parent-death signal is tied to the thread which spawned a child,
/// rather than only to its process. Keep that thread alive for the whole launch
/// so a retiring async-runtime worker cannot terminate a healthy workspace.
struct ParentGuard {
    release: Option<mpsc::Sender<()>>,
    launcher: Option<thread::JoinHandle<()>>,
}

impl Drop for ParentGuard {
    fn drop(&mut self) {
        self.release.take();
        if let Some(launcher) = self.launcher.take() {
            let _ = launcher.join();
        }
    }
}

fn spawn_guarded(mut command: Command) -> std::io::Result<(Child, ParentGuard)> {
    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    let (release, wait_for_release) = mpsc::channel();
    let launcher = thread::spawn(move || {
        let result = command.spawn();
        let started = result.is_ok();
        let _ = result_sender.send(result);
        if started {
            let _ = wait_for_release.recv();
        }
    });
    let guard = ParentGuard {
        release: Some(release),
        launcher: Some(launcher),
    };
    let child = result_receiver.recv().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "The process launcher stopped.",
        )
    })??;
    Ok((child, guard))
}

#[cfg(target_os = "linux")]
fn configure_parent_death(command: &mut Command) {
    let desktop_pid = std::process::id() as libc::pid_t;
    // Only async-signal-safe operations may run between fork and exec. prctl and
    // getppid are direct syscalls; errors here prevent the child from executing.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGTERM,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            ) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            // Close the race in which the desktop died before prctl was set.
            if libc::getppid() != desktop_pid {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn configure_parent_death(_command: &mut Command) {}

fn signal_group(group: i32, signal: i32) {
    // The child created its own process group, so negative PID addresses only
    // this launch and its descendants, never the desktop's own process group.
    unsafe { libc::kill(-group, signal) };
}

fn group_exists(group: i32) -> bool {
    unsafe { libc::kill(-group, 0) == 0 }
}

fn launch_url(line: &str) -> Option<String> {
    let (_, announcement) = line.split_once("dsh web: ")?;
    let candidate = announcement.split_whitespace().next()?;
    let url = Url::parse(candidate).ok()?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none_or(|port| port == 0)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.fragment().is_some()
    {
        return None;
    }
    Some(candidate.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        path: PathBuf,
        backend: Backend,
    }

    impl Fixture {
        fn new(script: &str) -> Self {
            let suffix = NEXT_TEST.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("dsh-shell-test-{}-{suffix}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            let runtime = path.join("runtime");
            fs::create_dir_all(runtime.join("node_modules/@deepseek-ai/dsh/lib")).unwrap();
            fs::write(runtime.join("node_modules/@deepseek-ai/dsh/lib/bin.js"), "").unwrap();
            let node = runtime.join("node");
            fs::write(&node, format!("#!/bin/sh\n{script}\n")).unwrap();
            fs::set_permissions(&node, fs::Permissions::from_mode(0o700)).unwrap();
            let mut backend = Backend::new(runtime, path.join("data"));
            backend.startup_timeout = Duration::from_millis(200);
            backend.shutdown_timeout = Duration::from_millis(150);
            Self { path, backend }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.backend.stop();
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn preserves_the_tokenized_owned_loopback_url() {
        let url = "http://127.0.0.1:3081/?token=example%2Bsecret";
        assert_eq!(
            launch_url(&format!("dsh web: {url}\n")).as_deref(),
            Some(url)
        );
        for rejected in [
            "https://127.0.0.1:3081/",
            "http://localhost:3081/",
            "http://127.0.0.1/",
            "http://127.0.0.1:0/",
            "http://example.com:3081/",
            "http://user@127.0.0.1:3081/",
            "http://127.0.0.1:3081/other",
            "http://127.0.0.1:3081/#fragment",
        ] {
            assert!(launch_url(&format!("dsh web: {rejected}")).is_none());
        }
    }

    #[test]
    fn starts_idempotently_and_never_logs_the_credential() {
        let mut fixture =
            Fixture::new("echo 'dsh web: http://127.0.0.1:3081/?token=do-not-log'\nsleep 60");
        let first = fixture.backend.start().unwrap();
        let second = fixture.backend.start().unwrap();
        assert_eq!(first.pid, second.pid);
        assert!(fixture.backend.is_running());
        fixture.backend.stop();
        assert!(!fixture.backend.is_running());
        let log = fs::read_to_string(fixture.path.join("data/desktop.log")).unwrap();
        assert!(!log.contains("do-not-log"));
        assert!(!log.contains("token="));
    }

    #[test]
    fn cleans_up_failed_startup_and_allows_a_retry() {
        let mut fixture = Fixture::new("exit 7");
        assert!(fixture.backend.start().is_err());
        assert!(!fixture.backend.is_running());
        fs::write(
            fixture.backend.runtime_dir.join("node"),
            "#!/bin/sh\necho 'dsh web: http://127.0.0.1:3081/?token=retry'\nsleep 60\n",
        )
        .unwrap();
        assert!(fixture.backend.start().is_ok());
    }

    #[test]
    fn times_out_and_reaps_the_child() {
        let mut fixture = Fixture::new("sleep 60");
        let started = Instant::now();
        assert!(fixture.backend.start().is_err());
        assert!(!fixture.backend.is_running());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn shutdown_interrupts_startup_without_waiting_for_the_timeout() {
        let mut fixture = Fixture::new("exec sleep 60");
        fixture.backend.startup_timeout = Duration::from_secs(60);
        let shutdown = fixture.backend.shutdown_signal();
        let signal = thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            shutdown.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        assert!(fixture.backend.start().is_err());
        signal.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!fixture.backend.is_running());
        // Exiting remains terminal even if the startup page tries to retry.
        assert!(fixture.backend.start().is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn workspace_survives_the_start_callers_thread() {
        let fixture =
            Fixture::new("echo 'dsh web: http://127.0.0.1:3081/?token=thread-test'\nexec sleep 60");
        let mut fixture = thread::spawn(move || {
            let mut fixture = fixture;
            assert!(fixture.backend.start().is_ok());
            fixture
        })
        .join()
        .unwrap();
        thread::sleep(Duration::from_millis(80));
        assert!(fixture.backend.is_running());
    }

    // Re-executed by the next test to prove abrupt desktop death without killing
    // this test harness or running a pre-exec closure after a hand-written fork.
    #[cfg(target_os = "linux")]
    #[test]
    fn parent_death_helper() {
        let Some(marker) = std::env::var_os("HARNESS_BACKEND_PARENT_DEATH_TEST") else {
            return;
        };
        let mut fixture =
            Fixture::new("echo 'dsh web: http://127.0.0.1:3081/?token=parent-test'\nexec sleep 60");
        let ready = fixture.backend.start().unwrap();
        fs::write(marker, ready.pid.to_string()).unwrap();
        loop {
            thread::park();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn abrupt_desktop_death_terminates_its_workspace() {
        let fixture = Fixture::new("exit 0");
        let marker = fixture.path.join("child.pid");
        let module = module_path!().split_once("::").unwrap().1;
        let helper_name = format!("{module}::parent_death_helper");
        let mut helper = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &helper_name, "--nocapture"])
            .env("HARNESS_BACKEND_PARENT_DEATH_TEST", &marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let helper_path = std::env::temp_dir().join(format!("dsh-shell-test-{}-0", helper.id()));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !marker.is_file() && Instant::now() < deadline {
            if helper.try_wait().unwrap().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let marker_contents = fs::read_to_string(&marker);
        let _ = helper.kill();
        let _ = helper.wait();
        let pid: u32 = marker_contents
            .expect("helper did not start its workspace")
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let running = || {
            fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    stat.rsplit_once(") ")
                        .map(|(_, fields)| !fields.starts_with('Z'))
                })
                .unwrap_or(false)
        };
        while running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let _ = fs::remove_dir_all(helper_path);
        assert!(!running(), "workspace survived abrupt desktop death");
    }

    #[test]
    fn missing_runtime_returns_actionable_error() {
        let mut backend = Backend::new(PathBuf::from("/missing-dsh-runtime"), PathBuf::new());
        match backend.start() {
            Err(message) => assert!(message.contains("runtime is missing")),
            Ok(_) => panic!("missing runtime unexpectedly started"),
        }
    }
}
