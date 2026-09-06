import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { buildWeb } from "./build-cached.mjs";

const roots = [];
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "zdx-web-build-"));
  roots.push(root);
  const write = (path, content) => {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), content);
  };
  write("src/main.ts", "initial source");
  write("bun.lock", "initial lockfile");
  const calls = [];
  const run = (args) => {
    calls.push(args);
    if (args[0] === "run") {
      rmSync(join(root, "dist"), { recursive: true, force: true });
      write("dist/index.html", "built html");
      write("dist/assets/app.js", "built js");
    }
  };
  const build = (env = {}) => buildWeb(root, run, env);
  return { root, write, run, calls, build };
}

test("unchanged builds skip both commands and leave embedded files untouched", () => {
  const { root, write, calls, build } = fixture();
  expect(build()).toBe(true);
  const before = statSync(join(root, "dist/index.html"));
  write("AGENTS.md", "documentation only");
  expect(build()).toBe(false);
  expect(calls).toEqual([["install", "--frozen-lockfile"], ["run", "build"]]);
  const after = statSync(join(root, "dist/index.html"));
  expect(after.mtimeMs).toBe(before.mtimeMs);
  expect(after.ino).toBe(before.ino);
});

test("source, config, dependencies, env files and public additions/deletions invalidate", () => {
  const { root, write, build } = fixture();
  build();
  for (const path of ["src/main.ts", "vite.config.ts", "package.json", "bun.lock", ".env.production", "public/icon.svg", "build-cached.mjs"]) {
    write(path, "changed");
    expect(build()).toBe(true);
    expect(build()).toBe(false);
  }
  rmSync(join(root, "public/icon.svg"));
  expect(build()).toBe(true);
  expect(build({ VITE_TITLE: "new title" })).toBe(true);
  expect(build({ VITE_TITLE: "new title" })).toBe(false);
  expect(build({ VITE_TITLE: "new title", NODE_ENV: "development" })).toBe(true);
});

test("missing, modified or extra output files and missing cache trigger rebuilding", () => {
  const { root, write, build } = fixture();
  build();
  write("dist/assets/app.js", "corrupted");
  expect(build()).toBe(true);
  write("dist/extra.txt", "unexpected");
  expect(build()).toBe(true);
  for (const path of ["dist/assets/app.js", "dist/index.html", "dist", "node_modules"]) {
    rmSync(join(root, path), { recursive: true, force: true });
    expect(build()).toBe(true);
    expect(build()).toBe(false);
  }
});

test("failed builds cannot leave a valid cache for partially written outputs", () => {
  const { root, write, build } = fixture();
  build();
  write("dist/assets/app.js", "corrupted");
  expect(() => buildWeb(root, () => { throw new Error("build failed"); }, {})).toThrow("build failed");
  write("dist/assets/app.js", "built js");
  expect(build()).toBe(true);
  expect(readFileSync(join(root, "dist/assets/app.js"), "utf8")).toBe("built js");
});