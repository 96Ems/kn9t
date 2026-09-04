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
  "the conversation that replaces the old messages: decisions, file paths, open tasks. " +
  "Tool results marked KEEP are preserved verbatim by the host and must NOT be repeated " +
  "in your summary. You have one tool, submit_summary. Call it exactly once with your " +
  "summary text. Do not reply with prose — the summary is delivered only through the " +
  "submit_summary tool call.";

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

// The real program: takes the hook payload, runs the two-pass agent turn.
function compactProgram(hookPayload: Record<string, unknown>) {
  return Effect.gen(function* (_) {
    const session = String(hookPayload["session"] ?? "");
    if (!session) return yield* _(Effect.fail(new Error("no session in compactor_compact payload")));
    const replaced = hookPayload["replaced"] as { start?: number; end?: number } | undefined;
    const start = replaced?.start ?? 0;
    const end = replaced?.end ?? Number.MAX_SAFE_INTEGER;

    // 1. Read the span to be replaced.
    const read = hostRequest("session_read", { session, start, end });
    if (!read.ok) return yield* _(Effect.fail(new Error(`session_read: ${read.error}`)));
    const messages = ((read.result as { messages?: MessageWire[] })["messages"] ?? []) as MessageWire[];

    const inv = inventory(messages);
    const knownIds = [...inv.byId.keys()];
    if (knownIds.length === 0 && messages.length === 0) {
      return yield* _(Effect.fail(new Error("span is empty — nothing to compact")));
    }

    const triageUser =
      `Transcript inventory (ids you may cite):\n${inv.text}\n\n` +
      `Cite ONLY ids from the list above. Call submit_triage with your plan.`;

    // 2. Triage pass — the model MUST deliver its plan via the submit_triage
    // tool call (schema-validated by the provider). One correction shot covers
    // both failure modes: the model answered without calling the tool, or it
    // cited ids not in the inventory. Only after the shot is spent do we fall
    // back (empty plan) rather than abort the whole compaction.
    let decisions: Decision[] = [];
    let resumeActions: string[] = [];
    let correction = "";
    for (let attempt = 0; attempt < 2; attempt++) {
      const msgs = [
        { id: "sys-triage", role: "system", silent: false, content: [{ type: "text", text: TRIAGE_SYSTEM }] },
        { id: "usr-triage", role: "user", silent: false, content: [{ type: "text", text: triageUser + correction }] },
      ];
      const r = hostRequest("provider_complete", { session, messages: msgs, tools: [TRIAGE_TOOL] });
      if (!r.ok) return yield* _(Effect.fail(new Error(`provider(triage): ${r.error}`)));
      const content = ((r.result as { content?: Array<Record<string, unknown>> })["content"] ?? []) as Array<Record<string, unknown>>;
      const args = toolCallArgs(content, "submit_triage");
      if (!args) {
        if (attempt === 0) {
          correction = "\n\nYou did not call submit_triage. Call submit_triage exactly once with the plan.";
          continue;
        }
        // Correction shot spent and still no tool call: fall back to an empty
        // plan so the host can still compact rather than fail the whole turn.
        decisions = [];
        resumeActions = [];
        break;
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
      const resumeOf = () =>
        Array.isArray(args["resume_actions"])
          ? (args["resume_actions"] as unknown[]).filter((a): a is string => typeof a === "string")
          : [];
      if (invalid.length === 0) {
        decisions = valid;
        resumeActions = resumeOf();
        break;
      }
      if (attempt === 1) {
        // One correction shot used; keep only the valid decisions.
        decisions = valid;
        resumeActions = resumeOf();
        break;
      }
      // First attempt cited unknown ids: spend the correction shot listing the
      // only ids that are allowed.
      correction = "\n\nYour previous submit_triage call cited unknown ids. Cite only:\n" + knownIds.join(", ");
    }

    // 3. Summary pass — the model MUST deliver its summary via submit_summary.
    // This must NOT round-trip full tool outputs back into the LLM's context
    // (96E-17: summarize_tool_result never requires full output to leave the
    // host). We send only previews + decisions; kept results are copied
    // verbatim host-side, summarized ones use the triage note.
    const summaryMsgs = [
      { id: "sys-summary", role: "system", silent: false, content: [{ type: "text", text: SUMMARY_SYSTEM }] },
      { id: "usr-summary", role: "user", silent: false, content: [{ type: "text", text: `Decisions:\n${JSON.stringify(decisions)}\n\nInventory:\n${inv.text}` }] },
    ];
    const s = hostRequest("provider_complete", { session, messages: summaryMsgs, tools: [SUMMARY_TOOL] });
    if (!s.ok) return yield* _(Effect.fail(new Error(`provider(summary): ${s.error}`)));
    const sContent = ((s.result as { content?: Array<Record<string, unknown>> })["content"] ?? []) as Array<Record<string, unknown>>;
    const sArgs = toolCallArgs(sContent, "submit_summary");
    const summaryText =
      sArgs && typeof sArgs["summary"] === "string" && sArgs["summary"].trim().length > 0
        ? String(sArgs["summary"])
        : "(compaction summary unavailable — model did not call submit_summary)";

    // 4. Assemble the plan: summary message embeds kept tool results VERBATIM.
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