import { parse } from "jsr:@std/toml";
import { parseArgs } from "jsr:@std/cli/parse-args";

const flags = parseArgs(Deno.args, {
  string: ["path"],
});

if (!flags.path) {
  console.error("path is required");
  Deno.exit(1);
}

const file = await Deno.readTextFile(flags.path);
const toml = parse(file);

if (!toml.package || typeof toml.package !== "object") {
  console.error("package is not found in the file");
  Deno.exit(1);
}

const version = (toml.package as { version?: string }).version;

if (!version) {
  console.error("version is not found in the file");
  Deno.exit(1);
}

console.log(version);
