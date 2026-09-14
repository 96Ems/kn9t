/**
 * kn9t-subagent — spawn sub-agent sessions using kn9t primitives.
 *
 * This plugin provides the `subagent` tool which:
 * 1. Calls `session_fork` or `session_create` (host primitives)
 * 2. Calls `session_prompt` (host primitive) to run kn9t's ReAct loop
 *
 * Two modes:
 * - Fork (default): inherits parent transcript for context-aware work
 * - Fresh: creates independent session for isolated tasks
 *
 * NO custom agent loop — uses kn9t's native ReAct via session_prompt.
 */

import * as fs from "node:fs";

// ── Wire Protocol ────────────────────────────────────────────────────────────

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
  t: "api_result";
  id: number;
  ok: boolean;
  result?: Record<string, unknown>;
  error?: string;
}

interface HookMsg {
  t: "hook";
  id: number;
  hook: string;
  payload: Record<string, unknown>;
}

const reader = new LineReader();
const replies = new Map<number, ApiResult>();
let requestId = 1000;

// ── Host API Client ──────────────────────────────────────────────────────────

/**
 * Event pump: read lines until reply for `awaitId` arrives.
 * Handles incoming hooks inline (supports recursive subagents).
 */
function pumpUntil(awaitId: number): ApiResult {
  for (;;) {
    const hit = replies.get(awaitId);
    if (hit !== undefined) {
      replies.delete(awaitId);
      return hit;
    }
    const line = reader.readLine();
    if (line === null) throw new Error("host closed stdin");
    const msg = JSON.parse(line) as { t?: string; id?: number } & Record<string, unknown>;

    if (msg.t === "api_result" && typeof msg.id === "number") {
      replies.set(msg.id, msg as unknown as ApiResult);
      continue;
    }
    if (msg.t === "hook" && typeof msg.id === "number") {
      handleHook(msg.id, msg as unknown as HookMsg);
      continue;
    }
    if (msg.t === "shutdown") throw new Error("host shutdown");
    // Ignore other messages (events, etc.)
  }
}

/** Call a host API operation and wait for result. */
function hostCall(op: string, payload: Record<string, unknown>): ApiResult {
  const id = requestId++;
  writeMsg({ t: "request", id, op, payload });
  return pumpUntil(id);
}

// ── Progress ─────────────────────────────────────────────────────────────────

function sendProgress(hookId: number, text: string): void {
  writeMsg({ t: "chunk", id: hookId, body: { text } });
}

// ── Subagent Execution ───────────────────────────────────────────────────────

interface SubagentArgs {
  task: string;
  model?: string;
  budget_usd?: number;
  tools?: string[];
  fresh?: boolean;
  timeout_s?: number;
}

function executeSubagent(
  args: SubagentArgs,
  session: string,
  hookId: number
): { content: Array<{ type: string; text: string }>; is_error: boolean } {
  const {
    task,
    model,
    budget_usd = 0.5,
    tools,
    fresh = false,
    timeout_s = 600,
  } = args;

  if (!task || task.trim() === "") {
    return {
      content: [{ type: "text", text: 'subagent requires non-empty "task"' }],
      is_error: true,
    };
  }

  sendProgress(hookId, `spawning subagent: ${truncate(task, 60)}...`);

  // Step 1: Create child session
  let childSession: string;

  if (fresh) {
    // Fresh session: no parent context
    const payload: Record<string, unknown> = {};
    if (model) payload.model = model;

    const res = hostCall("session_create", payload);
    if (!res.ok) {
      return {
        content: [{ type: "text", text: `session_create failed: ${res.error}` }],
        is_error: true,
      };
    }
    childSession = res.result?.session as string;
  } else {
    // Fork session: inherits parent transcript
    const payload: Record<string, unknown> = {
      session,
      copy_events: true,
      budget_usd,
    };
    if (model) payload.model = model;

    const res = hostCall("session_fork", payload);
    if (!res.ok) {
      return {
        content: [{ type: "text", text: `session_fork failed: ${res.error}` }],
        is_error: true,
      };
    }
    childSession = res.result?.session as string;
  }

  const shortId = childSession.substring(0, 8);
  sendProgress(hookId, `subagent session ${shortId}`);

  // Step 2: Run the task via session_prompt (uses kn9t's native ReAct loop)
  const promptPayload: Record<string, unknown> = {
    session: childSession,
    text: task,
    timeout_s,
  };
  if (tools && tools.length > 0) {
    promptPayload.tools = tools;
  }

  const promptRes = hostCall("session_prompt", promptPayload);
  if (!promptRes.ok) {
    return {
      content: [{ type: "text", text: `session_prompt failed: ${promptRes.error}` }],
      is_error: true,
    };
  }

  const result = (promptRes.result?.result as string) || "";
  sendProgress(hookId, "subagent complete");

  return {
    content: [
      { type: "text", text: result },
      {
        type: "text",
        text: `\n\n───────────────────────────────────────\n📎 Sub-agent session: ${childSession}\n   View with: kn9t attach ${shortId}`,
      },
    ],
    is_error: false,
  };
}

