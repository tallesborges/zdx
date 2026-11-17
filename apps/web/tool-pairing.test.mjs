import { expect, test } from "bun:test";
import { buildNodes, findToolNode } from "./src/lib/transcript.ts";

const view = (outcome, extra = {}) => ({
  outcome,
  body: { kind: "empty" },
  status: [],
  notes: [],
  ...extra,
});
const use = (id, sequence, name = "Bash", extra = {}) => ({
  type: "tool_use",
  sequence,
  time: "",
  ts: "",
  id,
  name,
  input: { command: "x" },
  ...extra,
});
const result = (id, sequence, v = view("ok")) => ({
  type: "tool_result",
  sequence,
  time: "",
  ts: "",
  tool_use_id: id,
  ok: true,
  output: { ok: true, data: {} },
  view: v,
});
const running = (id, sequence) => ({
  type: "tool_running",
  sequence,
  time: "",
  ts: "",
  started_at: "",
  id,
  name: "bash",
  summary: "x",
  input: { command: "x" },
  output_tail: "tail",
  running_for: "3s",
  view: view("pending"),
});

const timedOut = view("timed_out", { hint: { text: "timed out", bad: true, kind: "detail" } });
const activity = [
  use("done", 1),
  result("done", 2, timedOut),
  use("cut", 3, "Read", { view: view("pending", { head: { kind: "path", path: "a.rs" } }) }),
  running("live", 4),
  result("lost", 5),
];

test("a result's view replaces the call's and drives the failure flag", () => {
  const node = findToolNode(activity, "done", 2);
  expect(node.view).toBe(timedOut);
  expect(node.summary.failed).toBe(true);
});

test("an interrupted call keeps its input-only view", () => {
  const node = findToolNode(activity, "cut");
  expect(node.result).toBeUndefined();
  expect(node.running).toBeUndefined();
  expect(node.view.head.path).toBe("a.rs");
});

test("a live call carries its output tail and running view", () => {
  const node = findToolNode(activity, "live");
  expect(node.running.output_tail).toBe("tail");
  expect(node.view.outcome).toBe("pending");
});

test("a result without its call is found only by its own sequence", () => {
  expect(findToolNode(activity, "lost", 5).orphaned).toBe(true);
  expect(findToolNode(activity, "lost")).toBeUndefined();
  expect(findToolNode(activity, "nope", 9)).toBeUndefined();
});

test("a bash row prefers its description label over the raw command", () => {
  const cmd = [
    use("b1", 1, "Bash", {
      input: { command: "cargo test", description: "Run the test suite" },
      view: view("pending", {
        head: { kind: "shell", command: "cargo test", label: "Run the test suite" },
      }),
    }),
  ];
  const [work] = buildNodes(cmd);
  const [node] = work.items;
  expect(node.summary.subject).toBe("Run the test suite");
  expect(node.summary.mono).toBe(false);

  // Old calls without a label still show the command in monospace.
  const plain = [use("b2", 1, "Bash", { input: { command: "ls" } })];
  const [pWork] = buildNodes(plain);
  const [pNode] = pWork.items;
  expect(pNode.summary.subject).toBe("ls");
  expect(pNode.summary.mono).toBe(true);
});

test("edit line counts come from the view's diff", () => {
  const edit = [
    use("e", 1, "Edit", { input: { file_path: "a.rs", old_string: "x", new_string: "y" } }),
    result("e", 2, view("ok", { body: { kind: "diff", lines: [], additions: 1, deletions: 1 } })),
  ];
  const [work] = buildNodes(edit);
  const [node] = work.items;
  expect(node.summary.additions).toBe(1);
  expect(node.summary.deletions).toBe(1);
});
