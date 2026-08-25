# Hosted Workflows & Network Transports (`gemel` Phase 8)

Phase 8 ships the network transports (SSH + HTTP) that make Phase 6's
transport-agnostic sync actually distributed, hosted sync servers with
capability-scoped authentication, and the FRF **court runner** — the one,
policy-gated execution path in Gemel.

The architectural boundary from Phase 6 is preserved exactly: a `Transport`
implements six operations (list refs, reachable closure, missing set, fetch
envelopes, push envelopes, update refs). Nothing about a transport is allowed
to change canonical object semantics. Transports move bytes; the object store
verifies identities; the query layer interprets meaning. A transport is never
an oracle and never a write surface for canonical knowledge.

---

## 1. Principles

1. **Native objects remain authoritative.** A transport only negotiates and
   moves verified envelopes. Every byte received is re-hashed before use;
   same-id-different-bytes is fatal (THREAT_MODEL.md §11).
2. **Network transports are session transports.** Both SSH and HTTP speak the
   same bounded line-delimited JSON session protocol (§6), a strict superset of
   the agent-protocol discipline: one request per line, one response per line,
   stable error strings, nothing executed.
3. **Transport encryption is mandatory for non-local remotes.** `https://` is
   rejected by the URL parser (THREAT_MODEL.md §10): TLS is not implemented
   in-crate. Use `ssh://` (SSH supplies mutual auth and encryption) or
   terminate TLS in a proxy and speak `http://` to it on the loopback or a
   private network.
4. **Fail closed.** Missing/wrong credentials → `401`. Non-loopback HTTP binds
   without tokens → startup refusal. Malformed URLs → parse errors. Malformed
   sessions → error responses, never partial application. Read-only servers
   refuse every mutation even with a write token.
5. **Execution is a separate, explicit action.** The court runner (§7) is the
   only code path that runs recorded reproduction commands, and it is governed
   by `config.execution_policy` (default `never_auto_execute`). Ingestion,
   `status`, sync, and the agent protocol never execute anything.

---

## 2. Remote URL Grammar

A remote argument is a configured remote name, a URL, or a local path:

```
ssh://[user@]host[:port]/path/to/repo
http://[token@]host[:port]/path/to/repo      port defaults to 80
/absolute/path/to/repo
relative/path/to/repo
```

- `ssh://` requires a non-root repository path (`ssh://host/repo`).
- `http://` may carry a bearer token in the userinfo position
  (`http://token@host:port/repo`); this is a token shorthand, never a
  password. The token may also come from the configured remote's URL.
- `https://` is **rejected** with a message pointing at `ssh://` or a
  TLS-terminating proxy. There is no silent downgrade.
- Ports must be decimal `u16` when present; a malformed port
  (`ssh://host:abc/repo`, `ssh://host:/repo`, `ssh://host:99999/repo`) is a
  parse error, never silently swallowed. Omitted ports use the scheme default
  (22 for ssh — applied by `ssh(1)` — and 80 for http).
- Bracketed IPv6 literals are not supported in v1.
- Unknown schemes (`ftp://…`) are rejected naming the supported set.
- The derived remote name (the label for `refs/remotes/<name>/*`) includes
  the port when present: `ssh://host/path` → `host/path`,
  `http://127.0.0.1:8033/` → `127.0.0.1:8033`. Two remotes that differ only
  by port never share a tracking namespace.

Resolution order: a configured remote name wins; otherwise the argument is
parsed as a URL or path. Local paths resolve to a native Gemel repository
(`.gemel/meta.json` present) or a Git-only repository (deterministic
projection, GIT_INTEROP.md §6); anything else is an error.

---

## 3. The SSH Transport

`SshTransport` spawns `ssh [-p port] [user@]host gemel serve <path>`:

- argv-safe (no shell concatenation; the remote path must not start with `-`),
- SSH supplies mutual authentication, encryption, and host verification;
- the remote command is the bounded stdio session (§6) — `gemel serve <path>`
  — and the remote side applies its own read-only policy;
- sessions are per-operation synchronous with a monotonically increasing `id`
  echoed in every response;
- dropping the transport reaps the child (killed; a graceful EOF cannot be
  delivered after the session ends, so waiting on it could hang).

A test-only seam (`SshTransport::spawn(program, args, describe)`) points the
same session framing at any program — the integration courts drive the local
`gemel` binary with it, which is exactly what the real `ssh` invocation runs
remotely.

---

## 4. The HTTP Transport and Server

### 4.1 Client

`HttpTransport` POSTs the six session operations to the server:

