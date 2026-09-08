/**
 * kn9t-compactor — an agent-style compaction plugin for kn9t.
 *
 * kn9t does NOT embed sub-agents: this plugin IS the sub-agent. It runs its
 * own two-pass LLM "agent turn" using the plugin → host API (host_api
 * capability) — the session's own model, credentials and usage accounting:
 *
 *   Pass 1 (triage):  session_read the span, build a per-CallId inventory,
 *                     and force the model to call the plugin's own
 *                     `submit_triage` tool (declared inline, schema-validated
 *                     by the provider) to pick keep / summarize / drop per tool
 *                     call ID (+ resume_actions). Hallucinated IDs are rejected
 *                     and the model gets one correction shot.
 *   Pass 2 (summary): force the model to call `submit_summary`; kept tool
 *                     results are copied VERBATIM into the summary message
 *                     by this plugin (byte-exact, never re-summarized).
 *
 * The compactor declares its own tools and system prompts: it is a self-
 * contained agent. Structured output comes from the provider's tool-call
 * contract, not from parsing free-form model prose — kn9t-core is untouched.
 *
 * Reply to the host's `compactor_compact` hook with the plan; the host still
 * validates every cited CallId (validate_handoff, host-side) before persisting
 * Event::Compacted + Event::Handoff.
 *
 * Wire: NdJSON over stdio (spec 08b §2). No config, no API keys: everything
 * goes through the host.
 */
import { Array, Effect } from "effect";
import * as fs from "node:fs";

// ── stdio NdJSON (sync — the plugin only ever hears hello / compactor_compact /
//    shutdown, so the stream is strictly sequential) ─────────────────────────

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
  result?: unknown;
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
    // Unknown host messages in between: ignore (forward compatibility).
  }
}

/** Send a request without waiting — returns the request ID. */
function hostRequestAsync(op: string, payload: unknown): number {
  const id = requestId++;
  writeMsg({ t: "request", id, op, payload });
  return id;
}

/** 
 * Wait for multiple api_results by ID. Calls onResult for each as they arrive.
 * Returns a map of id → result when all are received.
 */
function awaitResults(
  ids: number[],
  onResult?: (id: number, result: ApiResult) => void
): Map<number, ApiResult> {
  const pending = new Set(ids);
  const results = new Map<number, ApiResult>();
  
  while (pending.size > 0) {
    const line = reader.readLine();
    if (line === null) throw new Error("host closed stdin");
    const msg = JSON.parse(line) as ApiResult;
    if (msg.t === "api_result" && pending.has(msg.id)) {
      pending.delete(msg.id);
      results.set(msg.id, msg);
      if (onResult) onResult(msg.id, msg);
    }
    // Ignore other messages (forward compatibility)
  }
  
  return results;
}

// ── UI state for live compaction viewer ──────────────────────────────────────

interface CompactorState {
  status: "reading" | "triage" | "triage_retry" | "summary" | "complete" | "error";
  session_id: string;
  messages_count: number;
  tool_calls_count: number;
  decisions: Array<{ id: string; action: string; name?: string }>;
  triage_done: boolean;
  summary_preview: string;
  error?: string;
  [key: string]: unknown;
}

