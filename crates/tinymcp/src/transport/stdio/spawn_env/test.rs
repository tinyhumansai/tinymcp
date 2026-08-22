//! Unit tests for the spawn-environment resolution.
//!
//! The pure helpers — marker parsing, merging, ordering, version comparison —
//! are tested directly. The parts that touch the machine are tested against
//! temporary directories rather than against whatever this machine happens to
//! have installed, so the suite says the same thing on a developer's laptop and
//! in a container with no Node at all.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::{
    PATH_MARK_END, PATH_MARK_START, PATH_SEP, join_dirs, locate_command, merge_path_strings,
    missing_command_error, order_sources, parse_marked_path, parse_version, spawn_path,
};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Creates a file, optionally with the execute bit set.
fn write_file(path: &Path, executable: bool) {
    std::fs::File::create(path)
        .unwrap()
        .write_all(b"x")
        .unwrap();

    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }
    #[cfg(not(unix))]
    let _ = executable;
}

/// Joins path entries with this platform's separator.
fn path_of(entries: &[&str]) -> String {
    entries.join(&PATH_SEP.to_string())
}

// ---------------------------------------------------------------------------
// parse_marked_path
// ---------------------------------------------------------------------------

#[test]
fn a_fenced_value_is_extracted_from_surrounding_noise() {
    // Startup files print banners, motds, and prompts. The markers are what
    // separate the value from all of it.
    let output = format!("Welcome!\n{PATH_MARK_START}/usr/bin:/bin{PATH_MARK_END}\n$ ");
    assert_eq!(parse_marked_path(&output).as_deref(), Some("/usr/bin:/bin"));
}

#[test]
fn a_value_is_trimmed() {
    let output = format!("{PATH_MARK_START}  /usr/bin  {PATH_MARK_END}");
    assert_eq!(parse_marked_path(&output).as_deref(), Some("/usr/bin"));
}

#[test]
fn output_without_markers_yields_nothing() {
    assert!(parse_marked_path("/usr/bin:/bin").is_none());
    assert!(parse_marked_path("").is_none());
}

#[test]
fn output_with_only_an_opening_marker_yields_nothing() {
    // A shell killed mid-print. Half a fence is not a value.
    let output = format!("{PATH_MARK_START}/usr/bin");
    assert!(parse_marked_path(&output).is_none());
}

#[test]
fn an_empty_fenced_value_yields_nothing() {
    // A shell that exports no path should fall back, not set an empty one.
    let output = format!("{PATH_MARK_START}{PATH_MARK_END}");
    assert!(parse_marked_path(&output).is_none());

    let whitespace = format!("{PATH_MARK_START}   {PATH_MARK_END}");
    assert!(parse_marked_path(&whitespace).is_none());
}

// ---------------------------------------------------------------------------
// merge_path_strings
// ---------------------------------------------------------------------------

#[test]
fn merging_keeps_the_first_occurrence_of_a_duplicate() {
    // First-seen order is what makes the leading source authoritative.
    let merged = merge_path_strings([
        path_of(&["/a", "/b"]).as_str(),
        path_of(&["/b", "/c"]).as_str(),
    ]);
    assert_eq!(merged, path_of(&["/a", "/b", "/c"]));
}

#[test]
fn merging_skips_empty_entries() {
    // A trailing separator in a path means the current directory to some
    // shells, and carrying that into a child's environment is not something to
    // do by accident.
    let merged = merge_path_strings([path_of(&["/a", "", "/b"]).as_str(), ""]);
    assert_eq!(merged, path_of(&["/a", "/b"]));
}

#[test]
fn merging_trims_each_entry() {
    let merged = merge_path_strings([path_of(&["  /a  ", "/b"]).as_str()]);
    assert_eq!(merged, path_of(&["/a", "/b"]));
}

#[test]
fn merging_nothing_yields_nothing() {
    assert_eq!(merge_path_strings(["", ""]), "");
}

// ---------------------------------------------------------------------------
// order_sources
// ---------------------------------------------------------------------------

#[test]
fn a_successful_probe_leads_so_the_shells_node_version_wins() {
    // The whole point: if the user's shell selected Node 20, a fallback
    // directory holding Node 18 must not shadow it.
    let ordered = order_sources(Some("/shell"), "/process", "/fallback");
    assert_eq!(ordered, ["/shell", "/process", "/fallback"]);
}

#[test]
fn a_failed_probe_lets_the_fallback_directories_lead() {
    // With no probe, the fallback is the only source of version-manager
    // locations, so it has to come before the stripped process path.
    let ordered = order_sources(None, "/process", "/fallback");
    assert_eq!(ordered, ["/fallback", "/process"]);
}

// ---------------------------------------------------------------------------
// join_dirs and parse_version
// ---------------------------------------------------------------------------

#[test]
fn joining_uses_this_platforms_separator() {
    let joined = join_dirs(&[PathBuf::from("/a"), PathBuf::from("/b")]);
    assert_eq!(joined, path_of(&["/a", "/b"]));
}

