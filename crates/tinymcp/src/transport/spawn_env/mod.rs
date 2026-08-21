//! Working out the environment a stdio MCP server should be spawned into.
//!
//! # The problem
//!
//! An application launched from a desktop environment — Finder or `launchd` on
//! macOS, a shell-less session on Linux — inherits a **stripped** `PATH`,
//! typically just `/usr/bin:/bin:/usr/sbin:/sbin`. That path has no Homebrew, no
//! `/usr/local/bin`, and none of the Node or Python version-manager shims that
//! `nvm`, `volta`, `bun`, and `uv` install.
//!
//! Most of the MCP ecosystem ships as `npx <package>` or `uvx <package>`. So a
//! server the user can run perfectly well in their terminal fails to spawn with
//! a bare `ENOENT`, and nothing in that error suggests the cause is a path.
//!
//! # What this does about it
//!
//! Reconstructs the path a terminal would have given the child:
//!
//! 1. Probe the user's interactive login shell (`$SHELL -ilc`) for its `$PATH`,
//!    so version managers that hook into shell startup files are honored.
//! 2. Keep a list of well-known version-manager directories as a fallback, for
//!    when the probe is unavailable — a sandbox with no interactive shell, a
//!    startup file that never exports a path, a hung profile.
//! 3. Merge them, de-duplicated, first occurrence winning.
//!
//! When the probe succeeds it is authoritative: the shell's path leads and the
//! fallback directories only fill gaps, so they never override the Node version
//! the user's shell selected. When it fails, the fallback leads instead.
//!
//! The result is computed once per process and cached, because the probe spawns
//! a login shell and running that per server would be both slow and rude.
//!
//! # Failing before the spawn
//!
//! [`locate_command`] resolves a command the way the operating system will, and
//! [`missing_command_error`] turns a miss into guidance that names the runtime
//! the user is actually missing. Checking first is what turns "ENOENT" into
//! "this server needs Node.js, which is not installed".

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::sync::OnceCell;

/// The separator this platform puts between path entries.
const PATH_SEP: char = if cfg!(windows) { ';' } else { ':' };

/// How long to wait for the login shell before giving up on it.
///
/// A user's startup files can hang — on a network mount, a version-manager that
/// reaches out, a prompt that waits on something. Three seconds is long enough
/// for a healthy shell and short enough that a sick one does not hold up every
/// server the user asked for.
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(3);

/// Fences around the probed value, so startup-file noise is not mistaken for it.
const PATH_MARK_START: &str = "__TINYMCP_PATH_START__";
/// The closing fence. See [`PATH_MARK_START`].
const PATH_MARK_END: &str = "__TINYMCP_PATH_END__";

/// The resolved path, computed once.
static SPAWN_PATH: OnceCell<String> = OnceCell::const_new();

/// The `PATH` a stdio MCP child should inherit.
///
/// Resolved once per process and cached; later calls clone the cached value.
pub async fn spawn_path() -> String {
    SPAWN_PATH.get_or_init(build_spawn_path).await.clone()
}

/// Builds the path from all three sources.
async fn build_spawn_path() -> String {
    let process_path = std::env::var("PATH").unwrap_or_default();
    let fallback = join_dirs(&version_manager_dirs());
    let login_path = login_shell_path().await;

    let resolved = merge_path_strings(order_sources(
        login_path.as_deref(),
        &process_path,
        &fallback,
    ));

    tracing::debug!(
        probed_login_shell = login_path.is_some(),
        entries = resolved.split(PATH_SEP).count(),
        "resolved the stdio spawn path"
    );
    resolved
}

/// Probes the user's interactive login shell for its `$PATH`.
///
/// A Windows graphical process already inherits the full user path, so there is
/// no login-shell dance to perform.
#[cfg(windows)]
async fn login_shell_path() -> Option<String> {
    None
}

