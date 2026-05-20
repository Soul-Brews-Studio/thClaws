/**
 * Server-side adapter factory.
 *
 * Paperclip's plugin loader calls `createServerAdapter()` on this
 * module's main export and registers the returned
 * `ServerAdapterModule` in its mutable adapter registry. See
 * paperclip/server/src/adapters/plugin-loader.ts for the exact
 * contract.
 *
 * dev-plan/21: factory now declares the full feature surface (skills,
 * sessions, instructions bundle, model profiles) so Paperclip's UI
 * exposes the same tabs/pickers it shows for claude_local. Backing
 * implementations are intentionally thin (delegate to thClaws's
 * native primitives — settings layering, skills dir scanning, session
 * persistence — instead of duplicating that logic in the adapter).
 */

import type { ServerAdapterModule } from "@paperclipai/adapter-utils";
import { execute } from "./execute.js";
import { testEnvironment } from "./test.js";
import { listSkills, syncSkills } from "./skills.js";
import { sessionCodec } from "./session-codec.js";
import { modelProfiles } from "./model-profiles.js";
import { models, type, agentConfigurationDoc } from "../index.js";

export function createServerAdapter(): ServerAdapterModule {
  return {
    type,
    execute,
    testEnvironment,
    models,
    modelProfiles,
    agentConfigurationDoc,
    listSkills,
    syncSkills,
    sessionCodec,
    // thClaws reads <cwd>/.claude/CLAUDE.md + <cwd>/.thclaws/*.md
    // natively (see crates/core/src/config.rs). Paperclip writes the
    // bundle to whatever path this key on adapterConfig points to —
    // recommend setting `instructionsFilePath: ".claude/CLAUDE.md"`
    // in the agent config so it lands somewhere thClaws actually scans.
    supportsInstructionsBundle: true,
    instructionsPathKey: "instructionsFilePath",
    supportsLocalAgentJwt: true,
    // dev-plan/25 Phase B: thcompany-managed skills are materialized
    // into <workspaceDir>/.thclaws/skills/ before each /agent/run.
    // The orchestrator gates skill writes on this flag.
    requiresMaterializedRuntimeSkills: true,
  };
}

// Convenience re-exports so callers can import individual pieces
// without going through the factory.
export { execute } from "./execute.js";
export { testEnvironment } from "./test.js";
export { extractTokenSummary, stripTokenSummary } from "./parse.js";
export { listSkills, syncSkills } from "./skills.js";
export { sessionCodec } from "./session-codec.js";
export { modelProfiles } from "./model-profiles.js";