/** Lua source for the compactor TUI widget — live compaction viewer. */
const COMPACTOR_LUA = `
function render(state)
  local status = state.status or "unknown"
  local session_id = state.session_id or ""
  local messages_count = state.messages_count or 0
  local tool_calls_count = state.tool_calls_count or 0
  local decisions = state.decisions or {}
  local summary_preview = state.summary_preview or ""
  local triage_done = state.triage_done or false
  local short_id = session_id:sub(1, 8)
  
  -- Status styling
  local status_color = "gray"
  local status_icon = "○"
  local status_text = status
  if status == "reading" then
    status_color = "blue"
    status_icon = "◔"
    status_text = "reading"
  elseif status == "triage" then
    status_color = "yellow"
    status_icon = "↻"
    status_text = "triage"
  elseif status == "triage_retry" then
    status_color = "yellow"
    status_icon = "↻"
    status_text = "triage (retry)"
  elseif status == "summary" then
    status_color = "cyan"
    status_icon = "↻"
    status_text = "summarizing"
  elseif status == "complete" then
    status_color = "green"
    status_icon = "✓"
    status_text = "done"
  elseif status == "error" then
    status_color = "red"
    status_icon = "✕"
    status_text = "failed"
  end
  
  local items = {}
  
  -- Header with status
  local header = status_icon .. " " .. status_text
  if short_id ~= "" then
    header = header .. "  " .. short_id
  end
  table.insert(items, { text = header, fg = status_color })
  
  -- Info line
  if messages_count > 0 then
    local info = tostring(messages_count) .. " msgs"
    if tool_calls_count > 0 then
      info = info .. ", " .. tostring(tool_calls_count) .. " tools"
    end
    table.insert(items, { text = info, fg = "gray" })
  end
  
  -- Triage section (always shown once we have decisions)
  if #decisions > 0 or triage_done then
    table.insert(items, { text = "─── Triage ───", fg = "gray" })
    
    local keep_count = 0
    local summarize_count = 0
    local drop_count = 0
    for _, d in ipairs(decisions) do
      if d.action == "keep" then keep_count = keep_count + 1
      elseif d.action == "summarize" then summarize_count = summarize_count + 1
      elseif d.action == "drop" then drop_count = drop_count + 1
      end
    end
    
    if #decisions > 0 then
      local counts = {}
      if keep_count > 0 then table.insert(counts, "✓" .. keep_count) end
      if summarize_count > 0 then table.insert(counts, "≈" .. summarize_count) end
      if drop_count > 0 then table.insert(counts, "✕" .. drop_count) end
      table.insert(items, { text = table.concat(counts, " "), fg = "white" })
      
      -- Show ALL decisions (scrollable list)
      for _, d in ipairs(decisions) do
        local icon = "○"
        local color = "gray"
        if d.action == "keep" then
          icon = "✓"
          color = "green"
        elseif d.action == "summarize" then
          icon = "≈"
          color = "yellow"
        elseif d.action == "drop" then
          icon = "✕"
          color = "red"
        end
        local name = d.name or d.id:sub(1,12)
        table.insert(items, { text = " " .. icon .. " " .. name, fg = color })
      end
    end
  end
  
  -- Summary section (shown during/after summary phase)
  if status == "summary" or status == "complete" or summary_preview ~= "" then
    table.insert(items, { text = "─── Summary ───", fg = "gray" })
    if summary_preview ~= "" then
      -- Show full summary, wrapped
      for line in summary_preview:gmatch("[^\\n]+") do
        table.insert(items, { text = line, fg = "cyan" })
      end
    elseif status == "summary" then
      table.insert(items, { text = "generating...", fg = "cyan" })
    end
  end
  
  -- Error message
  if state.error then
    table.insert(items, { text = "⚠ " .. state.error, fg = "red" })
  end
  
  return {
    type = "box",
    border = "rounded",
    title = "📦 Compactor",
    border_fg = status_color,
    padding = { 0, 1 },
    child = {
      type = "list",
      items = items,
    },
  }
end
`;

/** Register the Lua UI widget for this plugin (once per session). */
const registeredSessions = new Set<string>();

function ensureLuaRegistered(session: string): void {
  if (registeredSessions.has(session)) return;
  hostRequest("ui_register_lua", { session, source: COMPACTOR_LUA });
  registeredSessions.add(session);
}

/** Push UI state update to TUI. */
function uiSetState(session: string, state: CompactorState): void {
  hostRequest("ui_set_state", { session, state: state as unknown as Record<string, unknown> });
}

/** Clear the UI widget when done. */
function uiClear(session: string): void {
  hostRequest("ui_clear", { session });
}

// ── Effect programs (the agent turn) ─────────────────────────────────────────

const TRIAGE_SYSTEM =
  "You are the compaction planner of a coding agent. You are given the transcript " +
  "inventory of the messages about to be compacted. Every tool call has a unique id. " +
  "Decide, per id, whether to KEEP the tool result verbatim (large/important outputs), " +
  "SUMMARIZE it (small note), or DROP it (noise). Keep the conversation's goal in mind. " +
  "You have one tool, submit_triage. Call it exactly once with your plan. Do not reply " +
  "with prose — the plan is delivered only through the submit_triage tool call.";

const SUMMARY_SYSTEM =
  "You are the summarizer of a coding agent. Write a concise but complete summary of " +
  "the conversation that will replace the old messages. Include: key decisions made, " +
  "file paths modified, current state, and next steps. Be thorough but concise. " +
  "You have one tool, submit_summary. Call it exactly once with your summary text. " +
  "Do not reply with prose — the summary is delivered only through the submit_summary tool call.";