/// Probes the user's interactive login shell for its `$PATH`.
#[cfg(not(windows))]
async fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    // The markers fence the value off from anything else the startup files
    // print — a banner, a message of the day, a prompt — so what comes back is
    // the path and nothing else.
    let script = format!("printf '{PATH_MARK_START}%s{PATH_MARK_END}' \"$PATH\"");

    let probe = tokio::process::Command::new(&shell)
        .arg("-ilc")
        .arg(&script)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        // Without this the timeout below only drops the future: tokio leaves
        // the child running, so a hung startup file would outlive the timeout
        // that exists to contain it.
        .kill_on_drop(true)
        .output();

    let stdout = match tokio::time::timeout(LOGIN_SHELL_TIMEOUT, probe).await {
        Ok(Ok(output)) if output.status.success() => output.stdout,
        Ok(Ok(output)) => {
            tracing::debug!(
                shell = %shell,
                status = ?output.status.code(),
                "the login-shell path probe exited non-zero"
            );
            return None;
        }
        Ok(Err(error)) => {
            tracing::debug!(shell = %shell, "the login-shell path probe could not run: {error}");
            return None;
        }
        Err(_) => {
            tracing::debug!(shell = %shell, "the login-shell path probe timed out");
            return None;
        }
    };

    parse_marked_path(&String::from_utf8_lossy(&stdout))
}

/// Extracts the value fenced between the probe markers.
///
/// Returns `None` when the markers are absent or fence nothing.
fn parse_marked_path(text: &str) -> Option<String> {
    let start = text.find(PATH_MARK_START)? + PATH_MARK_START.len();
    let rest = text.get(start..)?;
    let end = rest.find(PATH_MARK_END)?;
    let path = rest.get(..end)?.trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// Version-manager directories that exist on this machine.
///
/// A fallback for when the login-shell probe is unavailable. The probe is
/// authoritative whenever it succeeds, because it reflects the version the user
/// actually selected.
fn version_manager_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home) = dirs::home_dir() {
        // `volta` and `bun` publish stable shim directories. `nvm` needs a
        // version lookup. `fnm` is left out deliberately: it has no stable shim
        // directory and requires shell evaluation, so the probe is the only
        // thing that can find it anyway.
        push_if_dir(&mut dirs, home.join(".local").join("bin"));
        push_if_dir(&mut dirs, home.join(".volta").join("bin"));
        push_if_dir(&mut dirs, home.join(".bun").join("bin"));
        push_if_dir(&mut dirs, home.join(".cargo").join("bin"));
        dirs.extend(nvm_latest_bin_dir(&home));
    }

    for fixed in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/local/sbin"] {
        push_if_dir(&mut dirs, PathBuf::from(fixed));
    }

    dirs
}

/// The `bin` directory of the highest `nvm`-installed Node version, if any.
fn nvm_latest_bin_dir(home: &Path) -> Option<PathBuf> {
    let nvm_dir = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".nvm"));
    let versions = nvm_dir.join("versions").join("node");

    let mut latest: Option<(Vec<u32>, PathBuf)> = None;
    for entry in std::fs::read_dir(&versions).ok()?.flatten() {
        let Some(version) = parse_version(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let bin = entry.path().join("bin");
        if !bin.is_dir() {
            continue;
        }
        match &latest {
            Some((highest, _)) if *highest >= version => {}
            _ => latest = Some((version, bin)),
        }
    }

    latest.map(|(_, bin)| bin)
}

/// Appends `dir` when it exists and is a directory.
fn push_if_dir(dirs: &mut Vec<PathBuf>, dir: PathBuf) {
    if dir.is_dir() {
        dirs.push(dir);
    }
}

/// Parses a dotted version into numeric components, for ordering.
///
/// Accepts a leading `v`. Returns `None` when any component is not a number, so
/// a directory like `lts/*` or `system` sorts out rather than sorting wrong.
fn parse_version(raw: &str) -> Option<Vec<u32>> {
    let trimmed = raw.trim();
    let stripped = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let parts: Option<Vec<u32>> = stripped
        .split('.')
        .map(|part| part.parse::<u32>().ok())
        .collect();
    parts.filter(|parts| !parts.is_empty())
}

/// Orders the three sources so the authoritative one leads.
///
/// A successful probe *is* the user's terminal environment, so it wins: the
/// shell's path, then the inherited process path, then the fallback directories
/// only to fill gaps. When the probe fails, the fallback is the only source of
/// version-manager locations and therefore leads.
fn order_sources<'a>(login: Option<&'a str>, process: &'a str, fallback: &'a str) -> Vec<&'a str> {
    match login {
        Some(login) => vec![login, process, fallback],
        None => vec![fallback, process],
    }
}

/// Joins directories into one path-style string.
fn join_dirs(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|dir| dir.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(&PATH_SEP.to_string())
}

