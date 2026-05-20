/**
 * Skills surface for thclaws_local.
 *
 * dev-plan/25 Phase B: skills are now materialized into
 * `<workspaceDir>/.thclaws/skills/<key>/SKILL.md` by
 * `materialize-workspace.ts` before each agent run. The thClaws daemon
 * picks them up via filesystem scan at request time — same contract
 * claude-local uses with `.claude/skills/`.
 *
 * What this module reports back to thcompany's Skills tab:
 *   - desired skills (the agent's configured list)
 *   - each entry's state — `configured` (will run) /
 *     `available` (orchestrator has it but agent doesn't want it) /
 *     `missing` (agent wants it but orchestrator can't supply) /
 *     `external` (something is in the workspace's skill dir we didn't
 *     put there, e.g. user-added)
 *
 * The actual writes happen in `execute.ts → materializeAgentWorkspace`
 * — this file is the read-side: snapshot what's there + diff against
 * what the agent wants. Mirrors `claude-local/src/server/skills.ts`.
 */

import { readdir, stat } from "node:fs/promises";
import { join } from "node:path";
import type {
  AdapterSkillContext,
  AdapterSkillEntry,
  AdapterSkillSnapshot,
} from "@paperclipai/adapter-utils";

const ADAPTER_TYPE = "thclaws_local";

interface AvailableSkill {
  key: string;
  runtimeName: string | null;
  required?: boolean;
  requiredReason?: string | null;
  source?: string;
}

interface ConfigShape {
  workspaceDir?: unknown;
  /** Agent-selected skill names. */
  desiredSkills?: unknown;
  /**
   * Orchestrator-supplied catalogue of skills the agent COULD use.
   * Each entry exposes the same fields as the materialized form but
   * we only need `key` + optional metadata here — the content has
   * already been written to disk by the time listSkills is called.
   */
  availableSkills?: unknown;
}

function asConfig(ctx: AdapterSkillContext): ConfigShape {
  const config = (ctx.config ?? {}) as Record<string, unknown>;
  return config as ConfigShape;
}

function readStringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  const out: string[] = [];
  for (const v of value) {
    if (typeof v === "string" && v.trim().length > 0) out.push(v.trim());
  }
  return out;
}

function readAvailableSkills(value: unknown): AvailableSkill[] {
  if (!Array.isArray(value)) return [];
  const out: AvailableSkill[] = [];
  for (const entry of value) {
    if (!entry || typeof entry !== "object") continue;
    const obj = entry as Record<string, unknown>;
    const key = typeof obj.key === "string" ? obj.key.trim() : "";
    if (!key) continue;
    out.push({
      key,
      runtimeName: typeof obj.runtimeName === "string" ? obj.runtimeName : null,
      required: typeof obj.required === "boolean" ? obj.required : undefined,
      requiredReason:
        typeof obj.requiredReason === "string" ? obj.requiredReason : null,
      source: typeof obj.source === "string" ? obj.source : undefined,
    });
  }
  return out;
}

async function readInstalledSkillKeys(workspaceDir: string): Promise<Set<string>> {
  const installed = new Set<string>();
  const skillsDir = join(workspaceDir, ".thclaws", "skills");
  try {
    const entries = await readdir(skillsDir);
    for (const name of entries) {
      try {
        const meta = await stat(join(skillsDir, name));
        if (meta.isDirectory()) installed.add(name);
      } catch {
        // unreadable entry; skip
      }
    }
  } catch {
    // dir doesn't exist yet — first run; treat as empty.
  }
  return installed;
}

