# Jailed stdio servers

A stdio MCP server is third-party code. The module starts one of three ways:

| Server | Started |
| --- | --- |
| `McpServerConfig::operator_trusted = true` | As an ordinary child process, unconfined. Only the host's own configuration should set it. |
| Any other, with `McpClientConfig::stdio_sandbox` set | Inside the `TinyBox` native jail, through the `TinyBox` bus contract (`StartStdio`, contract 1.21). |
| Any other, with no sandbox | Refused. There is no fallback to an unconfined start. |

Every registry-installed server (catalog installs and setup test connections) is
in the second or third row; installed servers are never operator-trusted.

## The jail

`StdioSandboxConfig` carries the workspace (the server's writable root), read
and write grants, `deny_read` paths (credential stores), network and subprocess
permissions, and whether the backend must meet the untrusted-code isolation
floor (on by default), and `enforcement` (contract 1.27). `enforcement`
mirrors TinyBox's `JailEnforcement`: `Enforced` (the default) asks the backend to
enforce every grant, so a backend that cannot refuses and the server does not
start; `Declared` accepts the backend's advertised partial constraints, for a
host that has judged such a backend enough (for example macOS Seatbelt, which
confines folders). The launcher passes the value to TinyBox unchanged; only the
host chooses it, and existing hosts keep `Enforced`.

The launcher (`transport::stdio::jailed::JailedSpawner`) reserves a passthrough
resource over the workspace, starts the server with `StartStdio`, and pumps its
stdin and stdout over `WriteStdio` and `ReadStdio` (standard error is discarded,
as for a direct child). Closing the session, or dropping it, calls `KillStdio`
and `Close`. `StartStdio` carries the server's environment (credentials), so it is sent over the bus's confidential route and fails when that delivery is unavailable. The module reaches `TinyBox` only through `tinybox-bus`; the
module declares it as an optional dependency.

## Seam

`StdioSpawner` gained `spawn_process`, which returns a `StdioProcess` (pipes plus
a handle). Its default wraps `spawn`, so existing spawners are unchanged. A
spawner whose server is not a child of this process overrides it.

The jailed launcher is covered by `transport/stdio/jailed_tests.rs`.
