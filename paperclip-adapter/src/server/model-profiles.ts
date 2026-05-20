/**
 * Curated model profiles for the thClaws adapter.
 *
 * Maps Paperclip's "cheap / balanced / premium" UX presets to actual
 * model ids in thClaws's catalogue. Users can override by setting
 * `model` in adapterConfig directly.
 */

import type { AdapterModelProfileDefinition } from "@paperclipai/adapter-utils";

export const modelProfiles: AdapterModelProfileDefinition[] = [
  {
    key: "cheap",
    label: "Cheap (Haiku-class)",
    description: "Cheapest reliable model — good for tool-use loops where reasoning is light.",
    adapterConfig: { model: "claude-haiku-4-5" },
    source: "adapter_default",
  },
];