/// Merges path-style sources left to right, keeping the first occurrence.
fn merge_path_strings<'a>(sources: impl IntoIterator<Item = &'a str>) -> String {
    let mut seen = HashSet::new();
    let mut merged: Vec<String> = Vec::new();

    for source in sources {
        for entry in source.split(PATH_SEP) {
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                continue;
            }
            if seen.insert(trimmed.to_string()) {
                merged.push(trimmed.to_string());
            }
        }
    }

    merged.join(&PATH_SEP.to_string())
}

/// Resolves `command` against `path` the way the operating system will.
///
/// A command containing a path separator is treated as a direct path, matching
/// `execvp` and `CreateProcess`. A *relative* direct path resolves against
/// `cwd` when one is set, mirroring the child's working directory so a
/// `./server` with a configured directory is not rejected.
///
/// Returns the resolved path, or `None` when nothing matches.
///
/// # Examples
///
/// ```
/// # use tinymcp::transport::spawn_env::locate_command;
/// // An empty command resolves to nothing rather than to the current
/// // directory.
/// assert!(locate_command("", "/usr/bin", None).is_none());
/// ```
#[must_use]
pub fn locate_command(command: &str, path: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    if command.is_empty() {
        return None;
    }

    if has_path_separator(command) {
        let candidate = PathBuf::from(command);
        let resolved = match (candidate.is_relative(), cwd) {
            (true, Some(directory)) => directory.join(&candidate),
            _ => candidate,
        };
        return is_executable_file(&resolved).then_some(resolved);
    }

    for directory in path.split(PATH_SEP) {
        if directory.is_empty() {
            continue;
        }
        for candidate in executable_candidates(Path::new(directory).join(command)) {
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }

    None
}

/// Whether `command` names a path rather than a bare executable name.
fn has_path_separator(command: &str) -> bool {
    command.contains('/') || (cfg!(windows) && command.contains('\\'))
}

/// Whether the operating system would accept `path` as an executable.
///
/// The check has to match what `spawn` will do, or the preflight is worse than
/// useless: it would reject something that works, or accept something that does
/// not. On Unix that means the execute bit; on Windows executability comes from
/// the extension, which [`executable_candidates`] has already enumerated.
fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Every filename the operating system would try for `base`.
#[cfg(windows)]
fn executable_candidates(base: PathBuf) -> Vec<PathBuf> {
    let mut candidates = vec![base.clone()];
    let extensions =
        std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());

    for extension in extensions.split(';') {
        let extension = extension.trim();
        if extension.is_empty() {
            continue;
        }
        let mut name = base.as_os_str().to_os_string();
        name.push(extension);
        candidates.push(PathBuf::from(name));
    }

    candidates
}

/// Every filename the operating system would try for `base`.
#[cfg(not(windows))]
fn executable_candidates(base: PathBuf) -> Vec<PathBuf> {
    vec![base]
}

/// Explains why a command could not be found, naming the runtime to install.
///
/// The point is that "not found" is almost never what the user needs to hear.
/// They need to hear "this server needs Node.js". The common runtimes get a
/// name and an address; anything else gets a path hint.
///
/// # Examples
///
/// ```
/// # use tinymcp::transport::spawn_env::missing_command_error;
/// assert!(missing_command_error("npx").contains("Node.js"));
/// assert!(missing_command_error("uvx").contains("uv"));
/// ```
#[must_use]
pub fn missing_command_error(command: &str) -> String {
    let lowered = command.to_ascii_lowercase();
    let base = lowered.rsplit(['/', '\\']).next().unwrap_or(&lowered);

    match base {
        "npx" | "npm" | "node" => format!(
            "`{command}` was not found. This MCP server needs Node.js, which does not appear \
             to be installed, or is not on this application's PATH. Install Node.js from \
             https://nodejs.org and restart the application."
        ),
        "uvx" | "uv" => format!(
            "`{command}` was not found. This MCP server needs uv (Python), which does not \
             appear to be installed. Install it from https://docs.astral.sh/uv/ and restart \
             the application."
        ),
        _ => format!(
            "`{command}` was not found on this application's PATH. Install it, or its runtime, \
             make sure it is available in your shell, then restart the application."
        ),
    }
}

#[cfg(test)]
mod test;
