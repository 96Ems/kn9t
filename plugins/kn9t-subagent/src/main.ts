/**
 * kn9t-subagent — pilot plugin for the session-based sub-agent primitives.
 *
 * A "sub-agent" is NOT a kn9t concept: it is a forked session running a turn
 * (fork_reason=subagent, budget captured in the ForkSnapshot). This plugin
 * proves the open primitives by exposing a `spawn_session` tool that the main
 * agent can call:
 *
 *   1. session_fork   {copy_events:true, budget_usd, model} → child session
 *                     (inherits the parent transcript → full context)
 *   2. session_prompt {session: child, text: task}           → the child runs
 *                     its own ReAct turn with its own model/tools/hooks,
 *                     usage recorded in the CHILD session under its own budget
 *
 * The child transcript stays inspectable (session_read / TUI picker).
 * Everything goes through the host_api RPC — no keys, no HTTP.
 */
import * as fs from "node:fs";

// ── stdio NdJSON (sequential: this plugin only hears hello / tool_call / shutdown) ──

class LineReader {
  private buf = Buffer.alloc(0);
  readLine(): string | null {
    while (true) {
      const nl = this.buf.indexOf(0x0a);
      if (nl >= 0) {
        const line = this.buf.subarray(0, nl).toString("utf8");
        this.buf = this.buf.subarray(nl + 1);
        return line;
      }
      const chunk = Buffer.alloc(65536);
      const n = fs.readSync(0, chunk, 0, chunk.length, null);
      if (n <= 0) return null;
      this.buf = Buffer.concat([this.buf, chunk.subarray(0, n)]);
    }
  }
}

function writeMsg(msg: unknown): void {
  fs.writeSync(1, JSON.stringify(msg) + "\n");
}

interface ApiResult {
  t: string;
  id: number;
  ok: boolean;
  result?: Record<string, unknown>;
  error?: string;
}

let requestId = 1000;
const reader = new LineReader();

/** Send a plugin → host API request and await the api_result reply. */
function hostRequest(op: string, payload: unknown): ApiResult {
  const id = requestId++;
  writeMsg({ t: "request", id, op, payload });
  while (true) {
    const line = reader.readLine();
    if (line === null) throw new Error("host closed stdin");
    const msg = JSON.parse(line) as ApiResult;
    if (msg.t === "api_result" && msg.id === id) return msg;
    // Ignore anything else (forward compatibility).
  }
}

function textBlocks(content: unknown): string {
  const arr = Array.isArray(content) ? (content as Array<Record<string, unknown>>) : [];
  return arr
    .filter((b) => b["type"] === "text" && typeof b["text"] === "string")
    .map((b) => String(b["text"]))
    .join("\n");
}

/**
 * Handle one `spawn_session` tool call: fork a child session and run the task
 * synchronously inside it. The result returns to the calling agent together
 * with the child session id (so it can be inspected afterwards).
 */
function spawnSession(args: Record<string, unknown>, session: string): {
  content: Array<Record<string, unknown>>;
  is_error: boolean;
} {
  const task = typeof args["task"] === "string" ? args["task"] : null;
  if (!task) {
    return { content: [{ type: "text", text: "spawn_session requires \"task\"" }], is_error: true };
  }
  const model = typeof args["model"] === "string" ? args["model"] : undefined;
  const budget = typeof args["budget_usd"] === "number" ? args["budget_usd"] : undefined;
  const tools = Array.isArray(args["tools"])
    ? (args["tools"] as unknown[]).filter((t): t is string => typeof t === "string")
    : undefined;

  // 1. Fork: the child inherits the parent transcript (full context) and the
  //    budget is captured in the ForkSnapshot (R-PLUG-130).
  const fork = hostRequest("session_fork", { session, copy_events: true, budget_usd: budget, model });
  if (!fork.ok) return { content: [{ type: "text", text: `session_fork: ${fork.error}` }], is_error: true };
  const child = String(fork.result?.["session"] ?? "");

  // 2. Prompt: the child runs its own turn (model at fork, tool subset, hooks).
  const prompt = hostRequest("session_prompt", { session: child, text: task, tools });
  if (!prompt.ok) return { content: [{ type: "text", text: `session_prompt: ${prompt.error}` }], is_error: true };

  const raw = prompt.result?.["result"];
  const result = typeof raw === "string" ? raw : textBlocks(raw);
  return {
    content: [
      {
        type: "text",
        text: `[sub-agent session ${child}] ${result}`,
      },
    ],
    is_error: false,
  };
}

function main(): void {
  const hello = reader.readLine();
  if (hello === null) process.exit(1);
  const helloMsg = JSON.parse(hello) as { t?: string; kn9t?: string };
  if (helloMsg.t !== "hello") {
    console.error("kn9t-subagent: expected host hello, got:", hello);
    process.exit(1);
  }
  console.error(`kn9t-subagent: connected to kn9t ${helloMsg.kn9t ?? "?"} (host_api)`);
  writeMsg({
    t: "hello",
    name: "kn9t-subagent",
    capabilities: ["host_api"],
    tools: [
      {
        name: "spawn_session",
        description:
          "Spawn a sub-agent session (a forked kn9t session, R-PLUG-110): it inherits " +
          "the current transcript, runs the task synchronously as its own turn, and " +
          "returns the result plus the child session id.",
        schema: {
          type: "object",
          properties: {
            task: { type: "string", description: "Task for the sub-agent session." },
            model: { type: "string", description: "Optional model id (default: parent model)." },
            budget_usd: { type: "number", description: "Optional spend cap for the child." },
            tools: { type: "array", items: { type: "string" }, description: "Optional tool subset for the child." },
          },
          required: ["task"],
        },
        parallel_safe: false,
      },
    ],
  });

  while (true) {
    const line = reader.readLine();
    if (line === null) break;
    const msg = JSON.parse(line) as {
      t?: string;
      id?: number;
      hook?: string;
      payload?: Record<string, unknown>;
    };
    if (msg.t === "shutdown") break;
    if (msg.t === "hook" && msg.hook === "tool_call") {
      const id = msg.id ?? 0;
      const payload = msg.payload ?? {};
      const name = String(payload["tool"] ?? ""); // canonical tool_call field (SDK contract)
      const args = (payload["args"] as Record<string, unknown>) ?? {};
      const session = String(payload["session"] ?? ""); // added by the host (96E-17)
      if (name === "spawn_session") {
        const out = spawnSession(args, session);
        writeMsg({ t: "result", id, content: out.content, is_error: out.is_error });
      } else {
        writeMsg({
          t: "result",
          id,
          content: [{ type: "text", text: `kn9t-subagent: unknown tool ${name}` }],
          is_error: true,
        });
      }
    } else {
      writeMsg({ t: "result", id: msg.id ?? 0, error: `kn9t-subagent: unhandled hook ${msg.hook ?? "?"}` });
    }
  }
}

try {
  main();
} catch (e) {
  console.error("kn9t-subagent: fatal:", e);
  process.exit(1);
}