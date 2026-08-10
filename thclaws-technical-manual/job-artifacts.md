# Job Artifacts

Bearer-authenticated, session-scoped, hash-frozen file transfer for
external orchestrators. Since v0.88.0. User-facing guide: user-manual
chapter 30; this page is the wire-level reference.

**Problem shape:** a control plane dispatches work to N `--serve`
workers via `POST /agent/run`, and worker B needs worker A's actual
output files. The pre-existing option — `/workspace/sync/*` — is
whole-workspace, relies on the network layer for auth (tunnel /
ForwardAuth / multiuser HMAC), and its manifest→export two-step races
against concurrent edits. Job Artifacts is the per-job, token-authed,
race-free alternative; "job" = the `session_id` `/agent/run` already
returns.

Key sources:

- `crates/core/src/api_v1/artifacts.rs` — snapshot + all three handlers + tests
- `crates/core/src/api_v1/agent.rs` — `collect_files` field, `snapshot_collect_files` hook
- `crates/core/src/api_v1/mod.rs` — routes, `check_bearer_headers` (Tier 1 helper)
- `crates/core/src/server.rs` — `sync_bearer_gate` middleware on the sync group

---

## 1. Declaring outputs — `collect_files`

`AgentRunRequest` gains an optional field:

```json
POST /agent/run
{ "prompt": "…", "collect_files": ["reports/*.pdf", "data/**/*.csv"] }
```

Globs are `globset` syntax, matched against workspace-relative paths.
The snapshot hook (`snapshot_collect_files`) runs at run completion on
**all three** agent_run paths — sync and async share
`run_outcome_with_session`; the SSE path calls it after its session
save. Failures are logged to stderr and never fail the run.

## 2. Snapshot semantics (the atomicity guarantee)

`snapshot_artifacts(workspace, session_id, patterns)`:

1. Walks the workspace (`walkdir`, no symlink follow), **pruning
   `.thclaws/`, `.git/`, `node_modules/`** at the directory level —
   the first also prevents recursing into our own snapshot dir.
2. For each glob match: reads the bytes, sha256s them, **copies** them
   to `<ws>/.thclaws/state/artifacts/<sid>/files/<rel>`.
3. Caps: `MAX_ARTIFACT_FILES = 256`, `MAX_ARTIFACT_TOTAL_BYTES =
   300 MB` (parity with sync/push). Files past a cap go into the
   manifest's `skipped[]` — truncation is visible, never silent.
4. Sorts by path and assigns ids `a1..aN` (walkdir order is
   fs-dependent; sorting makes ids stable across identical runs).
5. Writes `manifest.json` next to `files/`.

Because the GET endpoints serve **the copy**, a live-workspace edit
after the run cannot change what an orchestrator downloads — the
re-hash-after-download dance the old sync flow required is obsolete.
Verified by test `snapshot_freezes_bytes_and_manifest` (tamper the live
file, frozen bytes + sha unchanged) and live E2E.

Storage lives under `.thclaws/state/` — gitignored, publish-stripped,
never packed. No automatic pruning; long-lived workers should clean old
session dirs.

## 3. Read endpoints

```
GET /v1/sessions/{sid}/artifacts            → manifest JSON
GET /v1/sessions/{sid}/artifacts/{aid}      → file bytes
```

- Bearer auth via the standard `/v1` `AuthOk` extractor.
- `{sid}` is validated against the engine id shape
  (`[A-Za-z0-9_-]{1,128}`) before touching the filesystem.
- `{aid}` matches the manifest `id` **or** the exact `path`.
- Optional `?workspace_dir=` — same `validate_workspace_dir` +
  daemon-CWD fallback as `/agent/run`.
- The file response sets `content-disposition` and an `x-sha256`
  header (the manifest hash) so clients can integrity-check inline.

Manifest shape:

