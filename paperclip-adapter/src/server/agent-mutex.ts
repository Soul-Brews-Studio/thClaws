/**
 * Per-agent in-process mutex.
 *
 * dev-plan/25 Phase D: even though thcompany's heartbeat already
 * serializes runs of the same agent via withAgentStartLock +
 * maxConcurrentRuns, the daemon process can host multiple adapter
 * instances (different agents) concurrently. When agent A and agent B
 * both materialize their own skill sets into their own workspace dirs
 * we're fine — different paths. But if the same agent's
 * maxConcurrentRuns goes >1, the materialize → POST window must
 * serialize so the second run doesn't write skills before the first
 * has issued the request.
 *
 * Scope is the adapter package's lifetime (in-process Map). Multiple
 * thcompany pods don't share this — that's heartbeat's
 * `FOR UPDATE SKIP LOCKED` job.
 */

type Resolver = () => void;

const queues: Map<string, Promise<void>> = new Map();

/**
 * Acquire the per-agent lock. Returns a release function. Callers
 * MUST call release in a finally block — leaking the lock wedges
 * every future run for the same agent.
 *
 * The Map stores `next` directly (not a `.then()` chain wrapper), so
 * the release-time identity check `queues.get(agentId) === next` can
 * actually match — otherwise the Map would leak one entry per unique
 * agentId forever.
 */
export async function acquireAgentLock(agentId: string): Promise<Resolver> {
  // Whatever's currently in the Map is the tail of the FIFO chain —
  // either a still-pending `next` we await, or undefined (no one
  // holding the lock). Replace it with ours so the next acquirer
  // chains onto us.
  const previous = queues.get(agentId) ?? Promise.resolve();
  let release!: Resolver;
  const next = new Promise<void>((resolve) => {
    release = () => {
      // Only delete if we're still the tail — if a later acquirer
      // already replaced us, leave them in place.
      if (queues.get(agentId) === next) queues.delete(agentId);
      resolve();
    };
  });
  queues.set(agentId, next);
  // Wait for our turn (resolves immediately if the map was empty).
  await previous;
  return release;
}

/** Test-only: drop all queued promises. */
export function _resetAgentLocksForTests(): void {
  queues.clear();
}

/** Test-only: how many agentIds currently hold queued promises. */
export function _agentLockMapSizeForTests(): number {
  return queues.size;
}
