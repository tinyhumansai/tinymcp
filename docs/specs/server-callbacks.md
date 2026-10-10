# Module-owned server sessions

The compiled module accepts host declarations and newline-delimited MCP input.
It owns parsing, request validation, version negotiation, provenance and response
framing. An opaque session locks declarations and client provenance until closed.
The host drains typed callback requests and completes them after applying its own
security, approvals and domain dispatch. No callback executes host policy in the
module. Callback errors become JSON-RPC responses, not supervisor observations.

Each session accepts one outstanding input operation, including a sequential
batch. The host drains at most one outstanding callback, completes it exactly
once, and observes the replayable final response. Caller-known operation UUIDs make
identical submissions idempotent; retired IDs cannot execute again. Terminal
observations remain until a new operation acknowledges them. Cancellation targets
both session and operation so delayed requests cannot cancel a successor. Cancellation drops the protocol future and
all callback waiters. Close drops the entire session; shutdown closes every
session owned by that registry object. Caller-generated nonzero UUID-encoded counters are known before acquisition and never reused.
The session admission window permits concurrent out-of-order delivery within
4096 counters, preserving active retries outside the window. Operation counters
increase strictly per session; one high-water mark rejects retired operations
without lifetime exhaustion.
Idempotent open recovers lost replies. A bounded rolling UUID-counter admission window fences closed acquisitions
and waits for cancelled workers; late opens cannot resurrect them. Callback polls
redeliver the same outstanding identifier until completion; hosts deduplicate
execution by that identifier.
Counts and serialized input, declaration and reply bytes are bounded. Output
budgets are charged during construction, including envelopes and punctuation,
and stop batch dispatch before retaining further responses. Invalid
completions retain the waiter so a host can correct them; cancellation removes it.

This slice exposes protocol operations. Existing stdio and HTTP library entry
points remain intact. Opening module-owned listeners and bridging HTTP credentials
is a separate transport slice; a host must not recreate MCP protocol routing.