```json
{ "session_id": "sess-…", "collected_at": "2026-07-08T…",
  "patterns": ["reports/*.md"],
  "artifacts": [ { "id": "a1", "path": "reports/summary.md",
                   "size": 180, "sha256": "67aeef…" } ],
  "skipped": [],
  "status": "completed" }
```

### Snapshot outcome — telling the four states apart

Collection is non-fatal to the run, so "the run succeeded" says nothing about
whether its outputs were captured. Since v0.111.0 the outcome is readable from
the API rather than only from the daemon's stderr ([#191]). Four states, four
distinct answers:

| State | `GET …/artifacts` | How to recognise it |
|---|---|---|
| Requested, files captured | `200` | `status: "completed"`, `artifacts` non-empty |
| Requested, matched nothing | `200` | `status: "completed"`, `artifacts: []` |
| Requested, collection failed | `200` | `status: "failed"`, `error` explains why |
| Never requested | `404` | error `code: "snapshot_not_requested"` |
| Manifest corrupt on disk | `404` | error `code: "manifest_unreadable"` |

A matched-nothing snapshot is **completed, not failed** — the patterns ran and
found nothing, which is a real answer. Treat `status: "failed"` as "retry or
investigate"; treat an empty `artifacts` array as "the job produced no
outputs".

`status` is absent from manifests written before v0.111.0. Read a missing
`status` as `"completed"`: back then a manifest could only be written on
success, so that is what it means.

`snapshot_not_requested` costs nothing on disk — a run that omits
`collect_files` writes no manifest at all, which is exactly why the 404 is
unambiguous rather than a guess.

[#191]: https://github.com/thClaws/thClaws/issues/191

## 4. Inputs — `POST /v1/inputs`

```json
{ "workspace_dir": "…(optional)…",
  "files": [ { "path": "inputs/brief.txt", "content_base64": "…" } ] }
```

Path jail (`path_allowed`): relative only, no `..`, never `.thclaws/`
or `.git/`, and must sit under an allowed prefix — default **`inputs/`**,
widened via `THCLAWS_INPUTS_PREFIXES="inputs/,data/"` or `"*"`
(anywhere except the always-denied pair). Caps: 100 files,
64 MB decoded (route body limit 96 MB for base64 overhead). Response
echoes `{path, size, sha256}` per file written. Violations: 403
`path_not_allowed`, 413 `limit_exceeded`, 400 `bad_base64`.

## 5. Tier 1 — `THCLAWS_SYNC_REQUIRE_AUTH`

For orchestrators that DO want whole-workspace sync without a tunnel:

```
THCLAWS_SYNC_REQUIRE_AUTH=1   (or "true")
```

adds a `route_layer` (`sync_bearer_gate`) over the six
`/workspace/sync/*` routes enforcing the same constant-time Bearer
policy as `/v1` (`api_v1::check_bearer_headers`). If the flag is set
while `THCLAWS_API_TOKEN` is unset, sync requests get 401 (fail
closed). Flag unset → passthrough, byte-identical to the classic
trusted-network behavior.

## 6. Threat-model notes

- **Least privilege:** an orchestrator token reads a session's
  *declared* outputs — not arbitrary workspace paths. (The token also
  grants `/agent/run`, so this is scoping, not a privilege boundary
  against the token holder; the boundary it does draw is against
  accidental whole-workspace coupling and path-traversal bugs.)
- Session ids and artifact paths never concatenate into the filesystem
  without validation (`safe_session_id`, manifest-membership lookup).
- Inputs cannot write engine state (`.thclaws/`) or VCS metadata
  (`.git/`) under any prefix configuration, including `*`.

## 7. Tests

`api_v1::artifacts::tests` — `snapshot_freezes_bytes_and_manifest`
(tamper-proofing), `snapshot_never_recurses_into_thclaws`,
`inputs_path_jail`, `session_id_shape`. Live E2E (documented in the
feature commit): sync 401/200 under the Tier-1 flag; inputs write +
403 outside prefix; a real run collected `reports/*.md`; post-run
tamper of the live file did not affect the downloaded bytes.
