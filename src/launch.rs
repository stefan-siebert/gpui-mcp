//! `launch_app` and `stop_app`: start the app from the server, and answer
//! once it can be driven.
//!
//! Without them an agent starts the app from a shell and then polls for the
//! socket, with nothing to show for a build that takes minutes and nothing to
//! say when it fails. Here the build's output goes to a log file, the answer
//! comes when a *new* instance of the app answers on its socket, and a build
//! that ends without one comes back as an error with the end of that log —
//! the compiler's message, not a timeout.
//!
//! The commands come from the configuration, never from the call:
//! `GPUI_MCP_LAUNCH` is either one command line or a JSON object of named
//! ones (`{"run": "cargo xtask run", "hot": "cargo xtask hot"}`), run by the
//! shell in `GPUI_MCP_LAUNCH_DIR` (default: this server's working
//! directory). An agent picks a name; it cannot make the server run
//! anything else.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const LAUNCH_TOOL: &str = "launch_app";
pub const STOP_TOOL: &str = "stop_app";

/// How long `launch_app` waits by default: a first build of a large app.
const DEFAULT_WAIT_MS: u64 = 900_000;
const MAX_WAIT_MS: u64 = 1_800_000;
/// Lines of the log an answer carries.
const TAIL_LINES: usize = 60;
/// How often a waiting launch looks for the app, and how often it reports.
const POLL: Duration = Duration::from_millis(500);
const PROGRESS_EVERY: Duration = Duration::from_secs(5);

/// The app instances the server can see, as `(pid, socket path)`.
pub type Instances<'a> = dyn Fn() -> Vec<(u32, String)> + 'a;
/// Whether the instance behind a socket answers a request.
pub type Answers<'a> = dyn Fn(&str) -> bool + 'a;
/// A progress line for the client, while a launch waits.
pub type Progress<'a> = dyn FnMut(&str) + 'a;

/// The launcher this server started, if it is still around.
#[derive(Default)]
pub struct Launcher {
    current: Option<Launch>,
}

struct Launch {
    profile: String,
    child: Child,
    log: PathBuf,
    /// Instances that were already there, so the new one can be told apart.
    before: Vec<u32>,
    started: Instant,
    /// Where in the log a build failure was last reported: a resumed wait
    /// looks only at what came after it.
    reported_failure_at: u64,
}

