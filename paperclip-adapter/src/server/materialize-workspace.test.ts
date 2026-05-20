/**
 * Unit tests for the workspace materializer.
 *
 * Run with `node --test --import tsx src/server/materialize-workspace.test.ts`
 * from the paperclip-adapter root (tsx hook lets node load TS directly).
 * Kept in-tree rather than under a dist build so the source path is
 * the unit of execution.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, readdir, mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";

import { materializeAgentWorkspace } from "./materialize-workspace.js";

async function freshWorkspace(): Promise<string> {
  return mkdtemp(join(tmpdir(), "thclaws-mw-"));
}

test("writes desired skills to .thclaws/skills/<key>/SKILL.md", async () => {
  const ws = await freshWorkspace();
  const result = await materializeAgentWorkspace({
    workspaceDir: ws,
    skills: [
      { key: "pdf", content: "---\nname: pdf\n---\nBody A" },
      { key: "deploy", content: "---\nname: deploy\n---\nBody B" },
    ],
  });
  assert.deepEqual(result.skillsWritten.sort(), ["deploy", "pdf"]);
  const pdf = await readFile(join(ws, ".thclaws", "skills", "pdf", "SKILL.md"), "utf-8");
  assert.match(pdf, /Body A/);
  const deploy = await readFile(join(ws, ".thclaws", "skills", "deploy", "SKILL.md"), "utf-8");
  assert.match(deploy, /Body B/);
});

test("prunes stale skill directories on re-materialize", async () => {
  const ws = await freshWorkspace();
  await materializeAgentWorkspace({
    workspaceDir: ws,
    skills: [
      { key: "old", content: "---\nname: old\n---\nold body" },
      { key: "keep", content: "---\nname: keep\n---\nkeep body" },
    ],
  });
  const beforeNames = (await readdir(join(ws, ".thclaws", "skills"))).sort();
  assert.deepEqual(beforeNames, ["keep", "old"]);

  const result = await materializeAgentWorkspace({
    workspaceDir: ws,
    skills: [{ key: "keep", content: "---\nname: keep\n---\nkeep body" }],
  });
  assert.deepEqual(result.skillsRemoved, ["old"]);
  const afterNames = await readdir(join(ws, ".thclaws", "skills"));
  assert.deepEqual(afterNames, ["keep"]);
});

test("writes AGENT.md when agentInstructions is non-empty", async () => {
  const ws = await freshWorkspace();
  const result = await materializeAgentWorkspace({
    workspaceDir: ws,
    agentInstructions: "You are a careful release manager.\n",
  });
  assert.equal(result.agentMdWritten, true);
  const agentMd = await readFile(join(ws, "AGENT.md"), "utf-8");
  assert.match(agentMd, /careful release manager/);
});

test("skips AGENT.md when agentInstructions is empty/whitespace", async () => {
  const ws = await freshWorkspace();
  const result = await materializeAgentWorkspace({
    workspaceDir: ws,
    agentInstructions: "   \n  ",
  });
  assert.equal(result.agentMdWritten, false);
  await assert.rejects(readFile(join(ws, "AGENT.md"), "utf-8"));
});

test("writes .thclaws/mcp.json when mcpServers supplied", async () => {
  const ws = await freshWorkspace();
  const result = await materializeAgentWorkspace({
    workspaceDir: ws,
    mcpServers: [{ name: "filesystem", command: "mcp-server-filesystem" }],
  });
  assert.equal(result.mcpWritten, true);
  const mcpRaw = await readFile(join(ws, ".thclaws", "mcp.json"), "utf-8");
  const mcp = JSON.parse(mcpRaw);
  assert.equal(mcp.servers[0].name, "filesystem");
});

test("does not touch mcp.json when no servers supplied", async () => {
  const ws = await freshWorkspace();
  // Pre-populate mcp.json to simulate a user-managed config.
  await mkdir(join(ws, ".thclaws"), { recursive: true });
  await writeFile(join(ws, ".thclaws", "mcp.json"), '{"servers":["preserved"]}\n');
  const result = await materializeAgentWorkspace({
    workspaceDir: ws,
    skills: [],
  });
  assert.equal(result.mcpWritten, false);
  const after = await readFile(join(ws, ".thclaws", "mcp.json"), "utf-8");
  assert.match(after, /preserved/);
});

test("skips skills with missing key or content", async () => {
  const ws = await freshWorkspace();
  const result = await materializeAgentWorkspace({
    workspaceDir: ws,
    skills: [
      { key: "", content: "x" },
      { key: "y", content: "" },
      { key: "ok", content: "body" },
    ],
  });
  assert.deepEqual(result.skillsWritten, ["ok"]);
});
