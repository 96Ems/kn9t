/**
 * kn9t-subagent — pilot plugin for the session-based sub-agent primitives.
 *
 * A "sub-agent" is NOT a kn9t concept: it is a forked session running a turn
 * (fork_reason=subagent, budget captured in the ForkSnapshot). This plugin
 * exposes a `spawn_session` tool that the main agent can call:
 *
 *   1. session_fork   {copy_events:true, budget_usd, model} → child session
 *                     (inherits the parent transcript → full context)
 *   2. session_prompt {session: child, text: task}           → the child runs
 *                     its own ReAct turn with its own model/tools/hooks,
 *                     usage recorded in the CHILD session under its own budget
 *
 * Recursion is ALLOWED (a sub-agent may spawn sub-agents — legitimate task
 * decomposition). This plugin MUST therefore service incoming hooks WHILE a
 * host request is pending: the single-threaded client uses an event pump —
 * one line reader, a shared reply buffer keyed by id, and inline hook
 * dispatch — mirroring the host's reader-thread + per-call demux. Without it,
 * a child's tool call arriving mid-wait would deadlock the whole chain.
 *
 * Everything goes through the host_api RPC — no keys, no HTTP.
 */
import * as fs from "node:fs";

// ── stdio NdJSON ────────────────────────────────────────────────────────────

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

interface HookMsg {
  t: string;
  id: number;
  payload: Record<string, unknown>;
}

let requestId = 1000;
const reader = new LineReader();

/** Replies that arrived for requests we were not pumping at that moment
 *  (e.g. an outer request completing while an inner pump runs). */
const replies = new Map<number, ApiResult>();

/** Default spend cap per spawned session when the caller gives none — the
 *  recursion safety net (a chain of children sharing one budget dies out). */
const DEFAULT_BUDGET_USD = 0.5;
const SPAWN_TOOL = "spawn_session";

/**
 * Recursion policy — an END-USER/plugin choice, not a host rule:
 * `KN9T_SUBAGENT_RECURSION` env (inherited by the plugin process):
 *   - unset or "allow" (default): a sub-agent may spawn sub-agents
 *     (the child inherits the full toolset, spawn_session included);
 *   - "deny": the child's toolset is computed via `tool_list` minus
 *     spawn_session — a sub-agent cannot spawn further sub-agents.
 */
const RECURSION_ALLOWED =
  (process.env["KN9T_SUBAGENT_RECURSION"] ?? "allow").toLowerCase() !== "deny";

/** Child toolset when recursion is denied: everything minus spawn_session. */
function noSpawnToolset(session: string): Array<string> | undefined {
  const r = hostRequest("tool_list", { session });
  if (!r.ok || !Array.isArray(r.result?.["tools"])) return undefined;
  return (r.result!["tools"] as Array<string>).filter((n) => n !== SPAWN_TOOL);
}

/**
 * Event pump: read lines until the reply for `awaitId` arrives. Incoming
 * hooks are dispatched INLINE (recursive spawn_session is served while we
 * wait — this is what makes re-entrancy/deadlock-free recursion possible);
 * api_results are buffered by id so a reply for an outer request is never
 * lost to an inner pump.
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
      handleHook(msg.id, (msg.payload as Record<string, unknown>) ?? {});
      continue;
    }
    if (msg.t === "shutdown") throw new Error("host shutdown during request");
    // Events and anything else: fire-and-forget, drop.
  }
}

/** Send a plugin → host API request and await the api_result reply. */
function hostRequest(op: string, payload: unknown): ApiResult {
  const id = requestId++;
  writeMsg({ t: "request", id, op, payload });
  return pumpUntil(id);
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
 *
 * Soft anti-delegation nudge: the child is told to complete the task itself
 * (it MAY still spawn, but pointless delegation is discouraged — the child
 * otherwise tends to mimic spawn-heavy parent transcripts).
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
  const budget = typeof args["budget_usd"] === "number" ? args["budget_usd"] : DEFAULT_BUDGET_USD;
  // Recursion policy is this plugin's (end-user's) choice: explicit tools win;
  // otherwise inherit the parent toolset, except when recursion is denied.
  const tools = Array.isArray(args["tools"])
    ? (args["tools"] as unknown[]).filter((t): t is string => typeof t === "string")
    : RECURSION_ALLOWED
      ? undefined
      : noSpawnToolset(session);

  // 1. Fork: the child inherits the parent transcript (full context) and the
  //    budget is captured in the ForkSnapshot (R-PLUG-130).
  const fork = hostRequest("session_fork", { session, copy_events: true, budget_usd: budget, model });
  if (!fork.ok) return { content: [{ type: "text", text: `session_fork: ${fork.error}` }], is_error: true };
  const child = String(fork.result?.["session"] ?? "");

  // 2. Prompt: the child runs its own turn (model at fork, tool subset, hooks).
  const childTask = `You are a sub-agent session. Complete this task yourself: ${task}`;
  const prompt = hostRequest("session_prompt", { session: child, text: childTask, tools });
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

function handleHook(id: number, payload: Record<string, unknown>): void {
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
        name: SPAWN_TOOL,
        description:
          "Spawn a sub-agent session (a forked kn9t session, R-PLUG-110): it inherits " +
          "the current transcript, runs the task synchronously as its own turn, and " +
          "returns the result plus the child session id.",
        schema: {
          type: "object",
          properties: {
            task: { type: "string", description: "Task for the sub-agent session." },
            model: { type: "string", description: "Optional model id (default: parent model)." },
            budget_usd: { type: "number", description: `Optional spend cap (default ${DEFAULT_BUDGET_USD} USD).` },
            tools: { type: "array", items: { type: "string" }, description: "Optional tool subset for the child (default: inherit)." },
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
    const msg = JSON.parse(line) as { t?: string; id?: number } & Record<string, unknown>;
    if (msg.t === "shutdown") break;
    if (msg.t === "hook" && typeof msg.id === "number") {
      handleHook(msg.id, (msg.payload as Record<string, unknown>) ?? {});
    } else if (msg.t === "hook") {
      writeMsg({ t: "result", id: (msg.id as number) ?? 0, error: `kn9t-subagent: unhandled hook` });
    }
    // api_result without a waiter: ignore (stale).
  }
}

try {
  main();
} catch (e) {
  console.error("kn9t-subagent: fatal:", e);
  process.exit(1);
}