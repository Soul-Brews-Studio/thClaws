/**
 * Tests for the per-agent FIFO mutex.
 * Run: `node --test --import tsx src/server/agent-mutex.test.ts`
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  acquireAgentLock,
  _agentLockMapSizeForTests,
  _resetAgentLocksForTests,
} from "./agent-mutex.js";

test("different agents acquire concurrently", async () => {
  _resetAgentLocksForTests();
  const r1 = await acquireAgentLock("agent-a");
  const r2 = await acquireAgentLock("agent-b");
  // Both acquired without blocking each other.
  r1();
  r2();
});

test("same agent serializes - second waits for first release", async () => {
  _resetAgentLocksForTests();
  const order: string[] = [];
  const r1 = await acquireAgentLock("agent-x");
  order.push("first-acquired");

  const secondAcquire = acquireAgentLock("agent-x").then((release) => {
    order.push("second-acquired");
    release();
  });

  // The second acquire should not have resolved yet.
  await new Promise((r) => setTimeout(r, 10));
  assert.deepEqual(order, ["first-acquired"]);

  // Release the first; the second should now resolve.
  r1();
  await secondAcquire;
  assert.deepEqual(order, ["first-acquired", "second-acquired"]);
});

test("FIFO order preserved across three waiters", async () => {
  _resetAgentLocksForTests();
  const order: number[] = [];
  const r1 = await acquireAgentLock("agent-y");
  const a2 = acquireAgentLock("agent-y").then((release) => {
    order.push(2);
    release();
  });
  const a3 = acquireAgentLock("agent-y").then((release) => {
    order.push(3);
    release();
  });
  order.push(1);
  r1();
  await Promise.all([a2, a3]);
  assert.deepEqual(order, [1, 2, 3]);
});

test("release is idempotent (defensive)", async () => {
  _resetAgentLocksForTests();
  const r = await acquireAgentLock("agent-z");
  r();
  // Calling release a second time shouldn't blow up.
  r();
  // The next acquire should still work.
  const r2 = await acquireAgentLock("agent-z");
  r2();
});

test("map empties after every waiter releases (no memory leak)", async () => {
  _resetAgentLocksForTests();
  assert.equal(_agentLockMapSizeForTests(), 0);

  // Acquire across several unique agentIds.
  const releases: Array<() => void> = [];
  for (const id of ["a", "b", "c"]) {
    releases.push(await acquireAgentLock(id));
  }
  assert.equal(_agentLockMapSizeForTests(), 3, "one entry per active agent");

  // Release in arbitrary order.
  releases[1]();
  releases[2]();
  releases[0]();

  // Give microtasks a tick to drain.
  await new Promise((r) => setTimeout(r, 5));

  assert.equal(
    _agentLockMapSizeForTests(),
    0,
    "Map should empty once all waiters release — guards against a previous bug where queues.set stored a .then() wrapper that never matched the identity check on cleanup",
  );
});

test("map empties when chain length > 1 fully drains", async () => {
  _resetAgentLocksForTests();
  const r1 = await acquireAgentLock("chain");
  const a2 = acquireAgentLock("chain");
  const a3 = acquireAgentLock("chain");
  assert.equal(_agentLockMapSizeForTests(), 1, "single entry while chain queues");

  r1();
  (await a2)();
  (await a3)();
  await new Promise((r) => setTimeout(r, 5));
  assert.equal(_agentLockMapSizeForTests(), 0, "entry gone after deepest waiter releases");
});
