// Simulated-host harness for kn9t-subagent. Two scenarios:
//  1. default (recursion allowed): a hook arrives WHILE the plugin awaits an
//     api_result — it must be served inline (event pump), proving re-entrancy;
//  2. KN9T_SUBAGENT_RECURSION=deny: the child toolset excludes spawn_session.
import { spawn } from "node:child_process";
import assert from "node:assert/strict";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function launch(env = {}) {
  const proc = spawn("node", ["dist/main.js"], {
    stdio: ["pipe", "pipe", "inherit"],
    env: { ...process.env, ...env },
  });
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
  return { proc, send, waitFor, got, kill: () => proc.kill() };
}

async function scenario1_reentrancy() {
  const { proc, send, waitFor, got, kill } = launch();
  send({ t: "hello", proto: 1, kn9t: "0.1.0-test" });
  const hello = await waitFor((m) => m.t === "hello" && m.name, "plugin hello");
  assert.equal(hello.tools[0].name, "spawn_session");
  console.log("✓ hello:", hello.name);

  // The agent calls spawn_session on the PARENT session.
  send({
    t: "hook",
    id: 42,
    hook: "tool_call",
    payload: { tool: "spawn_session", args: { task: "check the diff" }, session: "parent-001" },
  });

  // Serve requests; on session_prompt, FIRST throw a nested tool_call at the
  // plugin (the CHILD calling spawn_session) — it must be served inline.
  const seen = { fork: 0, prompt: 0, nFork: 0, nPrompt: 0 };
  let outerPromptId = 0;
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    await sleep(10);
    const req = got.find((m) => m.t === "request");
    if (req) {
      got.splice(got.indexOf(req), 1);
      if (req.op === "session_fork" && seen.fork === 0) {
        seen.fork++;
        assert.equal(req.payload.session, "parent-001", "outer fork carries the parent session");
        send({ t: "api_result", id: req.id, ok: true, result: { session: "child-900" } });
      } else if (req.op === "session_prompt" && seen.prompt === 0) {
        seen.prompt++;
        outerPromptId = req.id;
        assert.equal(req.payload.session, "child-900");
        assert.equal(req.payload.tools, undefined, "recursion allowed → inherit toolset");
        assert.ok(req.payload.text.includes("sub-agent session"), "child task directive");
        // Re-entrancy: the child calls spawn_session WHILE we owe the prompt reply.
        send({
          t: "hook",
          id: 77,
          hook: "tool_call",
          payload: { tool: "spawn_session", args: { task: "say hi" }, session: "child-900" },
        });
        // The nested spawn will be served before we reply — wait for its fork.
      } else if (req.op === "session_fork" && seen.fork > 0 && seen.prompt > 0) {
        seen.nFork++;
        assert.equal(req.payload.session, "child-900", "nested fork is for the child");
        send({ t: "api_result", id: req.id, ok: true, result: { session: "child-901" } });
      } else if (req.op === "session_prompt") {
        seen.nPrompt++;
        assert.equal(req.payload.session, "child-901", "nested prompt runs on the grandchild");
        send({
          t: "api_result",
          id: req.id,
          ok: true,
          result: { session: "child-901", result: "grandchild says hi" },
        });
      } else {
        send({ t: "api_result", id: req.id, ok: false, error: `unhandled op ${req.op}` });
      }
      continue;
    }
    // The nested hook reply (id 77).
    const hookReply = got.find((m) => m.t === "result" && m.id === 77);
    if (hookReply) {
      got.splice(got.indexOf(hookReply), 1);
      assert.ok(!hookReply.is_error, `nested tool error: ${JSON.stringify(hookReply.content)}`);
      assert.ok(
        hookReply.content[0].text.includes("[sub-agent session child-901] grandchild says hi"),
        `nested spawn served inline, got ${hookReply.content[0].text}`
      );
      // The child is unblocked → now complete the outer session_prompt.
      send({
        t: "api_result",
        id: outerPromptId,
        ok: true,
        result: { session: "child-900", result: "no regressions found" },
      });
      continue;
    }
    const done = got.find((m) => m.t === "result" && m.id === 42);
    if (done) {
      assert.ok(!done.is_error, `tool error: ${JSON.stringify(done.content)}`);
      assert.ok(done.content[0].text.includes("no regressions found"), done.content[0].text);
      assert.equal(seen.fork, 1);
      assert.equal(seen.prompt, 1);
      assert.equal(seen.nFork, 1, "nested fork counted");
      assert.equal(seen.nPrompt, 1, "nested prompt counted");
      console.log("✓ scenario 1 (recursion allowed): re-entrant spawn served inline");
      console.log("  result:", done.content[0].text);
      kill();
      return;
    }
  }
  console.error("✗ scenario 1 timeout; got:", JSON.stringify(got));
  kill();
  process.exit(1);
}

async function scenario2_recursion_denied() {
  const { proc, send, waitFor, got, kill } = launch({ KN9T_SUBAGENT_RECURSION: "deny" });
  send({ t: "hello", proto: 1, kn9t: "0.1.0-test" });
  await waitFor((m) => m.t === "hello" && m.name, "plugin hello");

  send({
    t: "hook",
    id: 9,
    hook: "tool_call",
    payload: { tool: "spawn_session", args: { task: "list files" }, session: "parent-002" },
  });

  const seen = { list: 0, fork: 0, prompt: 0 };
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    await sleep(10);
    const req = got.find((m) => m.t === "request");
    if (req) {
      got.splice(got.indexOf(req), 1);
      if (req.op === "tool_list") {
        seen.list++;
        send({
          t: "api_result",
          id: req.id,
          ok: true,
          result: { tools: ["bash", "read", "spawn_session", "mcp_list_servers"] },
        });
      } else if (req.op === "session_fork") {
        seen.fork++;
        send({ t: "api_result", id: req.id, ok: true, result: { session: "child-902" } });
      } else if (req.op === "session_prompt") {
        seen.prompt++;
        assert.ok(Array.isArray(req.payload.tools), "deny → explicit toolset");
        assert.ok(req.payload.tools.includes("bash"), "regular tools inherited");
        assert.ok(
          !req.payload.tools.includes("spawn_session"),
          `deny → spawn_session excluded, got ${JSON.stringify(req.payload.tools)}`
        );
        send({
          t: "api_result",
          id: req.id,
          ok: true,
          result: { session: "child-902", result: "listed" },
        });
      } else {
        send({ t: "api_result", id: req.id, ok: false, error: `unhandled op ${req.op}` });
      }
      continue;
    }
    const done = got.find((m) => m.t === "result" && m.id === 9);
    if (done) {
      assert.ok(!done.is_error, `tool error: ${JSON.stringify(done.content)}`);
      assert.equal(seen.list, 1, "tool_list consulted");
      console.log("✓ scenario 2 (recursion denied): child toolset excludes spawn_session");
      kill();
      return;
    }
  }
  console.error("✗ scenario 2 timeout; got:", JSON.stringify(got));
  kill();
  process.exit(1);
}

await scenario1_reentrancy();
await scenario2_recursion_denied();
console.log("✓ all scenarios passed");
process.exit(0);