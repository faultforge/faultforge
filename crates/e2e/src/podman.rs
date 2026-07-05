//! A typed wrapper over the `podman` CLI.
//!
//! The harness shells out to `podman` rather than a docker-compat socket: the
//! CLI is the one interface that behaves identically on macOS `podman machine`
//! and rootless Linux CI (design D1). Every helper captures stdout/stderr and,
//! on a non-zero exit or a spawn failure, returns a [`PodmanError`] naming the
//! exact invocation — an opted-in run wants a real error, never a silent skip
//! (spec: "explicit run without podman fails loudly").

use std::ffi::OsStr;
use std::process::Command;

/// Why a `podman` invocation failed.
#[derive(Debug, thiserror::Error)]
pub enum PodmanError {
    /// `podman` could not be spawned at all (not installed / not on `PATH`).
    #[error("could not run `podman {args}`: {source} — is podman installed and running?")]
    Spawn {
        /// The joined argument string, for diagnostics.
        args: String,
        /// The underlying spawn error.
        source: std::io::Error,
    },
    /// `podman` ran but exited non-zero.
    #[error("`podman {args}` failed (exit {code}):\n{stderr}")]
    Exit {
        /// The joined argument string.
        args: String,
        /// The process exit code (`-1` when killed by a signal).
        code: i32,
        /// Captured stderr, for diagnostics.
        stderr: String,
    },
}

/// The captured result of a successful `podman` invocation.
#[derive(Debug, Clone)]
pub struct Output {
    /// Captured stdout, trailing newline trimmed.
    pub stdout: String,
    /// Captured stderr, trailing newline trimmed.
    pub stderr: String,
}

/// Run `podman <args...>`, returning captured output or a diagnostic error.
///
/// # Errors
///
/// Returns [`PodmanError::Spawn`] if `podman` cannot be launched, or
/// [`PodmanError::Exit`] if it exits non-zero.
pub fn run<I, S>(args: I) -> Result<Output, PodmanError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    let joined = args.join(" ");

    let output = Command::new("podman")
        .args(&args)
        .output()
        .map_err(|source| PodmanError::Spawn {
            args: joined.clone(),
            source,
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string();
    let stderr = String::from_utf8_lossy(&output.stderr)
        .trim_end()
        .to_string();

    if output.status.success() {
        Ok(Output { stdout, stderr })
    } else {
        Err(PodmanError::Exit {
            args: joined,
            code: output.status.code().unwrap_or(-1),
            stderr,
        })
    }
}

/// `podman build --target <target> -t <tag> -f <containerfile> <context>`.
///
/// # Errors
///
/// Propagates any [`PodmanError`] from the build.
pub fn build(
    containerfile: &str,
    context: &str,
    target: &str,
    tag: &str,
) -> Result<Output, PodmanError> {
    run([
        "build",
        "--target",
        target,
        "-t",
        tag,
        "-f",
        containerfile,
        context,
    ])
}

/// `podman network create <name>`.
///
/// # Errors
///
/// Propagates any [`PodmanError`].
pub fn network_create(name: &str) -> Result<Output, PodmanError> {
    run(["network", "create", name])
}

/// `podman volume create <name>`.
///
/// # Errors
///
/// Propagates any [`PodmanError`].
pub fn volume_create(name: &str) -> Result<Output, PodmanError> {
    run(["volume", "create", name])
}

/// Run a detached container from a fully-formed argument list (everything after
/// `podman run -d`).
///
/// # Errors
///
/// Propagates any [`PodmanError`].
pub fn run_detached(extra: &[String]) -> Result<Output, PodmanError> {
    let mut args = vec!["run".to_string(), "-d".to_string()];
    args.extend_from_slice(extra);
    run(args)
}

/// `podman exec [--user <user>] <container> <cmd...>`.
///
/// # Errors
///
/// Propagates any [`PodmanError`]; a non-zero command exit surfaces as
/// [`PodmanError::Exit`] so callers can distinguish success from failure.
pub fn exec(container: &str, user: Option<&str>, cmd: &[&str]) -> Result<Output, PodmanError> {
    let mut args = vec!["exec".to_string()];
    if let Some(user) = user {
        args.push("--user".to_string());
        args.push(user.to_string());
    }
    args.push(container.to_string());
    args.extend(cmd.iter().map(ToString::to_string));
    run(args)
}

/// `podman port <container> <internal_port>`, returning the first mapped
/// `host:port` line (e.g. `127.0.0.1:49153`).
///
/// # Errors
///
/// Propagates any [`PodmanError`]; the mapping is missing if the container did
/// not publish the port.
pub fn port(container: &str, internal_port: u16) -> Result<String, PodmanError> {
    let out = run(["port", container, &internal_port.to_string()])?;
    Ok(out.stdout.lines().next().unwrap_or("").trim().to_string())
}

/// `podman logs <container>` (best-effort; returns the captured text or an error
/// string so teardown can dump it without unwrapping).
#[must_use]
pub fn logs(container: &str) -> String {
    match run(["logs", container]) {
        Ok(out) => format!(
            "--- stdout ---\n{}\n--- stderr ---\n{}",
            out.stdout, out.stderr
        ),
        Err(e) => format!("<could not read logs: {e}>"),
    }
}

/// `podman stop -t <secs> <container>` (best-effort teardown/scenario step).
///
/// # Errors
///
/// Propagates any [`PodmanError`].
pub fn stop(container: &str, timeout_secs: u32) -> Result<Output, PodmanError> {
    run(["stop", "-t", &timeout_secs.to_string(), container])
}

/// `podman kill <container>`.
///
/// # Errors
///
/// Propagates any [`PodmanError`].
pub fn kill(container: &str) -> Result<Output, PodmanError> {
    run(["kill", container])
}

/// `podman start <container>`.
///
/// # Errors
///
/// Propagates any [`PodmanError`].
pub fn start(container: &str) -> Result<Output, PodmanError> {
    run(["start", container])
}

/// `podman rm -f <container>` (best-effort; teardown ignores the result).
pub fn rm_container(container: &str) {
    let _ = run(["rm", "-f", container]);
}

/// `podman volume rm -f <name>` (best-effort).
pub fn rm_volume(name: &str) {
    let _ = run(["volume", "rm", "-f", name]);
}

/// `podman network rm -f <name>` (best-effort).
pub fn rm_network(name: &str) {
    let _ = run(["network", "rm", "-f", name]);
}
