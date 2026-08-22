//! Unit tests for the subprocess transport.
//!
//! The preflight tests run everywhere. The tests that drive a real handshake
//! stand up a fake server as a shell script and are `#[cfg(unix)]`, because
//! there is no portable way to write a one-line JSON-RPC responder that both a
//! POSIX shell and `cmd.exe` understand. The transport code itself is not
//! platform-specific; what is covered on Unix holds on Windows.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::McpStdioClient;
use crate::Error;
use tinymcp_bus::{LATEST_PROTOCOL_VERSION, McpClientIdentityConfig};

/// A client for `command` with no arguments and no environment.
fn client_for(command: &str, env: Vec<(String, String)>) -> McpStdioClient {
    McpStdioClient::new(
        command,
        Vec::new(),
        env,
        None,
        &McpClientIdentityConfig::default(),
    )
}

/// A path with nothing on it, to force the missing-command branch regardless of
/// what this machine has installed.
fn empty_path() -> Vec<(String, String)> {
    vec![(
        "PATH".to_string(),
        "/tinymcp/deliberately/does/not/exist".to_string(),
    )]
}

// ---------------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_command_fails_before_the_spawn_with_actionable_guidance() {
    let client = client_for("tinymcp-nonexistent-binary-zzz", Vec::new());

    let error = client.initialize().await.expect_err("a missing command");

    assert!(error.to_string().contains("was not found"), "{error}");
}

#[tokio::test]
async fn a_missing_node_runtime_says_so_by_name() {
    // The path override forces this branch deterministically, so the test says
    // the same thing on a machine that has Node and one that does not.
    let client = client_for("npx", empty_path());

    let error = client.initialize().await.expect_err("a missing npx");

    assert!(error.to_string().contains("Node.js"), "{error}");
}

#[tokio::test]
async fn a_missing_uv_runtime_says_so_by_name() {
    let client = client_for("uvx", empty_path());

    let error = client.initialize().await.expect_err("a missing uvx");

    assert!(error.to_string().contains("uv"), "{error}");
}

#[tokio::test]
async fn a_missing_command_names_the_command_it_looked_for() {
    let client = client_for("some-bespoke-server", empty_path());

    let error = client.initialize().await.expect_err("a missing command");

    assert!(error.to_string().contains("some-bespoke-server"), "{error}");
}

#[tokio::test]
async fn the_client_reports_the_command_it_would_spawn() {
    assert_eq!(client_for("npx", Vec::new()).command(), "npx");
}

#[tokio::test]
async fn closing_a_session_that_was_never_opened_does_nothing() {
    client_for("npx", Vec::new())
        .close_session()
        .await
        .expect("closing an unopened session");
}

