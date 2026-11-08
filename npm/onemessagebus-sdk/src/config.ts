// A bus configuration written from its generated type, for the binary to read.
import { writeFile } from "node:fs/promises";
import { BusRefused } from "./errors.js";
import { type Config, ConfigSchema } from "./generated/roots/config.js";
import { violation } from "./message.js";

/** One entry of a configuration's `hooks:` list. */
export type HookConfig = NonNullable<Config["hooks"]>[number];

/**
 * Write `config` to `path` as the binary reads a configuration, and answer `path`.
 * It is validated by the generated `config` schema first, so a configuration the
 * binary would refuse rejects with `BusRefused` naming the pointer, and nothing is
 * written. The document is JSON, which every YAML reader reads.
 */
export async function writeConfig(config: Config, path: string): Promise<string> {
  const parsed = ConfigSchema.safeParse(config);
  if (!parsed.success) throw new BusRefused(violation("config", parsed.error));
  await writeFile(path, `${JSON.stringify(config, null, 2)}\n`);
  return path;
}
