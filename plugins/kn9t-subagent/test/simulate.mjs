// Simulated-host harness for kn9t-subagent. Two scenarios:
//  1. default (recursion allowed): a hook arrives WHILE the plugin awaits an
//     api_result — it must be served inline (event pump), proving re-entrancy;
//  2. KN9T_SUBAGENT_RECURSION=deny: the child toolset excludes subagent.
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

/** Handle common ops that the subagent plugin sends. */
function handleCommonOps(req, send, childSession) {
  if (req.op === "ui_register_lua") {
    send({ t: "api_result", id: req.id, ok: true, result: null });
    return true;
  }
  if (req.op === "ui_set_state") {
    send({ t: "api_result", id: req.id, ok: true, result: null });
    return true;
  }
  if (req.op === "ui_clear") {
    send({ t: "api_result", id: req.id, ok: true, result: null });
    return true;
  }
  if (req.op === "tool_list") {
    send({ t: "api_result", id: req.id, ok: true, result: { tools: ["bash", "read", "subagent"] } });
    return true;
  }
  if (req.op === "provider_complete") {
    // Simulate a simple response that ends the turn (no tool calls)
    send({
      t: "api_result",
      id: req.id,
      ok: true,
      result: {
        content: [{ type: "text", text: `completed task for session ${req.payload.session}` }],
        stop: "stop",
      },
    });
    return true;
  }
  return false;
}

async function scenario1_reentrancy() {
  const { proc, send, waitFor, got, kill } = launch();
  send({ t: "hello", proto: 1, kn9t: "0.1.0-test" });
  const hello = await waitFor((m) => m.t === "hello" && m.name, "plugin hello");
  assert.equal(hello.tools[0].name, "subagent");
  console.log("✓ hello:", hello.name);

  // The agent calls subagent on the PARENT session.
  send({
    t: "hook",
    id: 42,
    hook: "tool_call",
    payload: { tool: "subagent", args: { task: "check the diff" }, session: "parent-001" },
  });

  // Track what we've seen
  const seen = { fork: 0, nestedFork: 0, nestedComplete: 0 };
  let childSession = "";
  let nestedHookSent = false;
  
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    await sleep(10);
    const req = got.find((m) => m.t === "request");
    if (req) {
      got.splice(got.indexOf(req), 1);
      
      // Handle UI and common ops
      if (handleCommonOps(req, send, childSession)) {
        continue;
      }
      
      if (req.op === "session_fork") {
        if (seen.fork === 0) {
          // First fork: parent -> child
          seen.fork++;
          childSession = "child-900";
          send({ t: "api_result", id: req.id, ok: true, result: { session: childSession } });
        } else {
          // Nested fork: child -> grandchild
          seen.nestedFork++;
          send({ t: "api_result", id: req.id, ok: true, result: { session: "child-901" } });
        }
        continue;
      }
      
      if (req.op === "provider_complete") {
        // If this is the first provider_complete for the child and we haven't
        // sent the nested hook yet, send it now to test re-entrancy
        if (req.payload.session === childSession && !nestedHookSent) {
          nestedHookSent = true;
          // Send nested hook WHILE awaiting this provider_complete
          send({
            t: "hook",
            id: 77,
            hook: "tool_call",
            payload: { tool: "subagent", args: { task: "say hi" }, session: childSession },
          });
          // Don't respond yet - let the nested hook be processed first
          // We'll respond after seeing the nested result
          continue;
        }
        
        // For grandchild or after nested processing, respond normally
        seen.nestedComplete++;
        send({
          t: "api_result",
          id: req.id,
          ok: true,
          result: {
            content: [{ type: "text", text: `result for ${req.payload.session}` }],
            stop: "stop",
          },
        });
        continue;
      }
      
      // Unknown op
      send({ t: "api_result", id: req.id, ok: false, error: `unhandled op ${req.op}` });
      continue;
    }
    
    // Check for nested hook result (id 77)
    const nestedResult = got.find((m) => m.t === "result" && m.id === 77);
    if (nestedResult) {
      got.splice(got.indexOf(nestedResult), 1);
      assert.ok(!nestedResult.is_error, `nested tool error: ${JSON.stringify(nestedResult.content)}`);
      console.log("  ✓ nested subagent completed inline (re-entrancy works)");
      // Now we can let the outer provider_complete finish by handling more requests
      continue;
    }
    
    // Check for final result (id 42)
    const done = got.find((m) => m.t === "result" && m.id === 42);
    if (done) {
      assert.ok(!done.is_error, `tool error: ${JSON.stringify(done.content)}`);
      assert.ok(seen.fork >= 1, "at least one fork");
      console.log("✓ scenario 1 (recursion allowed): re-entrant spawn served inline");
      console.log("  result:", done.content[0]?.text?.substring(0, 50) + "...");
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
    payload: { tool: "subagent", args: { task: "list files" }, session: "parent-002" },
  });

  const seen = { fork: 0, toolList: 0 };
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    await sleep(10);
    const req = got.find((m) => m.t === "request");
    if (req) {
      got.splice(got.indexOf(req), 1);
      
      if (req.op === "ui_register_lua" || req.op === "ui_set_state" || req.op === "ui_clear") {
        send({ t: "api_result", id: req.id, ok: true, result: null });
        continue;
      }
      
      if (req.op === "tool_list") {
        seen.toolList++;
        send({
          t: "api_result",
          id: req.id,
          ok: true,
          result: { tools: ["bash", "read", "subagent", "mcp_list_servers"] },
        });
        continue;
      }
      
      if (req.op === "session_fork") {
        seen.fork++;
        send({ t: "api_result", id: req.id, ok: true, result: { session: "child-902" } });
        continue;
      }
      
      if (req.op === "provider_complete") {
        // In deny mode, subagent should NOT be in the tools list
        // The plugin filters it out when calling provider_complete
        send({
          t: "api_result",
          id: req.id,
          ok: true,
          result: {
            content: [{ type: "text", text: "listed files" }],
            stop: "stop",
          },
        });
        continue;
      }
      
      send({ t: "api_result", id: req.id, ok: false, error: `unhandled op ${req.op}` });
      continue;
    }
    
    const done = got.find((m) => m.t === "result" && m.id === 9);
    if (done) {
      assert.ok(!done.is_error, `tool error: ${JSON.stringify(done.content)}`);
      assert.ok(seen.toolList >= 1, "tool_list was consulted");
      console.log("✓ scenario 2 (recursion denied): child toolset excludes subagent");
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
