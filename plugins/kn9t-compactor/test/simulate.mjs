// Simulated-host harness: drives the built kn9t-compactor plugin over stdio
// and asserts the compactor_compact round trip (session_read → triage →
// summary → plan with verbatim kept tool result).
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

const SPAN = [
  { seq: 1, role: "user", content: [{ type: "text", text: "fix the bug" }] },
  { seq: 2, role: "assistant", content: [{ type: "tool_call", id: "t1", name: "bash", args_json: "{\"cmd\":\"ls\"}" }] },
  { seq: 3, role: "assistant", content: [{ type: "tool_result", id: "t1", is_error: false, content: [{ type: "text", text: "file1 file2" }] }] },
  { seq: 4, role: "assistant", content: [{ type: "tool_call", id: "t2", name: "bash", args_json: "{\"cmd\":\"ls /tmp\"}" }] },
  { seq: 5, role: "assistant", content: [{ type: "tool_result", id: "t2", is_error: false, content: [{ type: "text", text: "scratch noise (bbbb\ncccc)" }] }] },
];

send({ t: "hello", proto: 1, kn9t: "0.1.0-test" });

const hello = await waitFor((m) => m.t === "hello" && m.name, "plugin hello");
assert.deepEqual(hello.capabilities.sort(), ["compactor", "host_api"].sort(), "capabilities");
console.log("✓ hello:", hello.name, hello.capabilities.join(","));

send({ t: "hook", id: 42, hook: "compactor_compact", payload: { session: "sess-x", model: { provider: "test", id: "m1" }, replaced: { start: 1, end: 3 } } });

// Drive requests until the final result lands.
const deadline = Date.now() + 15000;
while (Date.now() < deadline) {
  await sleep(10);
  const req = got.find((m) => m.t === "request");
  if (req) {
    got.splice(got.indexOf(req), 1);
    if (req.op === "session_read") {
      assert.equal(req.payload.session, "sess-x");
      send({ t: "api_result", id: req.id, ok: true, result: { messages: SPAN } });
    } else if (req.op === "provider_complete") {
      assert.equal(req.payload.session, "sess-x", "provider_complete carries session");
      assert.ok(Array.isArray(req.payload.messages) && req.payload.messages.length === 2, "two messages");
      const sys = req.payload.messages[0].content[0].text;
      const reply = sys.includes("compaction planner")
        ? { content: [{ type: "text", text: JSON.stringify({ decisions: [{ id: "t1", action: "keep" }, { id: "t2", action: "drop" }], resume_actions: ["run the fix"] }) }] }
        : { content: [{ type: "text", text: JSON.stringify({ summary: "all done — keep bash output" }) }] };
      send({ t: "api_result", id: req.id, ok: true, result: reply });
    } else {
      send({ t: "api_result", id: req.id, ok: false, error: `unhandled op ${req.op}` });
    }
    continue;
  }
  const done = got.find((m) => m.t === "result" && m.id === 42);
  if (done) {
    assert.ok(!done.error, `compactor error: ${done.error}`);
    assert.equal(done.summary.role, "assistant");
    const textBlock = done.summary.content.find((b) => b.type === "text");
    assert.ok(textBlock.text.includes("all done"), "summary text from the model");
    const kept = done.summary.content.find((b) => b.type === "tool_result" && b.id === "t1");
    assert.ok(kept, "kept tool result embedded verbatim");
    assert.equal(kept.content[0].text, "file1 file2", "byte-exact kept output");
    assert.deepEqual(done.handoff.keep, ["t1"]);
    assert.deepEqual(done.handoff.drop, ["t2"]);
    assert.deepEqual(done.handoff.resume_actions, ["run the fix"]);
    console.log("✓ compactor_compact round trip OK");
    console.log("  summary content blocks:", done.summary.content.map((b) => b.type).join(", "));
    console.log("  handoff:", JSON.stringify(done.handoff));
    proc.kill();
    process.exit(0);
  }
}
console.error("✗ no result within deadline; got:", JSON.stringify(got));
proc.kill();
process.exit(1);