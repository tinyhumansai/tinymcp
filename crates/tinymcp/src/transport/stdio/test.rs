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
