# Chapter 12 — Skills

A skill is a **reusable workflow** packaged as a directory containing:

- `SKILL.md` — YAML frontmatter (name, description, whenToUse) plus
  Markdown instructions the model follows.
- `scripts/` (optional) — shell / Python / Node scripts the SKILL.md
  refers to. The model calls them via `Bash`, never rewrites them.

Skills are how you turn "please deploy using our 6-step ritual" into a
single tool call. The model reads the SKILL.md, follows its
instructions, and uses the scripts you've already written.

## Discovery

thClaws looks in these dirs on startup (in order):

1. `.thclaws/skills/` — project-scoped
2. `~/.config/thclaws/skills/` — user global
3. `~/.claude/skills/` — Claude Code compat
4. Plugin-contributed dirs

`/skills` lists what's loaded. `/skill show <name>` prints the full
SKILL.md content + resolved path.

## Marketplace

The thClaws marketplace is a curated, license-vetted catalog of skills
maintained at [thClaws/marketplace](https://github.com/thClaws/marketplace).
The client fetches the catalog from `thclaws.ai/api/marketplace.json`
and exposes three discovery commands:

```
❯ /skill marketplace
marketplace (baseline 2026-04-29, 1 skill(s))
── development ──
  skill-creator            — Create new skills, optimize triggering, run evals
install with: /skill install <name>   |   detail: /skill info <name>
```

```
❯ /skill search creator
1 match(es) for 'creator':
  skill-creator            — Create new skills, optimize triggering, run evals
```

```
❯ /skill info skill-creator
name:        skill-creator
description: Create new skills, modify and improve existing skills, and measure skill performance...
category:    development
license:     Apache-2.0 (open)
source:      thClaws/marketplace (skills/skill-creator)
homepage:    https://github.com/thClaws/marketplace/tree/main/skills/skill-creator
install:     /skill install skill-creator (resolves to https://github.com/thClaws/marketplace.git#main:skills/skill-creator)
```

`/skill marketplace --refresh` fetches the latest catalog from
`thclaws.ai` (default cadence is on-demand; the embedded baseline
covers offline use). Search is case-insensitive and ranks name matches
above description matches.

### License tiers

Each entry carries a `license_tier`:

- **`open`** — Apache-2.0 / MIT / similar. `/skill install <name>`
  installs directly.
- **`linked-only`** — source-available; cannot be redistributed.
  Visible in the catalog (so you know it exists) but
  `/skill install <name>` refuses with the upstream homepage URL —
  install from the upstream repo manually if you want it.

## Installing a skill

### From the marketplace (recommended)

If the skill is in the marketplace catalog, install it by name — no
URL needed:

```
❯ /skill install skill-creator
  cloned https://github.com/thClaws/marketplace.git (subpath: skills/skill-creator) → .thclaws/skills/skill-creator
  installed skill 'skill-creator' (single)
(skill available in this session — no restart needed)
```

Behind the scenes, thClaws resolves the marketplace name to the entry's
`install_url`, clones just that subpath out of the registry repo
(see "From a git repo" below for the subpath syntax), and lands the
result at `.thclaws/skills/<name>/`. The skill becomes available in
the same session.

### From a git repo

For skills not in the marketplace, pass any git URL:

```
❯ /skill install https://github.com/some-user/some-skill.git
  cloned https://github.com/some-user/some-skill.git → .thclaws/skills/some-skill
  installed skill 'some-skill' (single)
```

If the repo is a **bundle** (multiple skills under subdirectories),
thClaws auto-detects and promotes each sub-skill to a sibling at
`.thclaws/skills/<sub-name>/`.

#### Subpath syntax (one skill from a multi-skill repo)

To install just one directory out of a larger repo, use the
`#<branch>:<subpath>` extension:

```
❯ /skill install https://github.com/anthropics/skills.git#main:skills/canvas-design
  cloned https://github.com/anthropics/skills.git (subpath: skills/canvas-design) → .thclaws/skills/canvas-design
  installed skill 'canvas-design' (single)
```

The repo is cloned to a staging dir, only the requested subpath is
moved into place, and the rest is discarded. The derived skill name
comes from the subpath's last segment (`canvas-design` here), not the
repo URL.

### From a `.zip` URL

```
❯ /skill install https://example.com/api/skills/deploy-v1.zip
  downloaded https://example.com/...zip (4210 bytes) → extracted
  installed skill 'deploy-v1' (single)
```

- 64 MB size cap.
- Zip-slip guarded (malicious archive paths rejected).
- Unix exec bits preserved so shipped scripts stay runnable.
- A single top-level wrapper directory (`pack-v1/...`) is auto-unwrapped.

### Scope

`--user` installs into `~/.config/thclaws/skills/` instead of the
project's `.thclaws/skills/`. Default is project. The marketplace
respects this flag too:

```
❯ /skill install --user skill-creator
```

### Override the derived name

```
❯ /skill install https://example.com/deploy.zip ourdeploy
```

## Invoking a skill

Three equivalent ways:

1. **Let the model decide.** The skill's `whenToUse` trigger shows up
   in the system prompt; the model calls `Skill(name: "…")` on match.

   ```
   ❯ make me a PDF from this data
   Using the `pdf` skill to generate a PDF...
   [tool: Skill: pdf] ✓
   [tool: Bash: .../scripts/pdf_from_data.py] ✓
   ```

2. **Direct slash shortcut.** `/pdf [args]` is rewritten into a
   `Skill(name: "pdf")` call.

   ```
   ❯ /pdf from the report markdown, 10pt font
   (/pdf → Skill(name: "pdf"))
   ```

3. **Explicit from a prompt.** "Use the pdf skill to…" — the model
   obliges.

## SKILL.md anatomy

```markdown
---
name: deploy-to-staging
description: Deploy the current branch to staging and run smoke tests
whenToUse: When the user asks to deploy or ship to staging
---

1. Ensure the working tree is clean (`git status`). Abort if dirty.
2. Run `{skill_dir}/scripts/build.sh` to build the production bundle.
3. Upload `dist/` via `{skill_dir}/scripts/push-to-staging.sh`.
4. Run `{skill_dir}/scripts/smoke-test.sh` — it should exit 0.
5. Report the staging URL to the user.

If any step fails, abort and report the failing step.
```

Frontmatter fields:

| Field | Required | Purpose |
|---|---|---|
| `name` | yes | Unique skill id (matches the filename by default) |
| `description` | recommended | Short one-liner shown in `/skills` |
| `whenToUse` | recommended | Trigger hint the model uses to decide on invocation |
| `model` | optional | Recommended default model — applied automatically when the user has the relevant API key. Single string or array; see below |

`{skill_dir}` in the body is substituted with the absolute path to the
skill directory at load time, so script paths always resolve regardless
of where the user launched thClaws.

### `model:` — recommend a default

Some skills assume capabilities not every model has — vision (read a
namecard photo, parse a receipt image), long context (summarise a 200-
page PDF), structured output (XLSX cell formulas). Adding a `model:`
field lets the skill author encode the recommendation so non-expert
users get a known-good default automatically.

Single recommendation:

```yaml
---
name: namecard-to-excel
description: Extract namecard info from a photo into Excel
whenToUse: When user shares a namecard photo to add to contacts
model: claude-sonnet-4-6
---
```

Priority list (first one the user has a key for wins):

```yaml
---
model: [claude-sonnet-4-6, gpt-4o, gemini-2.5-pro]
---
```

Behavior when the skill is invoked:

- **User has a key for the recommended model** — silent auto-switch
  for the duration of the turn. Chat shows
  `[model → claude-sonnet-4-6 (skill recommendation, reverts at end of turn)]`
  when the swap takes effect, and `[model → <baseline> (skill ended)]`
  when the turn finishes. The user's previously-active model is
  restored automatically.
- **User has no key for any candidate** — chat shows
  `[skill recommends claude-sonnet-4-6; you don't have a key for that
  provider — using current model]` and the skill proceeds with the
  current model. Add a key in Settings if results look poor.

The override is **per-turn only** — `/model` switches the user makes
explicitly are unaffected, and the next user prompt starts from the
baseline again. This keeps the swap unobtrusive: it helps the skill
without taking over the session.

### Override the recommendation via `settings.json`

Built-in skills (currently `extract-and-save` — see [Chapter 9](ch09-knowledge-bases-kms.md) and Chapter 1 for context) ship with a default model recommendation in their embedded SKILL.md. You can override that recommendation from `settings.json` without forking the entire skill body — useful when the default (e.g. `gpt-4.1-nano`) needs an OpenAI key that you don't have, but you'd like the skill to still apply your preferred Claude model:

```json
// .thclaws/settings.json (project) or ~/.config/thclaws/settings.json (user)
{
  "extract_save_skill_models": "claude-sonnet-4-6"
}
```

Priority list (first model the user has a key for wins):

```json
{
  "extract_save_skill_models": ["claude-sonnet-4-6", "gpt-4o", "gemini-2.5-pro"]
}
```

**Resolution chain** when the skill is invoked:

1. `settings.json` field (e.g. `extract_save_skill_models`) — wins if present
2. SKILL.md frontmatter `model:` — fallback when no override is configured
3. No recommendation — your current model stays untouched

Each future built-in skill that needs special model selection will get its own settings field (`tts_skill_models`, `transcribe_skill_models`, etc.) — the per-skill named convention scales better than a generic map for the small curated set of built-ins, and makes the intent visible in `settings.json`.

## Writing your own skill

Smallest possible skill:

```
.thclaws/skills/hello/
  SKILL.md
```

```markdown
---
name: hello
description: Say hi in all caps
whenToUse: User asks to say hi loudly
---

Reply with exactly "HELLO!" and nothing else.
```

That works — no scripts needed for prompt-only skills.

For script-driven work:

```
.thclaws/skills/greet/
  SKILL.md
  scripts/
    greet.sh
```

```markdown
---
name: greet
description: Run the greeting script with the user's name
whenToUse: User asks to greet someone
---

Run `bash {skill_dir}/scripts/greet.sh <name>` where <name> is the
person to greet.
```

```bash
# scripts/greet.sh
#!/bin/sh
echo "Hello, $1! Glad to see you."
```

Make the script executable (`chmod +x`) and the `Bash` tool will run
it directly.

## Runtime refresh

After `/skill install`, thClaws immediately re-discovers skills,
refreshes the SkillTool's live store and the `/<skill-name>` shortcut
resolver, and rebuilds the system prompt so the new skill shows up in
the `# Available skills` section — no restart required. Works in the
CLI REPL and either GUI tab.

## Writing for different models

Smaller local models (Gemma via Ollama, 7-12B parameter Qwens) follow
explicit, imperative skill instructions better than loose ones. When
authoring for broad compatibility:

- Say "run X" not "consider running X".
- Keep steps numbered.
- Put failure handling at the end, not scattered in each step.
- Avoid Markdown nested bullets — some tokenizers mangle them.

Skills that work on Claude Sonnet aren't guaranteed to work on
`ollama/gemma3:12b`; test on your target models.