| op | endpoint | body | response |
|---|---|---|---|
| `list_refs` | `/refs` | `{}` | `{"refs":[[name,gid],…]}` |
| `reachable` | `/reachable` | `{"seeds":[gid,…]}` | `{"ids":[gid,…]}` |
| `missing` | `/missing` | `{"ids":[gid,…]}` | `{"ids":[gid,…]}` |
| `fetch` | `/objects` | `{"ids":[gid,…]}` | raw `gemlpack` bytes |
| `push` | `/push` | raw `gemlpack` bytes | `{"ok":true,"inserted":N}` |
| `update_refs` | `/update-refs` | `{"refs":[[name,gid],…]}` | `{"ok":true,"applied":N}` |

Bearer tokens are sent as `Authorization: Bearer <token>`. Responses are
bounded (read/write timeouts, a hard `Content-Length` cap, `Connection:
close`).

### 4.2 Server (`gemel serve --http`)

```
gemel serve [path] --http 127.0.0.1:8033 [--token "tok read"]… [--read-only]
gemel serve --root /srv/gemel --http 0.0.0.0:8033 --token-file /etc/gemel/tokens
```

- **Tokens**: `--token "<token> <capability>"` where capability is `read` or
  `write` (repeatable; `--token <token>` alone means `write`). A token-file
  argument (`--token /path/to/file`) reads one `token capability` per line,
  `#` comments allowed.
- **Capability scoping**: a `read` token may query and fetch; every mutation
  (`/push`, `/update-refs`) is refused. `--read-only` refuses mutations
  globally, even for write tokens.
- **Loopback fail-closed**: binding a non-loopback address with no tokens
  configured is a startup error. On loopback, an empty token set means open
  (the machine is trusted); tokens, when configured, are always enforced.
- **Multi-repository hosting**: `--root <dir>` serves any Gemel repository
  under that directory by URL path (`/repo1/refs`, `/repo1/push`, …).
  Repository names are single path segments; `/..`, `/../etc`, and nested
  paths are rejected (`404`). Without `--root`, only the server's own
  repository (path `/`) is served. The endpoint suffix is not part of the
  repository path.
- **Request hygiene**: HTTP/1.0/1.1 POST only, `Content-Length` required,
  headers bounded (64), body bounded by `MAX_PACK_BYTES` (4 GiB), 120 s read
  timeout, unknown endpoints `404`, method-not-POST `405`.
- The server needs no local repository of its own when `--root` is given; it
  opens each repository per request.

### 4.3 Stdio session (`gemel serve [path]`)

Without `--http`, `gemel serve [path]` runs the same session protocol over
stdin/stdout — the SSH transport backend. `--read-only` is honored.

---

## 5. Remote Resolution and CLI Surface

```
gemel serve [path] [--http addr] [--root dir] [--read-only] [--token …]
gemel court <evidence-id> [--allow] [--timeout secs]
gemel fetch|push|pull <remote>     # name, ssh://, http://, or local path
gemel remote add <name> <url-or-path> [--init]   # URLs validated strictly
```

`fetch`/`push`/`pull` accept a configured name, a URL, or a path directly.
`remote add` validates the URL/path before recording it; `--init` applies to
local paths only. Git-only paths still receive the deterministic
export/import projections (GIT_INTEROP.md §6).

---

## 6. The Session Protocol (bounds and errors)

Request: `{"id":N,"op":"<op>","params":{…}}`; response:
`{"id":N,"ok":true,…}` or `{"id":N,"ok":false,"error":{code,message}}`.
`fetch`/`push` carry a `pack_len` header followed by exactly that many raw
`gemlpack` bytes (never newline-escaped).

Limits (enforced on both sides):

- `MAX_LINE` — 64 KiB per request/response line;
- `MAX_PACK_BYTES` — 4 GiB per pack;
- `MAX_IDS` — 10 000 000 ids per request;
- push packs are decoded, schema-validated, and inserted by content identity;
  advertised id ≠ stored id is an error, and same-id-different-bytes is fatal.

`update_refs` is validated before publication (public ref names only, full
closure present) and applied as one journaled transaction.

---

## 7. The FRF Court Runner (brief §38)

`gemel court <evidence-id>` re-executes the reproduction command recorded in
an evidence object (field `0x05`) and publishes the fresh observation as a
**new** evidence object. The original evidence is never rewritten; the court
never fabricates.

- **Policy** (`config.execution_policy`, OBJECT_MODEL.md §6.21):
  - `never_auto_execute` (default) — the court refuses unconditionally
    (`POLICY_DENIED`), even with `--allow`;
  - `policy_gated` — requires the explicit `--allow` flag;
  - `allowlist` — permits only commands whose first token matches
    `.gemel/court.allowlist` (one pattern per line, `#` comments; a trailing
    `*` is a prefix match on the first token). `--allow` does not bypass the
    allowlist.