/**
 * Tool the triage pass forces the model to call. The `schema` is the JSON
 * Schema of the plan object; the provider validates the model's arguments
 * against it, so the plugin never parses free-form model prose. The compactor
 * declares this itself — kn9t-core has no compaction-specific tool.
 */
const TRIAGE_TOOL = {
  name: "submit_triage",
  description:
    "Submit the compaction plan: per tool-call-id keep/summarize/drop decisions, " +
    "plus the actions the agent should resume with.",
  schema: {
    type: "object",
    additionalProperties: false,
    properties: {
      decisions: {
        type: "array",
        items: {
          type: "object",
          additionalProperties: false,
          properties: {
            id: { type: "string", description: "a tool call id from the inventory" },
            action: { type: "string", enum: ["keep", "summarize", "drop"] },
            note: { type: "string", description: "optional one-line note" },
          },
          required: ["id", "action"],
        },
      },
      resume_actions: {
        type: "array",
        items: { type: "string" },
        description: "what the agent should do next",
      },
    },
    required: ["decisions"],
  },
};

/** Tool the summary pass forces the model to call. */
const SUMMARY_TOOL = {
  name: "submit_summary",
  description: "Submit the summary text that replaces the compacted messages.",
  schema: {
    type: "object",
    additionalProperties: false,
    properties: {
      summary: { type: "string", description: "the replacement summary" },
    },
    required: ["summary"],
  },
};

interface Decision {
  id: string;
  action: "keep" | "summarize" | "drop";
  note?: string;
}

interface MessageWire {
  seq: number;
  role: string;
  content: Array<Record<string, unknown>>;
}

function isText(b: Record<string, unknown>): b is { type: "text"; text: string } {
  return b["type"] === "text" && typeof b["text"] === "string";
}

/** Flatten text blocks from a content array (for previews). */
function textOf(content: Array<Record<string, unknown>>): string {
  return content.filter(isText).map((b) => b.text).join("\n");
}

/** Format messages as a readable transcript for the summary pass. */
function formatTranscript(messages: MessageWire[]): string {
  const lines: string[] = [];
  for (const m of messages) {
    const role = m.role.toUpperCase();
    for (const block of m.content) {
      const t = block["type"];
      if (t === "text") {
        const text = String(block["text"] ?? "");
        // Truncate very long text blocks
        const truncated = text.length > 2000 ? text.slice(0, 2000) + "..." : text;
        lines.push(`[${role}] ${truncated}`);
      } else if (t === "tool_call") {
        const name = String(block["name"] ?? "");
        const args = String(block["args_json"] ?? "").slice(0, 500);
        lines.push(`[${role}] Tool call: ${name}(${args})`);
      } else if (t === "tool_result") {
        const id = String(block["id"] ?? "");
        const content = Array.isArray(block["content"]) ? block["content"] : [];
        const preview = textOf(content as Array<Record<string, unknown>>).slice(0, 1000);
        lines.push(`[TOOL RESULT ${id}] ${preview}`);
      }
    }
  }
  return lines.join("\n\n");
}

/** Build the per-CallId inventory text + maps for the triage pass. */
function inventory(messages: MessageWire[]): {
  text: string;
  byId: Map<string, { result: Record<string, unknown> | undefined; preview: string }>;
} {
  const lines: string[] = [];
  const byId = new Map<string, { result: Record<string, unknown> | undefined; preview: string }>();
  const seen = (id: string, result?: Record<string, unknown>, preview = "") => {
    if (!byId.has(id)) byId.set(id, { result, preview });
    else {
      const prev = byId.get(id)!;
      if (result) prev.result = result;
      if (preview) prev.preview = preview;
    }
  };
  for (const m of messages) {
    for (const block of m.content) {
      const t = block["type"];
      if (t === "tool_call") {
        const id = String(block["id"] ?? "");
        const name = String(block["name"] ?? "");
        const args = String(block["args_json"] ?? "").slice(0, 200);
        lines.push(`tool_call ${id} ${name}(${args})`);
        seen(id);
      } else if (t === "tool_result") {
        const id = String(block["id"] ?? "");
        const preview = textOf(
          (Array.isArray(block["content"]) ? block["content"] : []) as Array<Record<string, unknown>>,
        ).slice(0, 300);
        lines.push(`tool_result ${id}: ${preview.length} chars: ${preview}`);
        seen(id, block, preview);
      } else if (t === "text") {
        const text = String(block["text"] ?? "").slice(0, 400);
        lines.push(`text: ${text}`);
      }
    }
  }
  return { text: lines.join("\n"), byId };
}

