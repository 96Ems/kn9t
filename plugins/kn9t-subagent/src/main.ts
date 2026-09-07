/**
 * kn9t-subagent — pilot plugin for the session-based sub-agent primitives.
 *
 * A "sub-agent" is NOT a kn9t concept: it is a forked session running a turn
 * (fork_reason=subagent, budget captured in the ForkSnapshot). This plugin
 * exposes a `subagent` tool that the main agent can call:
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
const SPAWN_TOOL = "subagent";

/**
 * Recursion policy — an END-USER/plugin choice, not a host rule:
 * `KN9T_SUBAGENT_RECURSION` env (inherited by the plugin process):
 *   - unset or "allow" (default): a sub-agent may spawn sub-agents
 *     (the child inherits the full toolset, subagent included);
 *   - "deny": the child's toolset is computed via `tool_list` minus
 *     subagent — a sub-agent cannot spawn further sub-agents.
 */
const RECURSION_ALLOWED =
  (process.env["KN9T_SUBAGENT_RECURSION"] ?? "allow").toLowerCase() !== "deny";

/** Child toolset when recursion is denied: everything minus subagent. */
function noSpawnToolset(session: string): Array<string> | undefined {
  const r = hostRequest("tool_list", { session });
  if (!r.ok || !Array.isArray(r.result?.["tools"])) return undefined;
  return (r.result!["tools"] as Array<string>).filter((n) => n !== SPAWN_TOOL);
}

/**
 * Event pump: read lines until the reply for `awaitId` arrives. Incoming
 * hooks are dispatched INLINE (recursive subagent is served while we
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

/** Lua source for the subagent TUI widget — live transcript viewer.
 * 
 * State structure:
 * {
 *   status: "forking" | "running" | "complete" | "error",
 *   session_id: string,
 *   task: string,
 *   transcript: [
 *     { role: "assistant", text: "I'll read..." },
 *     { role: "tool", name: "read", status: "running" | "complete" | "error", summary?: string },
 *     { role: "assistant", text: "I see the issue..." },
 *   ],
 *   error?: string
 * }
 */
const SUBAGENT_LUA = `
function render(state)
  local status = state.status or "unknown"
  local session_id = state.session_id or ""
  local task = state.task or ""
  local transcript = state.transcript or {}
  local short_id = session_id:sub(1, 8)
  
  -- Status styling
  local status_color = "gray"
  local status_icon = "○"
  if status == "forking" then
    status_color = "blue"
    status_icon = "◔"
  elseif status == "running" then
    status_color = "yellow"
    status_icon = "↻"
  elseif status == "complete" then
    status_color = "green"
    status_icon = "✓"
  elseif status == "error" then
    status_color = "red"
    status_icon = "✕"
  end
  
  -- Build header
  local header = status_icon .. " " .. status
  if short_id ~= "" then
    header = header .. "  │  " .. short_id
  end
  
  -- Build transcript items for list widget
  local items = {}
  for i, entry in ipairs(transcript) do
    if entry.role == "assistant" then
      -- Assistant text: just show it
      local text = entry.text or ""
      if #text > 80 then
        text = text:sub(1, 77) .. "..."
      end
      table.insert(items, { text = text, fg = "white" })
    elseif entry.role == "tool" then
      -- Tool call: show as mini-card
      local tool_icon = "○"
      local tool_color = "gray"
      if entry.status == "running" then
        tool_icon = "↻"
        tool_color = "yellow"
      elseif entry.status == "complete" then
        tool_icon = "✓"
        tool_color = "green"
      elseif entry.status == "error" then
        tool_icon = "✕"
        tool_color = "red"
      end
      local tool_line = "  " .. tool_icon .. " " .. (entry.name or "?")
      if entry.summary then
        tool_line = tool_line .. ": " .. entry.summary
      end
      table.insert(items, { text = tool_line, fg = tool_color })
    elseif entry.role == "thinking" then
      -- Thinking: dimmed
      local text = entry.text or ""
      if #text > 60 then
        text = text:sub(1, 57) .. "..."
      end
      table.insert(items, { text = "💭 " .. text, fg = "gray" })
    end
  end
  
  -- If no transcript yet, show task
  if #items == 0 then
    local display_task = task
    if #task > 60 then
      display_task = task:sub(1, 57) .. "..."
    end
    table.insert(items, { text = display_task, fg = "gray" })
  end
  
  return {
    type = "box",
    border = "rounded",
    title = "⚡ Sub-agent",
    border_fg = status_color,
    padding = { 0, 1 },
    child = {
      type = "split",
      direction = "vertical",
      children = {
        {
          type = "text",
          content = header,
          fg = status_color,
        },
        {
          type = "list",
          items = items,
        },
      },
      sizes = { 1, "fill" },
    },
  }
end
`;

/** Register the Lua UI widget for this plugin (once per session). */
const registeredSessions = new Set<string>();