#[test]
fn joining_nothing_yields_an_empty_string() {
    assert_eq!(join_dirs(&[]), "");
}

#[test]
fn a_version_parses_with_or_without_its_leading_v() {
    assert_eq!(parse_version("v22.11.0"), Some(vec![22, 11, 0]));
    assert_eq!(parse_version("18.3"), Some(vec![18, 3]));
    assert_eq!(parse_version("20"), Some(vec![20]));
}

#[test]
fn versions_order_numerically_rather_than_lexically() {
    // The bug this prevents: "v9" sorting above "v10" as text.
    assert!(parse_version("v10.0.0") > parse_version("v9.0.0"));
    assert!(parse_version("v22.11.0") > parse_version("v22.9.0"));
}

#[test]
fn a_non_numeric_version_parses_to_nothing() {
    // `nvm` keeps aliases beside real versions. They must sort out, not wrong.
    for raw in ["lts/*", "system", "", "v", "v20.x", "node"] {
        assert!(parse_version(raw).is_none(), "{raw} parsed as a version");
    }
}

// ---------------------------------------------------------------------------
// locate_command
// ---------------------------------------------------------------------------

#[test]
fn an_empty_command_resolves_to_nothing() {
    assert!(locate_command("", "/usr/bin", None).is_none());
}

#[test]
fn a_bare_name_resolves_against_the_path() {
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("server");
    write_file(&binary, true);

    let found = locate_command("server", &directory.path().to_string_lossy(), None);
    assert_eq!(found.as_deref(), Some(binary.as_path()));
}

#[test]
fn a_bare_name_takes_the_first_directory_that_has_it() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_file(&first.path().join("server"), true);
    write_file(&second.path().join("server"), true);

    let path = path_of(&[
        &first.path().to_string_lossy(),
        &second.path().to_string_lossy(),
    ]);
    let found = locate_command("server", &path, None).unwrap();

    assert!(found.starts_with(first.path()), "{found:?}");
}

#[test]
fn a_bare_name_that_is_nowhere_on_the_path_resolves_to_nothing() {
    let directory = tempfile::tempdir().unwrap();
    assert!(locate_command("absent", &directory.path().to_string_lossy(), None).is_none());
}

#[test]
fn an_absolute_command_is_taken_as_a_direct_path() {
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("server");
    write_file(&binary, true);

    // The path argument is deliberately somewhere else: a direct path does not
    // consult it, matching how the operating system resolves one.
    let found = locate_command(&binary.to_string_lossy(), "/nonexistent", None);
    assert_eq!(found.as_deref(), Some(binary.as_path()));
}

#[test]
fn a_relative_command_resolves_against_the_working_directory() {
    // A `./server` beside a configured working directory must not be rejected.
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("server");
    write_file(&binary, true);

    let found = locate_command("./server", "/nonexistent", Some(directory.path()));
    assert_eq!(
        found.as_deref(),
        Some(directory.path().join("./server").as_path())
    );
}

#[test]
fn a_relative_command_with_no_working_directory_is_not_invented() {
    assert!(locate_command("./server", "/nonexistent", None).is_none());
}

#[cfg(unix)]
#[test]
fn a_file_without_the_execute_bit_is_not_a_command() {
    // Resolution has to agree with what `spawn` will do. Accepting a
    // non-executable file here would turn a clear preflight failure into a
    // confusing spawn failure.
    let directory = tempfile::tempdir().unwrap();
    write_file(&directory.path().join("server"), false);

    assert!(locate_command("server", &directory.path().to_string_lossy(), None).is_none());
}

#[test]
fn a_directory_is_not_a_command() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("server")).unwrap();

    assert!(locate_command("server", &directory.path().to_string_lossy(), None).is_none());
}

#[test]
fn empty_path_entries_are_skipped_rather_than_treated_as_the_current_directory() {
    let directory = tempfile::tempdir().unwrap();
    write_file(&directory.path().join("server"), true);

    let path = path_of(&["", &directory.path().to_string_lossy(), ""]);
    assert!(locate_command("server", &path, None).is_some());
}

// ---------------------------------------------------------------------------
// missing_command_error
// ---------------------------------------------------------------------------

#[test]
fn a_missing_node_runtime_names_node_and_where_to_get_it() {
    for command in ["npx", "npm", "node"] {
        let message = missing_command_error(command);
        assert!(message.contains("Node.js"), "{message}");
        assert!(message.contains("nodejs.org"), "{message}");
        assert!(message.contains("was not found"), "{message}");
    }
}

#[test]
fn a_missing_uv_runtime_names_uv_and_where_to_get_it() {
    for command in ["uvx", "uv"] {
        let message = missing_command_error(command);
        assert!(message.contains("uv"), "{message}");
        assert!(message.contains("astral.sh"), "{message}");
    }
}

#[test]
fn a_runtime_is_recognised_through_a_full_path() {
    // Servers are configured with `/usr/local/bin/npx` as often as with `npx`,
    // and the guidance is just as relevant.
    let message = missing_command_error("/opt/homebrew/bin/npx");
    assert!(message.contains("Node.js"), "{message}");
}

