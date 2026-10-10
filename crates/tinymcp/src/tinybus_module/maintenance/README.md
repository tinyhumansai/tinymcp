# Registry maintenance

Maintenance owns a background boot pass followed by the reconnect supervisor.
Production pacing uses an interval; fixture pacing injects cycles without
wall-clock waits. Dropping maintenance aborts the task.

Each cycle records its ordered observations into Events, a bounded queue local
to that registry object. DrainSupervisorEvents consumes at most 256 observations
per call. Queue overflow and oversized observations increment an explicit loss
counter. The queue cannot stall supervision or the transport when a host stops
draining it. Product notification policy stays with the host.

The serialized ServerRef, ProbeOutcome, SupervisorEvent and TickReport types
live in tinymcp-bus. Task scheduling, probing, reconnect/backoff and queue
operations live here. Details are in the supervisor-observations specification.
