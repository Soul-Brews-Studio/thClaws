/**
 * Lazy spawn + lifecycle for a per-process `thclaws --serve` subprocess.
 *
 * Shared singleton: the first execute() in a Paperclip process spawns
 * the daemon, captures its port (assigned by the OS via --port 0), polls
 * /healthz until ready, then returns the {baseUrl, token} for every
 * subsequent call to reuse. The daemon is torn down on process exit.
 *
 * Why a singleton per-process: thclaws --serve owns a single working
 * directory + keychain + skills state. Running N daemons per agent
 * would duplicate that state for no UX benefit. Multi-agent
 * concurrency rides on the OpenAI API's request-level concurrency.
 *
 * Per-agent isolation (separate /workspace per agent, separate keychain)
 * is the `thclaws_pod` adapter's job (dev-plan/20) — `thclaws_local`
 * inherits the shared-subprocess profile that existed in the v1 stub.
 */

import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createServer } from "node:net";

export interface LocalEndpoint {
  baseUrl: string;
  bearerToken: string;
}

interface PendingSpawn {
  promise: Promise<LocalEndpoint>;
}

let cached: LocalEndpoint | null = null;
let pending: PendingSpawn | null = null;
let child: ChildProcess | null = null;
let cleanupRegistered = false;

const READY_TIMEOUT_MS = 30_000;
const READY_POLL_INTERVAL_MS = 200;

export interface SpawnOptions {
  /** Override the thclaws binary path. Default: "thclaws" (rely on $PATH). */
  command?: string;
  /** Override the workspace cwd for the daemon. Default: process.cwd(). */
  cwd?: string;
  /**
   * Extra env vars to inject into the spawned daemon (provider API keys,
   * etc.). Merged on top of `process.env`. ONLY APPLIED ON FIRST SPAWN —
   * subsequent calls reuse the cached daemon and its locked-in env. To
   * rotate keys, restart the parent process.
   */
  env?: Record<string, string>;
}

/**
 * Get the local thClaws endpoint, spawning the daemon if not already up.
 * Concurrent callers share the same in-flight spawn promise.
 */
export async function getLocalThclawsEndpoint(opts: SpawnOptions = {}): Promise<LocalEndpoint> {
  if (cached) return cached;
  if (pending) return pending.promise;

  const promise = spawnDaemon(opts);
  pending = { promise };
  try {
    const endpoint = await promise;
    cached = endpoint;
    return endpoint;
  } finally {
    pending = null;
  }
}

async function spawnDaemon(opts: SpawnOptions): Promise<LocalEndpoint> {
  const command = opts.command ?? "thclaws";
  const cwd = opts.cwd ?? process.cwd();
  const bearerToken = `thc_local_${randomBytes(16).toString("base64url")}`;

  // We pick a free port ourselves and pass it explicitly. Originally
  // tried `--port 0` (let OS assign), but thClaws's banner prints
  // `config.bind` verbatim — with --port 0 the banner literally reads
  // ":0" because the rebound listener's actual port isn't surfaced.
  // The tiny TOCTOU window between closing our probe socket and
  // thClaws binding is acceptable for v1 (port reuse on loopback
  // within ~1ms is rare).
  const port = await pickFreePort();

  // Env layering: process.env (tenant pod base) → caller-supplied
  // overrides (thcompany-injected provider secrets, etc.) → our
  // bearer token (always wins so /v1/* auth is consistent).
  const childEnv: Record<string, string> = {
    ...(process.env as Record<string, string>),
    ...(opts.env ?? {}),
    THCLAWS_API_TOKEN: bearerToken,
  };

  const proc = spawn(
    command,
    ["--serve", "--bind", "127.0.0.1", "--port", String(port)],
    {
      cwd,
      env: childEnv,
      stdio: ["ignore", "pipe", "pipe"],
    },
  );

  child = proc;
  registerCleanupOnce();

  return new Promise<LocalEndpoint>((resolve, reject) => {
    let resolved = false;
    let banner = "";
    const timeout = setTimeout(() => {
      if (resolved) return;
      resolved = true;
      proc.kill("SIGTERM");
      reject(
        new Error(
          `thclaws --serve did not announce a listening port within ${READY_TIMEOUT_MS / 1000}s. ` +
            `stdout so far:\n${banner.slice(0, 1000)}`,
        ),
      );
    }, READY_TIMEOUT_MS);

    const onData = (chunk: Buffer | string) => {
      banner += chunk.toString();
      // Banner shape: "[serve] thClaws listening on http://127.0.0.1:54321"
      const m = banner.match(/listening on (https?:\/\/127\.0\.0\.1:\d+)/);
      if (m && !resolved) {
        const baseUrl = `${m[1]}/v1`;
        resolved = true;
        clearTimeout(timeout);
        // Confirm /healthz responds before returning — covers the gap
        // between the listener binding and the router being live.
        confirmReady(baseUrl)
          .then(() => resolve({ baseUrl, bearerToken }))
          .catch((e) => reject(e));
      }
    };
    proc.stdout?.on("data", onData);
    proc.stderr?.on("data", onData);
    proc.on("exit", (code) => {
      if (resolved) return;
      resolved = true;
      clearTimeout(timeout);
      reject(
        new Error(
          `thclaws --serve exited (code=${code}) before announcing a port. ` +
            `Output:\n${banner.slice(0, 1000)}`,
        ),
      );
    });
    proc.on("error", (e) => {
      if (resolved) return;
      resolved = true;
      clearTimeout(timeout);
      reject(new Error(`failed to spawn ${command}: ${(e as Error).message}`));
    });
  });
}

async function confirmReady(baseUrl: string): Promise<void> {
  const healthUrl = baseUrl.replace(/\/v1$/, "/healthz");
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(healthUrl);
      if (r.ok) return;
    } catch {
      /* keep polling */
    }
    await new Promise((res) => setTimeout(res, READY_POLL_INTERVAL_MS));
  }
  throw new Error(`thclaws --serve /healthz not ready after spawn at ${healthUrl}`);
}

function registerCleanupOnce(): void {
  if (cleanupRegistered) return;
  cleanupRegistered = true;
  const teardown = () => {
    if (child && !child.killed) {
      child.kill("SIGTERM");
    }
  };
  process.on("exit", teardown);
  process.on("SIGINT", () => {
    teardown();
    process.exit(130);
  });
  process.on("SIGTERM", () => {
    teardown();
    process.exit(143);
  });
}

/**
 * Ask the OS for a free TCP port (0) on loopback, then close.
 * Returns the port number. Small TOCTOU window before the daemon
 * binds; acceptable for the lazy-init use case.
 */
async function pickFreePort(): Promise<number> {
  return new Promise<number>((resolve, reject) => {
    const srv = createServer();
    srv.unref();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      if (!addr || typeof addr === "string") {
        srv.close();
        reject(new Error("could not derive port from server address"));
        return;
      }
      const p = addr.port;
      srv.close(() => resolve(p));
    });
  });
}

/** For tests: clear cached endpoint so the next call re-spawns. */
export function resetForTests(): void {
  if (child && !child.killed) child.kill("SIGTERM");
  cached = null;
  pending = null;
  child = null;
}
