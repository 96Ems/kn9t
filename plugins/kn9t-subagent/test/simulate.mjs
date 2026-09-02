// Simulated-host harness for kn9t-subagent: drives the plugin over stdio and
// asserts the spawn_session tool round trip (fork → prompt → result with the
// child session id).
import { spawn } from "node:child_process";
import assert from "node:assert/strict";

const proc = spawn("node", ["dist/main.js"], { stdio: ["pipe", "pipe", "inherit"] });
let buf = "";
const got = [];
proc.stdout.on("data", (d) => {
  buf += d.toString("utf8");
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    got.push(JSON.parse(buf.slice(0, i)));
    buf = buf.slice(i + 1);
  }
});
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const send = (m) => proc.stdin.write(JSON.stringify(m) + "\n");

async function waitFor(pred, what, timeoutMs = 15000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const idx = got.findIndex(pred);
    if (idx >= 0) return got.splice(idx, 1)[0];
    await sleep(10);
  }
  throw new Error(`timeout waiting for ${what}; got: ${JSON.stringify(got)}`);
}

send({ t: "hello", proto: 1, kn9t: "0.1.0-test" });

const hello = await waitFor((m) => m.t === "hello" && m.name, "plugin hello");
assert.deepEqual(hello.capabilities, ["host_api"]);
assert.equal(hello.tools[0].name, "spawn_session");
console.log("✓ hello:", hello.name, "| tool:", hello.tools[0].name);

send({
  t: "hook",
  id: 7,
  hook: "tool_call",
  payload: {
    tool: "spawn_session",
    args: { task: "check the diff and report regressions", budget_usd: 0.5 },
    session: "parent-001",
  },
});

// Drive the requests: session_fork then session_prompt.
let forkSeen = 0;
let promptSeen = 0;
const deadline = Date.now() + 15000;
while (Date.now() < deadline) {
  await sleep(10);
  const req = got.find((m) => m.t === "request");
  if (req) {
    got.splice(got.indexOf(req), 1);
    if (req.op === "session_fork") {
      forkSeen++;
      assert.equal(req.payload.session, "parent-001", "fork carries the parent session");
      assert.equal(req.payload.copy_events, true, "child inherits the transcript");
      assert.equal(req.payload.budget_usd, 0.5, "budget forwarded");
      send({ t: "api_result", id: req.id, ok: true, result: { session: "child-900" } });
    } else if (req.op === "session_prompt") {
      promptSeen++;
      assert.equal(req.payload.session, "child-900", "prompt runs on the forked child");
      assert.equal(req.payload.text, "check the diff and report regressions");
      send({
        t: "api_result",
        id: req.id,
        ok: true,
        result: { session: "child-900", result: "no regressions found" },
      });
    } else {
      send({ t: "api_result", id: req.id, ok: false, error: `unhandled op ${req.op}` });
    }
    continue;
  }
  const done = got.find((m) => m.t === "result" && m.id === 7);
  if (done) {
    assert.ok(!done.is_error, `tool error: ${JSON.stringify(done.content)}`);
    const text = done.content[0].text;
    assert.ok(text.includes("[sub-agent session child-900]"), `result carries child id, got: ${text}`);
    assert.ok(text.includes("no regressions found"), `result carries child output, got: ${text}`);
    assert.equal(forkSeen, 1, "exactly one fork");
    assert.equal(promptSeen, 1, "exactly one prompt");
    console.log("✓ spawn_session round trip OK: fork → prompt → result");
    console.log("  result:", text);
    proc.kill();
    process.exit(0);
  }
}
console.error("✗ no result within deadline; got:", JSON.stringify(got));
proc.kill();
process.exit(1);