/**
 * Find the tool_call block named `name` in a content array and parse its
 * `args_json` (the provider's verbatim, schema-validated argument bytes).
 * Returns null when the model did not call the tool, or its arguments were not
 * a JSON object. No prose/fence tolerance: the contract is the tool call.
 */
function toolCallArgs(
  content: Array<Record<string, unknown>>,
  name: string,
): Record<string, unknown> | null {
  for (const block of content) {
    if (block["type"] === "tool_call" && block["name"] === name) {
      try {
        const v = JSON.parse(String(block["args_json"] ?? ""));
        if (v && typeof v === "object" && !Array.isArray(v)) return v as Record<string, unknown>;
      } catch {
        return null;
      }
    }
  }
  return null;
}

/** Parse triage result, validate IDs, return decisions or null if retry needed. */
function parseTriageResult(
  content: Array<Record<string, unknown>>,
  knownIds: string[]
): { decisions: Decision[]; resumeActions: string[]; needsRetry: boolean; invalidIds: string[] } {
  const args = toolCallArgs(content, "submit_triage");
  if (!args) {
    return { decisions: [], resumeActions: [], needsRetry: true, invalidIds: [] };
  }
  
  const rawDecisions = Array.isArray(args["decisions"]) ? (args["decisions"] as Array<Record<string, unknown>>) : [];
  const candidate = rawDecisions
    .filter((d) => typeof d["id"] === "string")
    .map((d) => ({
      id: String(d["id"]),
      action: d["action"] === "summarize" || d["action"] === "drop" ? (d["action"] as Decision["action"]) : "keep" as Decision["action"],
      note: typeof d["note"] === "string" ? String(d["note"]) : undefined,
    }));
  
  const valid = candidate.filter((d) => knownIds.includes(d.id));
  const invalid = candidate.filter((d) => !knownIds.includes(d.id));
  const resumeActions = Array.isArray(args["resume_actions"])
    ? (args["resume_actions"] as unknown[]).filter((a): a is string => typeof a === "string")
    : [];
  
  if (invalid.length > 0) {
    return { decisions: valid, resumeActions, needsRetry: true, invalidIds: invalid.map(d => d.id) };
  }
  
  return { decisions: valid, resumeActions, needsRetry: false, invalidIds: [] };
}

/** Parse summary result. */
function parseSummaryResult(content: Array<Record<string, unknown>>): string {
  const args = toolCallArgs(content, "submit_summary");
  if (args && typeof args["summary"] === "string" && args["summary"].trim().length > 0) {
    return String(args["summary"]);
  }
  return "(compaction summary unavailable — model did not call submit_summary)";
}