function ensureLuaRegistered(session: string): void {
  if (registeredSessions.has(session)) return;
  hostRequest("ui_register_lua", { session, source: SUBAGENT_LUA });
  registeredSessions.add(session);
}

/** Push UI state update to TUI. */
function uiSetState(session: string, state: SubagentState): void {
  hostRequest("ui_set_state", { session, state: state as unknown as Record<string, unknown> });
}

/** Clear the UI widget when done. */
function uiClear(session: string): void {
  hostRequest("ui_clear", { session });
}

// ── Transcript types for UI ────────────────────────────────────────────────

interface TranscriptEntry {
  role: "assistant" | "tool" | "thinking";
  text?: string;
  name?: string;
  status?: "running" | "complete" | "error";
  summary?: string;
  [key: string]: unknown;  // Index signature for JSON serialization
}

interface SubagentState {
  status: "forking" | "running" | "complete" | "error";
  session_id: string;
  task: string;
  transcript: TranscriptEntry[];
  error?: string;
  [key: string]: unknown;  // Index signature for JSON serialization
}

// ── Agent loop types ───────────────────────────────────────────────────────

interface Message {
  role: string;
  content: ContentBlock[];
}

interface ContentBlock {
  type: string;
  text?: string;
  id?: string;
  name?: string;
  args_json?: string;
  content?: ContentBlock[];
  is_error?: boolean;
}

interface ToolSpec {
  name: string;
  description: string;
  schema: Record<string, unknown>;
}

// ── Mini agent loop (runs in child session, streams to parent UI) ──────────

const MAX_TURNS = 20;  // Safety limit

function runAgentLoop(
  childSession: string,
  parentSession: string,
  task: string,
  toolNames: string[] | undefined,
  state: SubagentState
): { result: string; is_error: boolean } {
  
  // Get available tools
  const toolListRes = hostRequest("tool_list", { session: childSession });
  const allTools: string[] = toolListRes.ok && Array.isArray(toolListRes.result?.["tools"]) 
    ? toolListRes.result!["tools"] as string[]
    : [];
  
  // Filter to requested tools (or all if not specified, minus subagent if recursion denied)
  let activeTools = toolNames ?? allTools;
  if (!RECURSION_ALLOWED) {
    activeTools = activeTools.filter(t => t !== SPAWN_TOOL);
  }

  // Build messages array - start with the task
  const messages: Message[] = [
    { role: "user", content: [{ type: "text", text: `You are a sub-agent. Complete this task yourself: ${task}` }] }
  ];

  let finalResult = "";
  let turns = 0;

  while (turns < MAX_TURNS) {
    turns++;

    // Call provider_complete
    const completeRes = hostRequest("provider_complete", {
      session: childSession,
      messages,
      tools: activeTools,
    });

    if (!completeRes.ok) {
      state.status = "error";
      state.error = completeRes.error;
      uiSetState(parentSession, state);
      return { result: `provider_complete failed: ${completeRes.error}`, is_error: true };
    }

    const content = completeRes.result?.["content"] as ContentBlock[] ?? [];
    const stop = completeRes.result?.["stop"] as string ?? "stop";

    // Process response content
    const assistantContent: ContentBlock[] = [];
    const toolCalls: ContentBlock[] = [];
    let assistantText = "";

    for (const block of content) {
      if (block.type === "text" && block.text) {
        assistantText += block.text;
        assistantContent.push(block);
        // Update transcript with assistant text
        state.transcript.push({ role: "assistant", text: block.text });
        uiSetState(parentSession, state);
      } else if (block.type === "thinking" && block.text) {
        assistantContent.push(block);
        state.transcript.push({ role: "thinking", text: block.text });
        uiSetState(parentSession, state);
      } else if (block.type === "tool_call" || block.type === "tool_use") {
        toolCalls.push(block);
        assistantContent.push(block);
      }
    }

    // Add assistant message to history
    messages.push({ role: "assistant", content: assistantContent });

    // If no tool calls, we're done
    if (toolCalls.length === 0 || stop === "stop") {
      finalResult = assistantText;
      break;
    }

    // Execute tool calls
    const toolResults: ContentBlock[] = [];
    
    for (const call of toolCalls) {
      const toolName = call.name ?? "unknown";
      const callId = call.id ?? `call-${Date.now()}`;
      let args: Record<string, unknown> = {};
      
      try {
        args = call.args_json ? JSON.parse(call.args_json) : {};
      } catch {
        args = {};
      }

      // Show tool as running in transcript
      const toolEntry: TranscriptEntry = { 
        role: "tool", 
        name: toolName, 
        status: "running",
        summary: summarizeToolArgs(toolName, args)
      };
      state.transcript.push(toolEntry);
      uiSetState(parentSession, state);

      // Execute the tool
      const execRes = hostRequest("tool_execute", {
        session: childSession,
        name: toolName,
        args,
      });

      // Update tool status
      const toolIdx = state.transcript.length - 1;
      const toolEntryRef = state.transcript[toolIdx];
      if (execRes.ok) {
        const toolContent = execRes.result?.["content"] as ContentBlock[] ?? [];
        const isError = execRes.result?.["is_error"] as boolean ?? false;
        
        if (toolEntryRef) {
          toolEntryRef.status = isError ? "error" : "complete";
          toolEntryRef.summary = summarizeToolResult(toolName, toolContent);
        }
        uiSetState(parentSession, state);

        toolResults.push({
          type: "tool_result",
          id: callId,
          content: toolContent,
          is_error: isError,
        });
      } else {
        if (toolEntryRef) {
          toolEntryRef.status = "error";
          toolEntryRef.summary = execRes.error ?? "failed";
        }
        uiSetState(parentSession, state);

        toolResults.push({
          type: "tool_result", 
          id: callId,
          content: [{ type: "text", text: execRes.error ?? "tool execution failed" }],
          is_error: true,
        });
      }
    }

    // Add tool results to messages
    messages.push({ role: "tool", content: toolResults });
  }

  if (turns >= MAX_TURNS) {
    return { result: `Reached maximum turns (${MAX_TURNS}). Last output: ${finalResult}`, is_error: true };
  }

  return { result: finalResult, is_error: false };
}