#[test]
fn a_runtime_is_recognised_regardless_of_case() {
    assert!(missing_command_error("NPX").contains("Node.js"));
}

#[test]
fn an_unrecognised_command_gets_a_path_hint() {
    let message = missing_command_error("some-bespoke-server");
    assert!(message.contains("some-bespoke-server"), "{message}");
    assert!(message.contains("PATH"), "{message}");
    assert!(!message.contains("Node.js"), "{message}");
}

#[test]
fn every_message_names_the_command_it_could_not_find() {
    for command in ["npx", "uvx", "whatever"] {
        assert!(
            missing_command_error(command).contains(command),
            "{command} was not named in its own error"
        );
    }
}

// ---------------------------------------------------------------------------
// spawn_path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_spawn_path_is_resolved_once_and_cached() {
    let first = spawn_path().await;
    let second = spawn_path().await;

    assert_eq!(first, second);
    // Whatever else is true of this machine, the process path is one of the
    // sources, so the result cannot be empty on any machine that has one.
    if std::env::var("PATH").is_ok_and(|path| !path.is_empty()) {
        assert!(!first.is_empty());
    }
}

#[tokio::test]
async fn the_spawn_path_has_no_duplicate_entries() {
    let path = spawn_path().await;
    let entries: Vec<&str> = path.split(PATH_SEP).collect();

    let mut deduplicated = entries.clone();
    deduplicated.sort_unstable();
    deduplicated.dedup();

    assert_eq!(
        entries.len(),
        deduplicated.len(),
        "the resolved path repeats an entry: {path}"
    );
}

// ---------------------------------------------------------------------------
// Finding the newest `nvm` Node
// ---------------------------------------------------------------------------
//
// Driven against a directory the test builds rather than the machine's own
// `~/.nvm`: whether this passes must not depend on whether the developer
// happens to use `nvm`, and CI runners do not.

/// Builds an `nvm` layout holding `versions`, each with a `bin` directory.
fn nvm_layout(versions: &[&str]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("tempdir");
    for version in versions {
        std::fs::create_dir_all(
            directory
                .path()
                .join("versions")
                .join("node")
                .join(version)
                .join("bin"),
        )
        .expect("create the version directory");
    }
    directory
}

#[test]
fn the_highest_installed_version_wins() {
    // Lexical order would pick `v9` over `v20`, which is the whole reason the
    // version is parsed rather than compared as a string.
    let layout = nvm_layout(&["v9.11.2", "v20.11.0", "v18.19.0"]);

    let bin = super::nvm_latest_bin_dir(layout.path()).expect("a bin directory");

    assert!(bin.ends_with("v20.11.0/bin"), "{}", bin.display());
}

#[test]
fn a_higher_patch_of_the_same_minor_wins() {
    let layout = nvm_layout(&["v20.11.0", "v20.11.9"]);

    let bin = super::nvm_latest_bin_dir(layout.path()).expect("a bin directory");

    assert!(bin.ends_with("v20.11.9/bin"), "{}", bin.display());
}

#[test]
fn a_directory_that_is_not_a_version_is_skipped() {
    // `nvm` keeps aliases and caches beside the versions.
    let layout = nvm_layout(&["v20.11.0"]);
    std::fs::create_dir_all(layout.path().join("versions").join("node").join("alias"))
        .expect("create the alias directory");

    let bin = super::nvm_latest_bin_dir(layout.path()).expect("a bin directory");

    assert!(bin.ends_with("v20.11.0/bin"), "{}", bin.display());
}

#[test]
fn a_version_with_no_bin_directory_is_skipped() {
    // A half-removed install has the version directory and nothing in it.
    let layout = nvm_layout(&["v18.19.0"]);
    std::fs::create_dir_all(layout.path().join("versions").join("node").join("v20.11.0"))
        .expect("create the empty version directory");

    let bin = super::nvm_latest_bin_dir(layout.path()).expect("a bin directory");

    assert!(bin.ends_with("v18.19.0/bin"), "{}", bin.display());
}

#[test]
fn a_layout_with_no_versions_finds_nothing() {
    let layout = nvm_layout(&[]);

    assert_eq!(super::nvm_latest_bin_dir(layout.path()), None);
}

#[test]
fn a_directory_that_does_not_exist_finds_nothing() {
    // The ordinary case on a machine without `nvm`, and it must not fail.
    let directory = tempfile::tempdir().expect("tempdir");

    assert_eq!(
        super::nvm_latest_bin_dir(&directory.path().join("absent")),
        None
    );
}

#[test]
fn the_environment_overrides_where_nvm_is_looked_for() {
    // `NVM_DIR` is how a user relocates it, and the default is only the
    // fallback.
    let home = std::path::Path::new("/home/somebody");

    let resolved = super::nvm_dir(home);

    match std::env::var_os("NVM_DIR") {
        Some(configured) => assert_eq!(resolved, std::path::PathBuf::from(configured)),
        None => assert_eq!(resolved, home.join(".nvm")),
    }
}