// The real program: takes the hook payload, runs triage + summary in PARALLEL.
function compactProgram(hookPayload: Record<string, unknown>) {
  return Effect.gen(function* (_) {
    const session = String(hookPayload["session"] ?? "");
    if (!session) return yield* _(Effect.fail(new Error("no session in compactor_compact payload")));
    const replaced = hookPayload["replaced"] as { start?: number; end?: number } | undefined;
    const start = replaced?.start ?? 0;
    const end = replaced?.end ?? Number.MAX_SAFE_INTEGER;

    // Initialize UI state
    const state: CompactorState = {
      status: "reading",
      session_id: session,
      messages_count: 0,
      tool_calls_count: 0,
      decisions: [],
      triage_done: false,
      summary_preview: "",
    };
    
    // Register UI widget and show initial state
    ensureLuaRegistered(session);
    uiSetState(session, state);

    // 1. Read the span to be replaced.
    const read = hostRequest("session_read", { session, start, end });
    if (!read.ok) {
      state.status = "error";
      state.error = `session_read: ${read.error}`;
      uiSetState(session, state);
      return yield* _(Effect.fail(new Error(`session_read: ${read.error}`)));
    }
    const messages = ((read.result as { messages?: MessageWire[] })["messages"] ?? []) as MessageWire[];

    const inv = inventory(messages);
    const knownIds = [...inv.byId.keys()];
    
    // Update UI with message/tool counts
    state.messages_count = messages.length;
    state.tool_calls_count = knownIds.length;
    uiSetState(session, state);
    
    if (knownIds.length === 0 && messages.length === 0) {
      state.status = "error";
      state.error = "span is empty — nothing to compact";
      uiSetState(session, state);
      return yield* _(Effect.fail(new Error("span is empty — nothing to compact")));
    }

    // Build a map of tool call id -> tool name for display
    const toolNameById = new Map<string, string>();
    for (const m of messages) {
      for (const block of m.content) {
        if (block["type"] === "tool_call") {
          const id = String(block["id"] ?? "");
          const name = String(block["name"] ?? "");
          if (id && name) toolNameById.set(id, name);
        }
      }
    }

    // Prepare messages for both passes
    const triageUser =
      `Transcript inventory (ids you may cite):\n${inv.text}\n\n` +
      `Cite ONLY ids from the list above. Call submit_triage with your plan.`;
    const triageMsgs = [
      { id: "sys-triage", role: "system", silent: true, content: [{ type: "text", text: TRIAGE_SYSTEM }] },
      { id: "usr-triage", role: "user", silent: true, content: [{ type: "text", text: triageUser }] },
    ];
    
    const transcript = formatTranscript(messages);
    const summaryMsgs = [
      { id: "sys-summary", role: "system", silent: true, content: [{ type: "text", text: SUMMARY_SYSTEM }] },
      { id: "usr-summary", role: "user", silent: true, content: [{ type: "text", text: `Summarize this conversation:\n\n${transcript}` }] },
    ];

    // 2. Launch BOTH triage and summary in parallel!
    state.status = "triage";
    uiSetState(session, state);
    
    const triageReqId = hostRequestAsync("provider_complete", { session, messages: triageMsgs, tools: [TRIAGE_TOOL] });
    const summaryReqId = hostRequestAsync("provider_complete", { session, messages: summaryMsgs, tools: [SUMMARY_TOOL] });
    
    let decisions: Decision[] = [];
    let resumeActions: string[] = [];
    let summaryText = "";
    let triageResult: ApiResult | null = null;
    let summaryResult: ApiResult | null = null;
    
    // Wait for both results, updating UI as each arrives
    const results = awaitResults([triageReqId, summaryReqId], (id, result) => {
      if (id === triageReqId) {
        triageResult = result;
        if (result.ok) {
          const content = ((result.result as { content?: Array<Record<string, unknown>> })["content"] ?? []) as Array<Record<string, unknown>>;
          const parsed = parseTriageResult(content, knownIds);
          decisions = parsed.decisions;
          resumeActions = parsed.resumeActions;
          
          state.decisions = decisions.map(d => ({
            id: d.id,
            action: d.action,
            name: toolNameById.get(d.id),
          }));
          state.triage_done = true;
          
          // If triage needs retry due to invalid IDs, we'll handle it after
          if (parsed.needsRetry && parsed.invalidIds.length > 0) {
            state.status = "triage_retry";
          }
        }
        uiSetState(session, state);
      }
      
      if (id === summaryReqId) {
        summaryResult = result;
        if (result.ok) {
          const content = ((result.result as { content?: Array<Record<string, unknown>> })["content"] ?? []) as Array<Record<string, unknown>>;
          summaryText = parseSummaryResult(content);
          state.summary_preview = summaryText;
          state.status = "summary"; // Show we got the summary
        }
        uiSetState(session, state);
      }
    });
    
    // Check for errors
    triageResult = results.get(triageReqId) ?? null;
    summaryResult = results.get(summaryReqId) ?? null;
    
    if (!triageResult?.ok) {
      state.status = "error";
      state.error = `provider(triage): ${triageResult?.error ?? "unknown"}`;
      uiSetState(session, state);
      return yield* _(Effect.fail(new Error(state.error)));
    }
    
    if (!summaryResult?.ok) {
      state.status = "error";
      state.error = `provider(summary): ${summaryResult?.error ?? "unknown"}`;
      uiSetState(session, state);
      return yield* _(Effect.fail(new Error(state.error)));
    }
    
    // Re-parse triage to check if retry is needed
    const triageContent = ((triageResult.result as { content?: Array<Record<string, unknown>> })["content"] ?? []) as Array<Record<string, unknown>>;
    const triageParsed = parseTriageResult(triageContent, knownIds);
    
    // If triage needs retry (no tool call or invalid IDs), do ONE sync retry
    if (triageParsed.needsRetry) {
      state.status = "triage_retry";
      uiSetState(session, state);
      
      let correction = "";
      if (triageParsed.invalidIds.length > 0) {
        correction = "\n\nYour previous submit_triage call cited unknown ids. Cite only:\n" + knownIds.join(", ");
      } else {
        correction = "\n\nYou did not call submit_triage. Call submit_triage exactly once with the plan.";
      }
      
      const retryMsgs = [
        { id: "sys-triage", role: "system", silent: true, content: [{ type: "text", text: TRIAGE_SYSTEM }] },
        { id: "usr-triage", role: "user", silent: true, content: [{ type: "text", text: triageUser + correction }] },
      ];
      
      const retryResult = hostRequest("provider_complete", { session, messages: retryMsgs, tools: [TRIAGE_TOOL] });
      if (retryResult.ok) {
        const retryContent = ((retryResult.result as { content?: Array<Record<string, unknown>> })["content"] ?? []) as Array<Record<string, unknown>>;
        const retryParsed = parseTriageResult(retryContent, knownIds);
        decisions = retryParsed.decisions;
        resumeActions = retryParsed.resumeActions;
        
        state.decisions = decisions.map(d => ({
          id: d.id,
          action: d.action,
          name: toolNameById.get(d.id),
        }));
        state.triage_done = true;
        uiSetState(session, state);
      }
      // If retry also fails, we keep whatever valid decisions we got
    } else {
      decisions = triageParsed.decisions;
      resumeActions = triageParsed.resumeActions;
    }

    // Ensure we have the summary text
    if (!summaryText) {
      const summaryContent = ((summaryResult.result as { content?: Array<Record<string, unknown>> })["content"] ?? []) as Array<Record<string, unknown>>;
      summaryText = parseSummaryResult(summaryContent);
      state.summary_preview = summaryText;
    }

    // 3. Assemble the plan: summary message embeds kept tool results VERBATIM.
    const kept = decisions.filter((d) => d.action === "keep");
    const content: Array<Record<string, unknown>> = [{ type: "text", text: summaryText }];
    for (const m of messages) {
      for (const block of m.content) {
        if (block["type"] === "tool_result" && kept.some((d) => d.id === String(block["id"]))) {
          content.push(block); // byte-exact copy of the original result block
        }
      }
    }

    const handoff = {
      keep: kept.map((d) => d.id),
      summarize: decisions
        .filter((d) => d.action === "summarize")
        .map((d) => ({ id: d.id, summary: d.note ?? "summarized during compaction" })),
      drop: decisions.filter((d) => d.action === "drop").map((d) => d.id),
      resume_actions: resumeActions,
    };

    // Mark as complete
    state.status = "complete";
    uiSetState(session, state);

    return {
      summary: { id: "compacted-1", role: "assistant", silent: false, content },
      handoff: (handoff.keep.length + handoff.summarize.length + handoff.drop.length) > 0 || handoff.resume_actions.length > 0
        ? handoff
        : undefined,
    };
  });
}