function buildSnapshot(input: {
  workspaceDir: string | null;
  available: AvailableSkill[];
  desired: Set<string>;
  installed: Set<string>;
}): AdapterSkillSnapshot {
  const { workspaceDir, available, desired, installed } = input;
  const availableByKey = new Map(available.map((s) => [s.key, s]));
  const entries: AdapterSkillEntry[] = [];
  const warnings: string[] = [];

  for (const entry of available) {
    const isDesired = desired.has(entry.key);
    const isInstalled = installed.has(entry.key);
    const state: AdapterSkillEntry["state"] = isDesired
      ? isInstalled
        ? "configured"
        : "configured"
      : "available";
    entries.push({
      key: entry.key,
      runtimeName: entry.runtimeName ?? entry.key,
      desired: isDesired,
      managed: true,
      required: entry.required ?? false,
      requiredReason: entry.requiredReason ?? null,
      state,
      origin: entry.required ? "paperclip_required" : "company_managed",
      originLabel: entry.required ? "Required by thCompany" : "Managed by thCompany",
      readOnly: false,
      sourcePath: entry.source ?? null,
      targetPath:
        workspaceDir && isInstalled
          ? join(workspaceDir, ".thclaws", "skills", entry.key, "SKILL.md")
          : null,
      detail: isDesired
        ? isInstalled
          ? "Materialized into the agent's workspace for this run."
          : "Will be materialized into the workspace on the next run."
        : null,
    });
  }

  for (const key of desired) {
    if (availableByKey.has(key)) continue;
    warnings.push(`Desired skill "${key}" is not available from the thCompany skills catalogue.`);
    entries.push({
      key,
      runtimeName: null,
      desired: true,
      managed: true,
      state: "missing",
      origin: "external_unknown",
      originLabel: "External or unavailable",
      readOnly: false,
      sourcePath: undefined,
      targetPath: undefined,
      detail: "thCompany cannot find this skill in its catalogue.",
    });
  }

  // Anything in the workspace's .thclaws/skills/ that thcompany didn't
  // put there — user dropped a SKILL.md by hand. Surface as external.
  for (const key of installed) {
    if (availableByKey.has(key) || desired.has(key)) continue;
    entries.push({
      key,
      runtimeName: key,
      desired: false,
      managed: false,
      state: "external",
      origin: "user_installed",
      originLabel: "User-installed",
      locationLabel: workspaceDir
        ? join(workspaceDir, ".thclaws", "skills")
        : ".thclaws/skills",
      readOnly: true,
      sourcePath: null,
      targetPath: workspaceDir
        ? join(workspaceDir, ".thclaws", "skills", key, "SKILL.md")
        : null,
      detail: "Present in the workspace but not managed by thCompany.",
    });
  }

  entries.sort((a, b) => a.key.localeCompare(b.key));
  const desiredArr = [...desired].sort();

  return {
    adapterType: ADAPTER_TYPE,
    supported: true,
    // Persistent because thcompany owns the lifecycle: it writes
    // skills before each run, prunes stale entries, and the daemon
    // reads from a stable on-disk path.
    mode: "persistent",
    desiredSkills: desiredArr,
    entries,
    warnings,
  };
}

export async function listSkills(ctx: AdapterSkillContext): Promise<AdapterSkillSnapshot> {
  const config = asConfig(ctx);
  const workspaceDir =
    typeof config.workspaceDir === "string" && config.workspaceDir.trim().length > 0
      ? config.workspaceDir.trim()
      : null;
  const available = readAvailableSkills(config.availableSkills);
  const desired = new Set(readStringArray(config.desiredSkills));
  const installed = workspaceDir ? await readInstalledSkillKeys(workspaceDir) : new Set<string>();
  return buildSnapshot({ workspaceDir, available, desired, installed });
}

export async function syncSkills(
  ctx: AdapterSkillContext,
  desiredSkills: string[],
): Promise<AdapterSkillSnapshot> {
  // Materialization happens at execute() time (it's batched with all
  // the other workspace files there). For listSkills/syncSkills we
  // return the post-state snapshot so the UI can render the new
  // desired set immediately; the actual filesystem write lands when
  // the agent's next run kicks off.
  const config = asConfig(ctx);
  const workspaceDir =
    typeof config.workspaceDir === "string" && config.workspaceDir.trim().length > 0
      ? config.workspaceDir.trim()
      : null;
  const available = readAvailableSkills(config.availableSkills);
  const desired = new Set(desiredSkills.map((s) => s.trim()).filter((s) => s.length > 0));
  const installed = workspaceDir ? await readInstalledSkillKeys(workspaceDir) : new Set<string>();
  return buildSnapshot({ workspaceDir, available, desired, installed });
}