function truncate(s: string, max: number): string {
  if (s.length <= max) return s;
  return s.substring(0, max - 1) + "…";
}

// ── Hook Handler ─────────────────────────────────────────────────────────────

function handleHook(id: number, msg: HookMsg): void {
  if (msg.hook !== "tool_call") {
    writeMsg({ t: "done", id, content: [], is_error: true });
    return;
  }

  const payload = msg.payload;
  const tool = payload.tool as string;
  const args = (payload.args as Record<string, unknown>) || {};
  const session = payload.session as string;

  if (tool !== "subagent") {
    writeMsg({
      t: "done",
      id,
      content: [{ type: "text", text: `unknown tool: ${tool}` }],
      is_error: true,
    });
    return;
  }

  const result = executeSubagent(args as unknown as SubagentArgs, session, id);
  writeMsg({ t: "done", id, ...result });
}

// ── Main ─────────────────────────────────────────────────────────────────────

function main(): void {
  // Handshake
  const helloLine = reader.readLine();
  if (!helloLine) process.exit(1);

  const hello = JSON.parse(helloLine) as { t?: string; kn9t?: string };
  if (hello.t !== "hello") {
    console.error("kn9t-subagent: expected host hello");
    process.exit(1);
  }

  console.error(`kn9t-subagent: connected to kn9t ${hello.kn9t ?? "?"}`);

  writeMsg({
    t: "hello",
    name: "kn9t-subagent",
    capabilities: ["host_api", "streaming"],
    tools: [
      {
        name: "subagent",
        description:
          "Spawn a sub-agent session (a forked kn9t session, R-PLUG-110): it inherits " +
          "the current transcript, runs the task synchronously as its own turn, and " +
          "returns the result plus the child session id.",
        schema: {
          type: "object",
          properties: {
            task: {
              type: "string",
              description: "Task for the sub-agent session.",
            },
            model: {
              type: "string",
              description: "Optional model id (default: parent model).",
            },
            budget_usd: {
              type: "number",
              description: "Optional spend cap (default 0.5 USD).",
            },
            tools: {
              type: "array",
              items: { type: "string" },
              description: "Optional tool subset for the child (default: inherit).",
            },
            fresh: {
              type: "boolean",
              description:
                "If true, create a fresh session without parent context (default: false, inherits transcript).",
            },
            timeout_s: {
              type: "integer",
              description: "Timeout in seconds (default: 600).",
            },
          },
          required: ["task"],
        },
        parallel_safe: false,
      },
    ],
    hooks: [],
    events: [],
  });

  // Main loop
  while (true) {
    const line = reader.readLine();
    if (line === null) break;

    const msg = JSON.parse(line) as { t?: string; id?: number } & Record<string, unknown>;

    if (msg.t === "shutdown") break;

    if (msg.t === "hook" && typeof msg.id === "number") {
      handleHook(msg.id, msg as unknown as HookMsg);
    }
    // api_result without a waiter: ignore (stale)
  }
}

try {
  main();
} catch (e) {
  console.error("kn9t-subagent: fatal:", e);
  process.exit(1);
}