// ── main loop ────────────────────────────────────────────────────────────────

function main(): void {
  const hello = reader.readLine();
  if (hello === null) process.exit(1);
  const helloMsg = JSON.parse(hello) as { t?: string; kn9t?: string };
  if (helloMsg.t !== "hello") {
    console.error("kn9t-compactor: expected host hello, got:", hello);
    process.exit(1);
  }
  console.error(`kn9t-compactor: connected to kn9t ${helloMsg.kn9t ?? "?"} (host_api compactor)`);
  writeMsg({ t: "hello", name: "kn9t-compactor", capabilities: ["compactor", "host_api"] });

  while (true) {
    const line = reader.readLine();
    if (line === null) break;
    const msg = JSON.parse(line) as { t?: string; id?: number; hook?: string; payload?: Record<string, unknown> };
    if (msg.t === "shutdown") break;
    if (msg.t === "hook" && msg.hook === "compactor_compact") {
      const id = msg.id ?? 0;
      const exit = Effect.runSync(Effect.either(compactProgram(msg.payload ?? {})));
      if (exit._tag === "Left") {
        console.error(`kn9t-compactor: compaction failed: ${(exit.left as Error).message}`);
        writeMsg({ t: "result", id, error: (exit.left as Error).message });
      } else {
        writeMsg({ t: "result", id, ...(exit.right as Record<string, unknown>) });
      }
    } else {
      // Unknown hook: answer a benign error so the host never waits.
      writeMsg({ t: "result", id: msg.id ?? 0, error: `kn9t-compactor: unhandled hook ${msg.hook ?? "?"}` });
    }
  }
}

try {
  main();
} catch (e) {
  console.error("kn9t-compactor: fatal:", e);
  process.exit(1);
}