impl Launcher {
    /// `launch_app`: start the named command (or resume waiting for one this
    /// server started), and answer when a new instance answers.
    pub fn launch(
        &mut self,
        arguments: &Value,
        instances: &Instances<'_>,
        answers: &Answers<'_>,
        progress: &mut Progress<'_>,
    ) -> Result<Value> {
        let profiles = profiles()?;
        let restart = arguments["restart"].as_bool().unwrap_or(false);
        let wait = Duration::from_millis(
            arguments["timeout_ms"]
                .as_u64()
                .unwrap_or(DEFAULT_WAIT_MS)
                .clamp(1, MAX_WAIT_MS),
        );

        let still_running = self
            .current
            .as_mut()
            .is_some_and(|launch| matches!(launch.child.try_wait(), Ok(None)));
        let resume = still_running && !restart;
        if !resume {
            let profile = choose_profile(&profiles, arguments["profile"].as_str())?;
            self.stop(instances)?;
            let command = profiles
                .get(&profile)
                .context("the chosen profile is configured")?;
            let log = log_path();
            let child = spawn(command, &log)?;
            // A server that goes away (a reconnect, a crash) forgets its
            // launcher; the next one finds it here and ends it.
            std::fs::write(pid_path(), child.id().to_string()).ok();
            self.current = Some(Launch {
                profile,
                child,
                log,
                before: instances().into_iter().map(|(pid, _)| pid).collect(),
                started: Instant::now(),
                reported_failure_at: 0,
            });
        }
        let Some(launch) = self.current.as_mut() else {
            bail!("no launch to wait for");
        };

        let deadline = Instant::now() + wait;
        let mut reported = Instant::now();
        loop {
            if let Some((pid, socket)) = instances()
                .into_iter()
                .find(|(pid, _)| !launch.before.contains(pid))
                .filter(|(_, socket)| answers(socket))
            {
                return Ok(json!({
                    "ready": true,
                    "profile": launch.profile,
                    "pid": pid,
                    "socket": socket,
                    "waited_ms": launch.started.elapsed().as_millis() as u64,
                    "resumed": resume,
                    "log": launch.log.display().to_string(),
                    "log_tail": tail(&launch.log, 10),
                }));
            }
            // A dev server that fails a build does not exit: it waits for the
            // next change. Its own words for that end the wait instead.
            let (fresh, length) = read_from(&launch.log, launch.reported_failure_at);
            if let Some(failure) = build_failure(&last_lines(&fresh, 40)) {
                launch.reported_failure_at = length;
                let log_tail = tail(&launch.log, TAIL_LINES);
                let log = launch.log.display().to_string();
                bail!(
                    "`{}` failed to build ({failure}), and is waiting for a change: fix it \
                     and call launch_app again, which goes on waiting for the same launch. \
                     Its log is {log}; the end of it:\n{log_tail}",
                    launch.profile
                );
            }
            match launch.child.try_wait() {
                Ok(Some(status)) => {
                    let log_tail = tail(&launch.log, TAIL_LINES);
                    let log = launch.log.display().to_string();
                    let profile = launch.profile.clone();
                    self.current = None;
                    bail!(
                        "`{profile}` ended ({status}) before the app answered. Its log is \
                         {log}; the end of it:\n{log_tail}"
                    );
                }
                Ok(None) => {}
                Err(error) => bail!("waiting for `{}`: {error}", launch.profile),
            }
            if Instant::now() >= deadline {
                return Ok(json!({
                    "ready": false,
                    "profile": launch.profile,
                    "waited_ms": launch.started.elapsed().as_millis() as u64,
                    "hint": "Still starting — nothing failed. Call launch_app again to go on \
                             waiting (it does not start a second one unless restart is true).",
                    "log": launch.log.display().to_string(),
                    "log_tail": tail(&launch.log, 20),
                }));
            }
            if reported.elapsed() >= PROGRESS_EVERY {
                reported = Instant::now();
                let last = tail(&launch.log, 1);
                progress(&format!(
                    "{}: {}s — {}",
                    launch.profile,
                    launch.started.elapsed().as_secs(),
                    last.trim()
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    /// `stop_app`: end the launcher this server started and every instance
    /// of the app it can see.
    pub fn stop(&mut self, instances: &Instances<'_>) -> Result<Value> {
        let mut stopped = Vec::new();
        for (pid, _) in instances() {
            terminate(pid, false);
            stopped.push(pid);
        }
        // The launcher leads a process group of its own, so the build tool
        // and whatever it started go with it.
        let launcher = match self.current.take() {
            Some(mut launch) => {
                terminate(launch.child.id(), true);
                launch.child.wait().ok();
                Some(launch.profile)
            }
            None => orphaned_launcher().map(|pid| {
                terminate(pid, true);
                format!("left behind by an earlier server (process group {pid})")
            }),
        };
        std::fs::remove_file(pid_path()).ok();
        // An app that ignores the polite request is not left running.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !stopped.is_empty() && Instant::now() < deadline {
            let alive: Vec<u32> = instances().into_iter().map(|(pid, _)| pid).collect();
            if !stopped.iter().any(|pid| alive.contains(pid)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        for (pid, _) in instances() {
            if stopped.contains(&pid) {
                kill(pid);
            }
        }
        Ok(json!({ "stopped_instances": stopped, "stopped_launcher": launcher }))
    }
}

/// The configured commands by name.
fn profiles() -> Result<BTreeMap<String, String>> {
    let raw = std::env::var("GPUI_MCP_LAUNCH").unwrap_or_default();
    parse_profiles(&raw)
}

fn parse_profiles(raw: &str) -> Result<BTreeMap<String, String>> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!(
            "no launch command configured: set GPUI_MCP_LAUNCH in the server's environment \
             (.mcp.json), to a command line or to a JSON object of named ones, e.g. \
             {{\"run\": \"cargo xtask run\"}}"
        );
    }
    if !raw.starts_with('{') {
        return Ok(BTreeMap::from([("default".to_string(), raw.to_string())]));
    }
    let profiles: BTreeMap<String, String> =
        serde_json::from_str(raw).context("GPUI_MCP_LAUNCH is not a JSON object of strings")?;
    if profiles.is_empty() {
        bail!("GPUI_MCP_LAUNCH names no command");
    }
    Ok(profiles)
}

/// The asked-for profile, or the only one there is.
fn choose_profile(profiles: &BTreeMap<String, String>, asked: Option<&str>) -> Result<String> {
    let names = || profiles.keys().cloned().collect::<Vec<_>>().join(", ");
    match asked {
        Some(name) if profiles.contains_key(name) => Ok(name.to_string()),
        Some(name) => bail!("no launch profile `{name}`; configured: {}", names()),
        None if profiles.len() == 1 => Ok(profiles.keys().next().cloned().unwrap_or_default()),
        None => bail!("say which profile to launch: {}", names()),
    }
}

fn log_path() -> PathBuf {
    launch_file("log")
}

/// The launcher's process id — the id of the process group it leads.
fn pid_path() -> PathBuf {
    launch_file("pid")
}

fn launch_file(extension: &str) -> PathBuf {
    let app = std::env::var("GPUI_MCP_APP").unwrap_or_else(|_| "app".into());
    std::env::temp_dir().join(format!("gpui-mcp-launch-{app}.{extension}"))
}

/// A launcher an earlier server started and did not end, if one is still
/// running.
fn orphaned_launcher() -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pid_path())
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let alive = if cfg!(windows) {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
    } else {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    alive.then_some(pid)
}

fn spawn(command_line: &str, log: &Path) -> Result<Child> {
    let out = std::fs::File::create(log).with_context(|| format!("creating {}", log.display()))?;
    let err = out.try_clone()?;
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.args(["/C", command_line]);
        command
    } else {
        let mut command = Command::new("sh");
        command.args(["-c", command_line]);
        command
    };
    if let Some(dir) = std::env::var_os("GPUI_MCP_LAUNCH_DIR").filter(|dir| !dir.is_empty()) {
        command.current_dir(dir);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .with_context(|| format!("starting `{command_line}`"))
}

/// Ask a process — or with `group`, the process group it leads — to end.
fn terminate(pid: u32, group: bool) {
    if cfg!(windows) {
        let mut command = Command::new("taskkill");
        command.args(["/PID", &pid.to_string()]);
        if group {
            command.args(["/T", "/F"]);
        }
        command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
    } else {
        let target = if group {
            format!("-{pid}")
        } else {
            pid.to_string()
        };
        Command::new("kill")
            .args(["-TERM", "--", &target])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
    }
}

fn kill(pid: u32) {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("taskkill");
        command.args(["/F", "/PID", &pid.to_string()]);
        command
    } else {
        let mut command = Command::new("kill");
        command.args(["-KILL", &pid.to_string()]);
        command
    };
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok();
}

/// The line saying a build failed, if the log's end has one: `dx`'s, which
/// keeps serving after a failed build, and cargo's own.
fn build_failure(log_tail: &str) -> Option<String> {
    log_tail
        .lines()
        .rev()
        .find(|line| line.contains("Build failed") || line.contains("error: could not compile"))
        .map(|line| line.trim().chars().take(200).collect())
}

/// The last `lines` lines of the log, without terminal colour codes.
fn tail(log: &Path, lines: usize) -> String {
    last_lines(&read_from(log, 0).0, lines)
}

/// The log from byte `offset` on, and its length.
fn read_from(log: &Path, offset: u64) -> (String, u64) {
    let bytes = std::fs::read(log).unwrap_or_default();
    let start = (offset as usize).min(bytes.len());
    (
        String::from_utf8_lossy(&bytes[start..]).into_owned(),
        bytes.len() as u64,
    )
}

fn last_lines(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    strip_ansi(&all[start..].join("\n"))
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameters and intermediates, up to the final byte.
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_command_is_the_default_profile() {
        let profiles = parse_profiles("cargo xtask run").unwrap();
        assert_eq!(
            profiles.get("default").map(String::as_str),
            Some("cargo xtask run")
        );
        assert_eq!(choose_profile(&profiles, None).unwrap(), "default");
    }

    #[test]
    fn named_profiles_need_a_name_when_there_are_several() {
        let profiles =
            parse_profiles(r#"{"run": "cargo xtask run", "hot": "cargo xtask hot"}"#).unwrap();
        assert_eq!(choose_profile(&profiles, Some("hot")).unwrap(), "hot");
        let unnamed = choose_profile(&profiles, None).unwrap_err().to_string();
        assert!(unnamed.contains("hot, run"), "{unnamed}");
        let unknown = choose_profile(&profiles, Some("ship"))
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("no launch profile `ship`"), "{unknown}");
    }

    #[test]
    fn nothing_configured_says_how_to_configure_it() {
        let error = parse_profiles("  ").unwrap_err().to_string();
        assert!(error.contains("GPUI_MCP_LAUNCH"), "{error}");
        assert!(parse_profiles("{}").is_err());
        assert!(parse_profiles("{\"run\": 3}").is_err());
    }

    #[test]
    fn a_failed_build_is_recognised_by_its_own_words() {
        let dx = " 69.48s ERROR aborting due to 1 previous error\n 69.51s ERROR Build failed: \
                  Cargo build failed - no output location.";
        assert!(build_failure(dx).is_some_and(|line| line.contains("Build failed")));
        let cargo =
            "error[E0308]: mismatched types\nerror: could not compile `elane` (bin \"elane\")";
        assert!(build_failure(cargo).is_some());
        assert_eq!(
            build_failure("Compiling elane v0.9.1\nFinished `dev` profile"),
            None
        );
    }

    /// After a failure was reported, a resumed wait looks only at what the
    /// dev server wrote since: the old failure is not news.
    #[test]
    fn a_reported_failure_is_not_reported_again() {
        let dir = std::env::temp_dir().join(format!("gpui-mcp-again-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log");
        std::fs::write(&log, "Build failed: one\n").unwrap();
        let (fresh, length) = read_from(&log, 0);
        assert!(build_failure(&fresh).is_some());
        std::fs::write(&log, "Build failed: one\nCompiling elane\n").unwrap();
        let (fresh, _) = read_from(&log, length);
        assert_eq!(fresh, "Compiling elane\n");
        assert_eq!(build_failure(&fresh), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn colour_codes_are_left_out_of_the_log() {
        assert_eq!(
            strip_ansi("\u{1b}[33mwarning\u{1b}[0m: Waiting \u{1b}[2;37mfor\u{1b}[0m"),
            "warning: Waiting for"
        );
    }

    #[test]
    fn the_tail_is_the_end_of_the_log() {
        let dir = std::env::temp_dir().join(format!("gpui-mcp-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log");
        std::fs::write(&log, "one\ntwo\nthree\n").unwrap();
        assert_eq!(tail(&log, 2), "two\nthree");
        assert_eq!(tail(&log, 10), "one\ntwo\nthree");
        assert_eq!(tail(&dir.join("missing"), 3), "");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A launcher that fails before the app answers comes back as an error
    /// carrying the end of its log, not as a timeout.
    #[cfg(unix)]
    #[test]
    fn a_failed_build_answers_with_its_output() {
        // Before the stand-in starts: it has to outlive the wait for the lock.
        let _profiles = profiles_set(r#"{"run": "true"}"#);
        let mut launcher = Launcher::default();
        let log = std::env::temp_dir().join(format!("gpui-mcp-fail-{}.log", std::process::id()));
        let child = spawn(
            // Still running when launch_app looks, so it waits for this one
            // rather than starting anew.
            "echo 'error[E0425]: cannot find value'; sleep 1; exit 101",
            &log,
        )
        .unwrap();
        launcher.current = Some(Launch {
            profile: "run".into(),
            child,
            log: log.clone(),
            before: Vec::new(),
            started: Instant::now(),
            reported_failure_at: 0,
        });
        let error = launcher
            .launch(
                &json!({ "timeout_ms": 10_000 }),
                &|| Vec::new(),
                &|_| true,
                &mut |_| {},
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("error[E0425]"), "{error}");
        assert!(launcher.current.is_none());
        std::fs::remove_file(&log).ok();
    }

    /// The new instance is the answer; one that was already there is not.
    #[cfg(unix)]
    #[test]
    fn only_a_new_instance_counts() {
        let _profiles = profiles_set(r#"{"run": "true"}"#);
        let mut launcher = Launcher::default();
        let log = std::env::temp_dir().join(format!("gpui-mcp-new-{}.log", std::process::id()));
        let child = spawn("sleep 5", &log).unwrap();
        launcher.current = Some(Launch {
            profile: "run".into(),
            child,
            log: log.clone(),
            before: vec![7],
            started: Instant::now(),
            reported_failure_at: 0,
        });
        let seen = std::cell::Cell::new(0);
        let instances = || {
            seen.set(seen.get() + 1);
            if seen.get() < 3 {
                vec![(7, "old.sock".to_string())]
            } else {
                vec![(7, "old.sock".to_string()), (8, "new.sock".to_string())]
            }
        };
        let answer = launcher
            .launch(&json!({}), &instances, &|_| true, &mut |_| {})
            .unwrap();
        assert_eq!(answer["ready"], true);
        assert_eq!(answer["pid"], 8);
        assert_eq!(answer["resumed"], true);
        launcher.stop(&|| Vec::new()).unwrap();
        std::fs::remove_file(&log).ok();
    }

    /// `GPUI_MCP_LAUNCH` set for as long as the guard lives; tests that set
    /// it take turns.
    fn profiles_set(value: &str) -> impl Drop {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        struct Guard {
            _turn: std::sync::MutexGuard<'static, ()>,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                std::env::remove_var("GPUI_MCP_LAUNCH");
            }
        }
        let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::set_var("GPUI_MCP_LAUNCH", value);
        Guard { _turn: guard }
    }
}