// ---------------------------------------------------------------------------
// A real handshake, against a fake server
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod against_a_fake_server {
    use super::{Error, LATEST_PROTOCOL_VERSION, McpStdioClient};
    use std::fmt::Write as _;
    use std::io::Write as _;
    use std::path::Path;
    use tinymcp_bus::McpClientIdentityConfig;

    /// Writes an executable shell script and returns its path.
    fn write_script(directory: &Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join("fake-mcp-server");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "#!/bin/sh").unwrap();
        write!(file, "{body}").unwrap();
        drop(file);

        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();

        path.to_string_lossy().into_owned()
    }

    /// A server that answers each request in order, one JSON line per reply.
    ///
    /// It reads a line per reply so it stays in step with the client rather
    /// than racing ahead and closing its output.
    fn responder(replies: &[&str]) -> String {
        let mut body = String::new();
        for reply in replies {
            body.push_str("read -r _line\n");
            let _ = writeln!(body, "printf '%s\\n' '{reply}'");
        }
        // Then wait, rather than exiting and closing the pipe under the client.
        body.push_str("cat > /dev/null\n");
        body
    }

    fn client_for(command: String) -> McpStdioClient {
        McpStdioClient::new(
            command,
            Vec::new(),
            Vec::new(),
            None,
            &McpClientIdentityConfig::default(),
        )
    }

    fn initialize_reply() -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"{LATEST_PROTOCOL_VERSION}","capabilities":{{}},"serverInfo":{{"name":"fake","version":"1"}}}}}}"#
        )
    }

    #[tokio::test]
    async fn a_handshake_completes_and_is_cached() {
        let directory = tempfile::tempdir().unwrap();
        let command = write_script(directory.path(), &responder(&[&initialize_reply()]));
        let client = client_for(command);

        let first = client.initialize().await.expect("the handshake");
        assert_eq!(first.protocol_version, LATEST_PROTOCOL_VERSION);
        assert_eq!(first.server_info["name"], "fake");

        // A second call must not spawn again — the script only answers once, so
        // this would hang or fail if it did.
        let second = client.initialize().await.expect("the cached handshake");
        assert_eq!(second.protocol_version, first.protocol_version);
    }

    #[tokio::test]
    async fn a_server_negotiating_an_unknown_version_fails_the_handshake() {
        // The HTTP transport always checked this; the subprocess one did not.
        // A local child is no more trustworthy than a remote endpoint.
        let reply = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"1999-01-01","capabilities":{},"serverInfo":{}}}"#;
        let directory = tempfile::tempdir().unwrap();
        let command = write_script(directory.path(), &responder(&[reply]));

        let error = client_for(command)
            .initialize()
            .await
            .expect_err("an unknown version");

        assert!(
            matches!(error, Error::UnsupportedProtocolVersion { ref version } if version == "1999-01-01"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn tools_are_listed_after_the_handshake() {
        let tools = r#"{"jsonrpc":"2.0","id":3,"result":{"tools":[{"name":"forecast","description":"weather"}]}}"#;
        let directory = tempfile::tempdir().unwrap();
        let command = write_script(
            directory.path(),
            &responder(&[&initialize_reply(), "", tools]),
        );

        let listed = client_for(command).list_tools().await.expect("tools/list");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "forecast");
    }

    #[tokio::test]
    async fn a_banner_on_the_output_is_skipped_rather_than_treated_as_protocol() {
        // Servers print to their output whether or not they should. Treating a
        // startup banner as a protocol violation would break servers that
        // otherwise work perfectly.
        let mut body = String::from("read -r _line\n");
        body.push_str("printf '%s\\n' 'Starting fake server v1...'\n");
        body.push_str("printf '%s\\n' ''\n");
        let _ = writeln!(body, "printf '%s\\n' '{}'", initialize_reply());
        body.push_str("cat > /dev/null\n");

        let directory = tempfile::tempdir().unwrap();
        let command = write_script(directory.path(), &body);

        let initialized = client_for(command)
            .initialize()
            .await
            .expect("the banner did not break the handshake");

        assert_eq!(initialized.server_info["name"], "fake");
    }

    #[tokio::test]
    async fn a_json_rpc_error_reply_becomes_an_rpc_error() {
        let reply = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"internal"}}"#;
        let directory = tempfile::tempdir().unwrap();
        let command = write_script(directory.path(), &responder(&[reply]));

        let error = client_for(command)
            .initialize()
            .await
            .expect_err("an rpc error");

        assert!(matches!(error, Error::Rpc { .. }), "{error:?}");
        assert!(error.to_string().contains("internal"));
    }

    #[tokio::test]
    async fn a_server_that_closes_its_output_is_reported_clearly() {
        // The failure mode when a server crashes on startup. "closed its
        // output" is what a user can act on; a hang is not.
        //
        // Whether the exit is noticed on the write or on the following read is
        // a matter of scheduling, so both paths report it the same way. Without
        // that, this test passes or fails depending on how quickly the child
        // gets torn down.
        let directory = tempfile::tempdir().unwrap();
        let command = write_script(directory.path(), "exit 0\n");

        let error = client_for(command)
            .initialize()
            .await
            .expect_err("a server that exits immediately");

        assert!(error.to_string().contains("closed its output"), "{error}");
    }

    #[tokio::test]
    async fn a_reply_with_no_result_member_is_malformed() {
        let reply = r#"{"jsonrpc":"2.0","id":1}"#;
        let directory = tempfile::tempdir().unwrap();
        let command = write_script(directory.path(), &responder(&[reply]));

        let error = client_for(command)
            .initialize()
            .await
            .expect_err("no result member");

        assert!(
            matches!(error, Error::MalformedResponse { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn closing_a_session_terminates_the_child_and_forgets_it() {
        let directory = tempfile::tempdir().unwrap();
        let command = write_script(directory.path(), &responder(&[&initialize_reply()]));
        let client = client_for(command);

        client.initialize().await.expect("the handshake");
        client.close_session().await.expect("closing");

        // The session is gone, so this would have to spawn again. The script
        // answers once more only because a fresh child starts from the top.
        client
            .initialize()
            .await
            .expect("a closed session can be reopened");
    }
}

// ---------------------------------------------------------------------------
// Working directory, environment, and how a write failure reads
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod more {
    use super::*;

    /// A client running `body` through `/bin/sh`.
    ///
    /// The script is an *argument* rather than a file the test wrote and then
    /// executes. Writing one and exec'ing it races: between another test's fork
    /// and its exec, this test's still-open descriptor makes the kernel refuse
    /// the exec with `ETXTBSY`, and the suite runs its cases in parallel. The
    /// interpreter is never written by the test, so there is nothing to race.
    fn shell_client(
        body: &str,
        env: Vec<(String, String)>,
        cwd: Option<std::path::PathBuf>,
    ) -> McpStdioClient {
        McpStdioClient::new(
            "/bin/sh",
            vec!["-c".to_string(), body.to_string()],
            env,
            cwd,
            &McpClientIdentityConfig::default(),
        )
    }

    /// One handshake reply, then wait.
    fn handshake_only() -> String {
        format!(
            "read -r _line\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\
             {{\"protocolVersion\":\"{LATEST_PROTOCOL_VERSION}\",\"capabilities\":{{}},\"serverInfo\":\
             {{\"name\":\"fake\",\"version\":\"1\"}}}}}}'\ncat > /dev/null\n"
        )
    }

    #[tokio::test]
    async fn a_working_directory_is_where_the_server_is_started() {
        // A server given a project directory has to actually be started in it;
        // resolving relative paths against the host's cwd would read the wrong
        // files.
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("marker");
        std::fs::write(&marker, "here").unwrap();

        // Fails the handshake unless `marker` is in the process's cwd.
        let body = format!("test -f marker || exit 3\n{}", handshake_only());
        let client = shell_client(&body, Vec::new(), Some(directory.path().to_path_buf()));

        assert!(client.initialize().await.is_ok());
    }

    #[tokio::test]
    async fn a_configured_variable_reaches_the_server() {
        let directory = tempfile::tempdir().unwrap();
        let body = format!(
            "test \"$API_KEY\" = \"sekrit\" || exit 3\n{}",
            handshake_only()
        );
        let client = shell_client(
            &body,
            vec![("API_KEY".to_string(), "sekrit".to_string())],
            None,
        );

        assert!(client.initialize().await.is_ok());
    }

    #[tokio::test]
    async fn a_server_that_answers_with_an_rpc_error_reports_it_as_one() {
        // Distinct from a transport failure: the server answered, and it said
        // no. Telling the user their network is broken would send them looking
        // in the wrong place.
        let directory = tempfile::tempdir().unwrap();
        let body = "read -r _line\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\
                    \"error\":{\"code\":-32601,\"message\":\"method not found\"}}'\n\
                    cat > /dev/null\n";
        let client = shell_client(&body, Vec::new(), None);

        let error = client.initialize().await.expect_err("an rpc error");

        assert!(matches!(error, Error::Rpc { .. }), "{error:?}");
        assert!(error.to_string().contains("method not found"), "{error}");
    }

    #[tokio::test]
    async fn a_banner_line_on_the_output_is_skipped_rather_than_read_as_a_reply() {
        // Printing a startup banner to stdout is a common mistake in a server,
        // and it must not break the handshake of an otherwise working one.
        let directory = tempfile::tempdir().unwrap();
        let body = format!("printf '%s\\n' 'Server v1 starting'\n{}", handshake_only());
        let client = shell_client(&body, Vec::new(), None);

        assert!(client.initialize().await.is_ok());
    }

    #[tokio::test]
    async fn a_line_that_looks_like_json_but_is_not_is_reported_with_what_was_read() {
        // Unlike a banner, this cannot be skipped: it is where a reply should
        // be, and the offending text is what makes it diagnosable.
        let directory = tempfile::tempdir().unwrap();
        let client = shell_client(
            "read -r _line\nprintf '%s\\n' '{ not json'\ncat > /dev/null\n",
            Vec::new(),
            None,
        );

        let error = client.initialize().await.expect_err("not json");

        assert!(error.to_string().contains("not json"), "{error}");
        assert!(error.to_string().contains("{ not json"), "{error}");
    }

    #[tokio::test]
    async fn a_reply_carrying_neither_a_result_nor_an_error_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let client = shell_client(
            "read -r _line\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1}'\n\
             cat > /dev/null\n",
            Vec::new(),
            None,
        );

        let error = client.initialize().await.expect_err("no result");

        assert!(error.to_string().contains("no `result`"), "{error}");
    }

    #[tokio::test]
    async fn a_server_that_exits_immediately_says_the_server_is_gone() {
        // Noticed on the write or on the read depending on scheduling, so both
        // paths have to end at the same wording; a bare "broken pipe" tells a
        // user nothing they can act on.
        let directory = tempfile::tempdir().unwrap();
        let client = shell_client("exit 0\n", Vec::new(), None);

        let error = client.initialize().await.expect_err("the server exited");
        let message = error.to_string();

        assert!(
            message.contains("closed its output"),
            "expected the server-has-gone wording, got: {message}"
        );
    }
}

// ---------------------------------------------------------------------------
// How a write failure reads
// ---------------------------------------------------------------------------
//
// Driven directly rather than through a server that exits: whether the failure
// lands on the write or on the read depends on scheduling, so a test that races
// for it covers one branch on one machine and the other branch elsewhere.

#[test]
fn a_broken_pipe_is_reported_as_the_server_having_gone() {
    // The wording matters more than the classification: "broken pipe" tells a
    // user nothing they can act on, and this is the common case of a server
    // that exits during startup.
    let client = client_for("weather-mcp", Vec::new());

    let error = client.write_failure(
        "writing to",
        &std::io::Error::from(std::io::ErrorKind::BrokenPipe),
    );

    assert!(error.to_string().contains("closed its output"), "{error}");
    assert!(error.to_string().contains("weather-mcp"), "{error}");
}

#[test]
fn any_other_write_failure_says_what_was_being_attempted() {
    // A permission or resource failure is not the server having gone, and
    // collapsing it onto that wording would send the user looking in the wrong
    // place.
    let client = client_for("weather-mcp", Vec::new());

    let error = client.write_failure(
        "flushing to",
        &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    );

    let message = error.to_string();
    assert!(message.contains("flushing to"), "{message}");
    assert!(message.contains("weather-mcp"), "{message}");
    assert!(!message.contains("closed its output"), "{message}");
}
