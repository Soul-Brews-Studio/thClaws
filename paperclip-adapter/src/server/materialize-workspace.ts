/**
 * Materialize a per-agent workspace directory before calling
 * thClaws's `/agent/run` endpoint.
 *
 * Each agent gets its own `workspace_dir` (provisioned by the
 * orchestrator). thcompany-managed skills + agent instructions land
 * on disk under that path so thClaws — which reads skills via
 * filesystem scan — picks them up at request time. This is the
 * contract that closes dev-plan/25's "thClaws as agent, not LLM"
 * loop: instead of trying to inject skills over the wire, we treat
 * thClaws like claude-code and use its filesystem-shaped surface.
 *
 * Layout written:
 *   <workspaceDir>/AGENT.md                              instructions
 *   <workspaceDir>/.thclaws/skills/<key>/SKILL.md        each skill
 *   <workspaceDir>/.thclaws/mcp.json   (when ctx supplies)  MCP config
 *
 * Stale skill directories (a skill that thcompany removed from the
 * agent's desired list since the last run) are deleted so the agent
 * doesn't see ghost skills it isn't supposed to use anymore.
 */

import { mkdir, readdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { AdapterExecutionContext } from "@paperclipai/adapter-utils";

/** One thcompany-managed skill, as the orchestrator hands it to us. */
export interface MaterializedSkill {
  /** Stable identifier — becomes the directory name under `.thclaws/skills/`. */
  key: string;
  /** Full SKILL.md content including YAML frontmatter. */
  content: string;
}

/** One MCP server config, JSON-serialized into `.thclaws/mcp.json`. */
export interface MaterializedMcpServer {
  name: string;
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  transport?: string;
  [key: string]: unknown;
}

export interface MaterializeInput {
  workspaceDir: string;
  /** Agent instructions joined into AGENT.md. Empty string ⇒ skip the file. */
  agentInstructions?: string;
  /** thcompany-managed skills to expose to the agent for this run. */
  skills?: MaterializedSkill[];
  /** MCP servers to expose; written verbatim to `.thclaws/mcp.json`. */
  mcpServers?: MaterializedMcpServer[];
}

export interface MaterializeResult {
  workspaceDir: string;
  agentMdWritten: boolean;
  skillsWritten: string[];
  skillsRemoved: string[];
  mcpWritten: boolean;
}

/**
 * Write the materialized layout. Idempotent — running twice with the
 * same input is a no-op aside from re-writing files (cheap).
 *
 * Skill cleanup contract: any directory under `.thclaws/skills/` that
 * doesn't match a desired key is removed. This means the workspace is
 * canonically owned by the materializer — files placed there by other
 * tools may be deleted on the next run. The orchestrator is expected
 * to keep workspace_dir as a managed-by-us location.
 */
export async function materializeAgentWorkspace(
  input: MaterializeInput,
): Promise<MaterializeResult> {
  const { workspaceDir } = input;
  await mkdir(workspaceDir, { recursive: true });

  const result: MaterializeResult = {
    workspaceDir,
    agentMdWritten: false,
    skillsWritten: [],
    skillsRemoved: [],
    mcpWritten: false,
  };

  // AGENT.md — joined instructions. Empty ⇒ no file (don't ship an
  // empty file that the agent's project-context scanner would then
  // dutifully load).
  const trimmed = (input.agentInstructions ?? "").trim();
  if (trimmed.length > 0) {
    await writeFile(join(workspaceDir, "AGENT.md"), trimmed + "\n", "utf-8");
    result.agentMdWritten = true;
  }

  // Skills — write each desired skill, then prune anything not in the
  // desired set.
  const skillsDir = join(workspaceDir, ".thclaws", "skills");
  await mkdir(skillsDir, { recursive: true });
  const desiredKeys = new Set<string>();
  for (const skill of input.skills ?? []) {
    if (!skill.key || !skill.content) continue;
    desiredKeys.add(skill.key);
    const skillDir = join(skillsDir, skill.key);
    await mkdir(skillDir, { recursive: true });
    await writeFile(join(skillDir, "SKILL.md"), skill.content, "utf-8");
    result.skillsWritten.push(skill.key);
  }
  try {
    const existing = await readdir(skillsDir);
    for (const name of existing) {
      if (desiredKeys.has(name)) continue;
      // Defensive: ignore non-directory entries (a stray file someone
      // dropped here). rm with recursive:true handles both, but
      // skipping known config files keeps the cleanup focused on
      // thcompany-managed entries.
      if (name === "mcp.json" || name === "policies.toml") continue;
      await rm(join(skillsDir, name), { recursive: true, force: true });
      result.skillsRemoved.push(name);
    }
  } catch {
    // skillsDir didn't exist before mkdir — already created above, so
    // the readdir failure here would be a fs race. Safe to ignore.
  }

  // MCP — single JSON file. If the orchestrator doesn't supply mcp
  // servers we leave the file alone (don't blow away a manually-managed
  // .thclaws/mcp.json the user might have put there).
  if (input.mcpServers && input.mcpServers.length > 0) {
    const mcpPath = join(workspaceDir, ".thclaws", "mcp.json");
    await mkdir(join(workspaceDir, ".thclaws"), { recursive: true });
    await writeFile(
      mcpPath,
      JSON.stringify({ servers: input.mcpServers }, null, 2) + "\n",
      "utf-8",
    );
    result.mcpWritten = true;
  }

  return result;
}

/**
 * Extract the materialization input from a thcompany
 * `AdapterExecutionContext`. Conventions:
 *
 *   - `ctx.config.workspaceDir`           required absolute path
 *   - `ctx.context.agentInstructions`     joined markdown (heartbeat
 *                                         pre-joins memory + system
 *                                         message + instructions file)
 *   - `ctx.context.skills`                array of `{key, content}` —
 *                                         populated by
 *                                         companySkillService.materializeFor()
 *   - `ctx.context.mcpServers`            array of MCP server configs
 *
 * Returns null when `workspaceDir` is missing; the caller surfaces this
 * as a clean error rather than guessing a path.
 */
export function extractMaterializeInput(
  ctx: AdapterExecutionContext,
): MaterializeInput | null {
  const config = (ctx.config ?? {}) as Record<string, unknown>;
  const workspaceDir = typeof config.workspaceDir === "string" && config.workspaceDir.trim().length > 0
    ? config.workspaceDir.trim()
    : null;
  if (!workspaceDir) return null;

  const context = (ctx.context ?? {}) as Record<string, unknown>;
  const agentInstructions = typeof context.agentInstructions === "string"
    ? context.agentInstructions
    : "";

  const skills: MaterializedSkill[] = [];
  if (Array.isArray(context.skills)) {
    for (const entry of context.skills) {
      if (!entry || typeof entry !== "object") continue;
      const obj = entry as Record<string, unknown>;
      const key = typeof obj.key === "string" ? obj.key : null;
      const content = typeof obj.content === "string" ? obj.content : null;
      if (key && content) skills.push({ key, content });
    }
  }

  const mcpServers: MaterializedMcpServer[] = [];
  if (Array.isArray(context.mcpServers)) {
    for (const entry of context.mcpServers) {
      if (entry && typeof entry === "object" && !Array.isArray(entry)) {
        const obj = entry as MaterializedMcpServer;
        if (typeof obj.name === "string" && obj.name.length > 0) {
          mcpServers.push(obj);
        }
      }
    }
  }

  return { workspaceDir, agentInstructions, skills, mcpServers };
}
