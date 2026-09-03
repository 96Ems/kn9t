/**
 * kn9t-ask-user — reference plugin for 96E-28.
 *
 * Zero host special-case: this is the ONLY place `ask_user` exists. It is a
 * plain tool whose `execute` calls the generic `host_api` op
 * `interaction_request {session, payload}` (opaque JSON), which emits
 * `LiveEvent::InteractionRequest {id, plugin, payload}` to the client's SSE
 * bus. The TUI's generic renderer (no hardcoded "question" type) shows the
 * payload and POSTs `{id, payload}` to `/ui-respond`, which resolves the
 * pending `InteractionRegistry` slot and unblocks this tool's `execute`.
 *
 * This is the SDK proof that the generic primitive is sufficient — any plugin
 * can build its own ask_user-shaped UX without the host knowing "question".
 */
import * as fs from "node:fs";

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
function writeMsg(msg: unknown): void { fs.writeSync(1, JSON.stringify(msg) + "\n"); }

interface ApiResult { t: string; id: number; ok: boolean; result?: Record<string, unknown>; error?: string; }

let requestId = 1000;
const reader = new LineReader();
const replies = new Map<number, ApiResult>();

function pumpUntil(awaitId: number): ApiResult {
  for (;;) {
    const hit = replies.get(awaitId);
    if (hit !== undefined) { replies.delete(awaitId); return hit; }
    const line = reader.readLine();
    if (line === null) throw new Error("host closed");
    const msg = JSON.parse(line) as { t?: string; id?: number } & Record<string, unknown>;
    if (msg.t === "api_result" && typeof msg.id === "number") { replies.set(msg.id, msg as unknown as ApiResult); continue; }
    if (msg.t === "hook" && typeof msg.id === "number") { handleHook(msg.id, (msg.payload as Record<string, unknown>) ?? {}); continue; }
    if (msg.t === "shutdown") throw new Error("shutdown");
  }
}
function hostRequest(op: string, payload: unknown): ApiResult {
  const id = requestId++;
  writeMsg({ t: "request", id, op, payload });
  return pumpUntil(id);
}

function askUser(args: Record<string, unknown>, session: string): { content: Array<Record<string, unknown>>; is_error: boolean } {
  const question = typeof args["question"] === "string" ? args["question"] : null;
  if (!question) return { content: [{ type: "text", text: "ask_user requires \"question\"" }], is_error: true };
  const choices = Array.isArray(args["choices"]) ? (args["choices"] as unknown[]).filter((x): x is string => typeof x === "string") : undefined;
  const payload: Record<string, unknown> = { question };
  if (choices) payload["choices"] = choices;
  // Optional free-form extras from the caller are forwarded opaquely — the host never inspects.
  if (args["placeholder"] !== undefined) payload["placeholder"] = args["placeholder"];

  const r = hostRequest("interaction_request", { session, payload });
  if (!r.ok) return { content: [{ type: "text", text: `interaction_request: ${r.error}` }], is_error: true };
  const answer = r.result?.["payload"];
  // Normalize: the TUI sends {value: ...} or {cancelled:true}; forward verbatim but friendlier.
  const text = typeof answer === "string" ? answer : JSON.stringify(answer ?? null);
  return { content: [{ type: "text", text: `user answered: ${text}` }], is_error: false };
}

function handleHook(id: number, payload: Record<string, unknown>): void {
  const name = String(payload["tool"] ?? "");
  const args = (payload["args"] as Record<string, unknown>) ?? {};
  const session = String(payload["session"] ?? "");
  if (name === "ask_user") {
    const out = askUser(args, session);
    writeMsg({ t: "result", id, content: out.content, is_error: out.is_error });
  } else {
    writeMsg({ t: "result", id, content: [{ type: "text", text: `kn9t-ask-user: unknown tool ${name}` }], is_error: true });
  }
}

function main(): void {
  const hello = reader.readLine(); if (hello === null) process.exit(1);
  const h = JSON.parse(hello) as { t?: string };
  if (h.t !== "hello") { console.error("expected hello"); process.exit(1); }
  writeMsg({
    t: "hello",
    name: "kn9t-ask-user",
    capabilities: ["host_api"],
    tools: [
      {
        name: "ask_user",
        description: "Ask the human a question and wait for their reply. Use for genuine ambiguity, not to avoid deciding.",
        schema: {
          type: "object",
          properties: {
            question: { type: "string", description: "The question to ask the user." },
            choices: { type: "array", items: { type: "string" }, description: "Optional choices; omit for free-text." }
          },
          required: ["question"]
        },
        parallel_safe: false,
      }
    ]
  });
  while (true) {
    const line = reader.readLine(); if (line === null) break;
    const msg = JSON.parse(line) as { t?: string; id?: number } & Record<string, unknown>;
    if (msg.t === "shutdown") break;
    if (msg.t === "hook" && typeof msg.id === "number") handleHook(msg.id, (msg.payload as Record<string, unknown>) ?? {});
  }
}
try { main(); } catch (e) { console.error("kn9t-ask-user fatal:", e); process.exit(1); }