/** Create a short summary of tool arguments for display. */
function summarizeToolArgs(name: string, args: Record<string, unknown>): string {
  if (name === "read" || name === "write" || name === "edit") {
    const path = args["path"];
    return typeof path === "string" ? path : "";
  }
  if (name === "bash") {
    const cmd = args["cmd"];
    if (typeof cmd === "string") {
      return cmd.length > 40 ? cmd.substring(0, 37) + "..." : cmd;
    }
  }
  if (name === "subagent") {
    const task = args["task"];
    if (typeof task === "string") {
      return task.length > 40 ? task.substring(0, 37) + "..." : task;
    }
  }
  return "";
}

/** Create a short summary of tool result for display. */
function summarizeToolResult(name: string, content: ContentBlock[]): string {
  const text = content
    .filter(b => b.type === "text" && b.text)
    .map(b => b.text!)
    .join(" ");
  
  if (name === "read") {
    const match = text.match(/\((\d+) lines?\)/);
    return match ? `${match[1]} lines` : "done";
  }
  if (name === "edit" || name === "write") {
    return "done";
  }
  if (name === "bash") {
    if (text.includes("error") || text.includes("Error")) return "error";
    return text.length > 30 ? text.substring(0, 27) + "..." : (text || "done");
  }
  
  return text.length > 30 ? text.substring(0, 27) + "..." : (text || "done");
}

/**
 * Handle one `subagent` tool call: fork a child session and run the task
 * with a streaming mini-agent loop that updates the UI after each step.
 */
function spawnSession(args: Record<string, unknown>, session: string): {
  content: Array<Record<string, unknown>>;
  is_error: boolean;
} {
  const task = typeof args["task"] === "string" ? args["task"] : null;
  if (!task) {
    return { content: [{ type: "text", text: "subagent requires \"task\"" }], is_error: true };
  }
  const model = typeof args["model"] === "string" ? args["model"] : undefined;
  const budget = typeof args["budget_usd"] === "number" ? args["budget_usd"] : DEFAULT_BUDGET_USD;
  const tools = Array.isArray(args["tools"])
    ? (args["tools"] as unknown[]).filter((t): t is string => typeof t === "string")
    : undefined;

  // Initialize UI state
  const state: SubagentState = {
    status: "forking",
    session_id: "",
    task,
    transcript: [],
  };

  // Register UI widget and show initial state
  ensureLuaRegistered(session);
  uiSetState(session, state);

  // 1. Fork the session
  const fork = hostRequest("session_fork", { session, copy_events: true, budget_usd: budget, model });
  if (!fork.ok) {
    state.status = "error";
    state.error = fork.error;
    uiSetState(session, state);
    return { content: [{ type: "text", text: `session_fork: ${fork.error}` }], is_error: true };
  }
  const child = String(fork.result?.["session"] ?? "");
  state.session_id = child;
  state.status = "running";
  uiSetState(session, state);

  // 2. Run the agent loop with streaming updates
  const { result, is_error } = runAgentLoop(child, session, task, tools, state);

  // 3. Update final status
  state.status = is_error ? "error" : "complete";
  uiSetState(session, state);

  // Return structured output
  return {
    content: [
      { type: "text", text: result },
      { type: "text", text: `\n\n───────────────────────────────────────\n📎 Sub-agent session: ${child}\n   View with: kn9t attach ${child.substring(0, 8)}` },
    ],
    is_error,
  };
}

function handleHook(id: number, payload: Record<string, unknown>): void {
  const name = String(payload["tool"] ?? ""); // canonical tool_call field (SDK contract)
  const args = (payload["args"] as Record<string, unknown>) ?? {};
  const session = String(payload["session"] ?? ""); // added by the host (96E-17)
  if (name === "subagent") {
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