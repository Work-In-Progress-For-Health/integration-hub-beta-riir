# 05 — Health check

A TCP listener that accepts a connection and immediately closes it. That is
the whole contract. Implemented by `crates/ihb-health`.

Reference implementation:
`Integration-Hub-Beta/shared_libs/health_check_lib/health_check_server.py`.

---

## §1 Purpose

Azure Container Apps probes a container by opening a TCP connection. A
successful connect means the process is alive and its runtime is scheduling
work. Nothing is written and nothing is read: an HTTP probe would require
every background worker to carry an HTTP stack for no gain.

§1.1 The listener MUST accept a connection and close it without writing.

§1.2 The listener MUST NOT read from the connection. A probe that never
writes must not be kept waiting.

---

## §2 Configuration

| Variable | Default |
| --- | --- |
| `HEALTH_CHECK_HOST` | `127.0.0.1` |
| `HEALTH_CHECK_PORT` | `9000` |

§2.1 An absent value MUST resolve to the default rather than disabling the
listener — the Python passes `None` and the constructor substitutes the
default (`host if host is not None else "127.0.0.1"`).

§2.2 In a container the host is `0.0.0.0`; `127.0.0.1` is the default only
because it is the safe one for a laptop.

§2.3 `SO_REUSEADDR` MUST be set, so a restart does not fail on a socket in
`TIME_WAIT`.

---

## §3 Lifecycle

§3.1 `start` binds, listens with a backlog of at least 1, and begins
accepting on a background task. It MUST return as soon as the socket is
bound, so a caller that starts the health check before its main loop
reports healthy immediately.

§3.2 A bind failure MUST be reported to the caller, not swallowed. A
container that cannot bind its health check will be killed by the platform,
and it should say why.

§3.3 `stop` closes the listener and waits for the accept task to finish,
with a timeout of 1 second. A close failure is logged at warn.

§3.4 An accept error MUST end the accept loop rather than spinning.

§3.5 The type MUST implement a scope-based shutdown (a guard, or `Drop`) so
a service cannot forget to stop it.

---

## §4 Tests

§4.1 A started listener accepts a TCP connection and the peer sees EOF.

§4.2 Nothing is written to the connection.

§4.3 `stop` makes subsequent connections fail.

§4.4 Binding an already-bound port reports an error rather than panicking.

§4.5 Start, stop and start again on the same port succeeds (§2.3).
