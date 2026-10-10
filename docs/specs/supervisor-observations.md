# Supervisor observations through TinyBus

Contract 1.7 adds DrainSupervisorEvents(limit), taking one positional argument
and returning SupervisorBatch. Existing methods keep their arities and wire
forms. ServerRef, ProbeOutcome, SupervisorEvent and TickReport are serialized
contract vocabulary; the implementation re-exports the same types for library
compatibility. Reconnection, probes, backoff and parking stay in the module.
Older compatible hosts decode an unknown future observation or probe outcome
as `Unknown` and may ignore it; malformed known variants still fail decoding.

Each registry object owns one queue consumed by its host adapter. Background
maintenance records events in observation order, including a drop followed by
reconnection in the same cycle. Drains consume observations exactly once.
Directories opened through Open have independent maintenance and queues.
A service without maintenance returns an empty batch.

A drain accepts 1 through 256 events. The queue holds at most 1,024 events, each
at most 16 KiB serialized. Overflow drops the oldest event. Oversized events are
dropped; both count toward the batch's dropped field. A successful drain reports
and resets this count; invalid limits leave the queue untouched. Queue faults
return a static reason without observation content.

OpenHuman decides which observations produce domain events or user notices.
Server identifiers, display names and provider error details are product data
and must never enter module-failure telemetry. Telemetry uses closed safe
reason codes. Polling the queue does not drive or replace module supervision.

Local tests use in-memory stores and loopback fixtures. They prove ordering,
one-time consumption, limits, explicit loss accounting, queue faults and a
reconnection report produced by actual module maintenance. The compiled artifact
verifier also calls the drain member on a fresh registry.

The release workflow publishes the updated contract and manifest together.
Hosts must consume a compatible published artifact with verified digest before
switching supervisor adapters. Server protocol operations and the remaining
contract behavior split are separate migration work.