- **Execution**: `sh -c <command>` in the repository root, stdin closed, a
  timeout (CLI default 300 s, `--timeout` to override, clamped 1–3600 s).
- **Published observation**: producer `court-runner`; kind/subject carried
  from the source evidence; result `{outcome: pass|fail|inconclusive,
  detail, exit_code}`; reproduction record `{replayable: true, inputs_present:
  true, policy_required: false}`; `evaluated_state` = the current head state.
  A timeout records `inconclusive` with no exit code.
- **The only execution path.** Ingestion (exchange), `status`, sync, and the
  agent protocol never execute recorded commands; a court's default denial is
  the guarantee. The allowlist file lives outside the object store (it is
  operator configuration, not canonical knowledge).

---

## 8. Security Properties

| Property | Guarantee |
|---|---|
| Integrity | every envelope re-verified; identity mismatch aborts; id↔bytes conflict fatal |
| Authenticity | separate from integrity: SSH provides mutual auth; HTTP uses bearer tokens; Git hosting auth is not Gemel trust (EXCHANGE.md §14) |
| Authority | capability-scoped grants (`read`/`write`), read-only servers, per-op enforcement |
| Confidentiality | transport encryption mandatory for non-local remotes; `https://` rejected, not downgraded |
| Traversal | single-segment repo names under `--root`; `..`/nested paths `404` |
| Boundedness | line/pack/id/header/body limits on both ends; no unbounded reads |
| No execution | courts are policy-gated; everything else is data-only |
| Fail-closed | malformed URLs, sessions, and requests are errors; never partial activation |

### Explicit non-properties

- The HTTP server is not a general-purpose web server (no GET, no TLS, no
  streaming, no chunked transfer).
- Bearer tokens are secrets at rest for the operator to protect; token files
  are not canonical Gemel objects.
- The session protocol is not the Phase 6 `gemlpack` format; packs are
  payloads inside the session framing.
- There is no push-to-GitHub-style hosted service in this phase; `--root`
  serving is the hosted-workflow primitive.

---

## 9. Compatibility

- The session protocol and URL grammar are additive; new ops are rejected by
  old servers (`unknown op`), never misinterpreted.
- `RemoteUrl`/`RemoteTarget` are library API; the transport trait is stable
  (`&mut self` methods, since sessions own a read position).
- A newer client talking to an older server fails with stable error strings;
  an older client talking to a newer server ignores unknown response fields.

---

## 10. Courts (`tests/phase8_tests.rs`)

1. `remote_url_grammar_ssh_http_local` — valid ssh/http/local forms, scheme
   defaults, token userinfo.
2. `remote_url_fails_closed` — `https://` rejection, bad/missing ports, empty
   hosts, unknown schemes.
3. `http_roundtrip_identical_ids_and_idempotence` — push → server, re-push
   transfers 0, pull → fresh clone with identical canonical identities and
   semantic knowledge.
4. `http_auth_capabilities_fail_closed` — no token `401`, wrong token `401`,
   read-capability fetch-ok/push-refused, write token succeeds.
5. `http_read_only_server_refuses_mutation` — read-only server refuses push
   even with a write token.
6. `http_non_loopback_without_tokens_fails_closed` — startup refusal.
7. `http_multi_repo_root_serving_and_traversal_rejection` — `--root` hosting
   of independent repositories, unknown/traversal paths `404`.
8. `ssh_transport_roundtrip_via_local_binary` — the bounded stdio session over
   the local `gemel serve` binary (the exact remote command `ssh` runs).
9. `ssh_read_only_refuses_mutation` — `--read-only` session policy.
10. `cli_push_pull_http_url_end_to_end` — `gemel push/pull
    http://token@127.0.0.1:PORT/` with URL-derived tracking refs.
11. `court_default_policy_denies_execution_and_status_never_executes` — the
    default denies even `--allow`; `status` adds no objects and runs nothing.
12. `court_policy_gated_allow_publishes_fresh_evidence` — pass/fail
    observations, `court-runner` producer, `evaluated_state` = head,
    replayable reproduction record, original evidence untouched.
13. `court_allowlist_permits_matching_commands_only` — literal and prefix
    patterns; non-listed commands denied even with `--allow`.
14. `court_timeout_is_inconclusive` — timeout records `inconclusive`, no exit
    code.
15. `court_cli_end_to_end` — `gemel court <id> --allow --json` and the
    published evidence queried through `gemel show`.
16. `policy_gaps_reported_and_readiness_not_ready` — required-verification
    gaps via `gemel policy --json` and `NOT_READY` status.
