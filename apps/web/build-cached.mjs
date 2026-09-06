import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";

function treeHash(root, excluded = new Set()) {
  const hash = createHash("sha256");
  function visit(relative) {
    const path = join(root, relative);
    if (statSync(path).isDirectory()) {
      for (const name of readdirSync(path).sort()) {
        if (!relative && excluded.has(name)) continue;
        visit(relative ? `${relative}/${name}` : name);
      }
    } else {
      hash.update(JSON.stringify(relative));
      hash.update(createHash("sha256").update(readFileSync(path)).digest());
    }
  }
  visit("");
  return hash.digest("hex");
}

function inputHash(root, env) {
  const environment = Object.entries(env)
    .filter(([key]) => key.startsWith("VITE_") || key === "NODE_ENV")
    .sort(([a], [b]) => a.localeCompare(b));
  return JSON.stringify([
    process.versions.bun,
    process.platform,
    process.arch,
    environment,
    treeHash(root, new Set(["node_modules", "dist", ".git", "AGENTS.md"])),
  ]);
}

function runBun(args, root, env) {
  const result = spawnSync(process.execPath, args, {
    cwd: root,
    env,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`bun ${args.join(" ")} failed (${result.signal ?? result.status})`);
  }
}

export function buildWeb(root, run = runBun, env = process.env) {
  // Keep cache metadata outside dist: rust-embed embeds every file there.
  const cache = join(root, "node_modules/.cache/zdx-web-build.json");
  const dist = join(root, "dist");
  const inputs = inputHash(root, env);
  if (existsSync(cache) && existsSync(join(dist, "index.html"))) {
    const current = JSON.stringify({ inputs, outputs: treeHash(dist) });
    if (readFileSync(cache, "utf8") === current) return false;
  }

  rmSync(cache, { force: true });
  run(["install", "--frozen-lockfile"], root, env);
  run(["run", "build"], root, env);
  if (!existsSync(join(dist, "index.html"))) {
    throw new Error("Web build did not produce dist/index.html");
  }
  if (inputHash(root, env) === inputs) {
    mkdirSync(dirname(cache), { recursive: true });
    writeFileSync(cache, JSON.stringify({ inputs, outputs: treeHash(dist) }));
  }
  return true;
}

if (import.meta.main) {
  if (!buildWeb(import.meta.dir)) {
    console.log("Web build unchanged; skipping Bun install and Vite (dist intact).");
  }
}