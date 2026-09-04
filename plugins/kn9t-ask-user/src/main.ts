/**
 * kn9t-ask-user — advanced question tool for kn9t.
 *
 * Supports multiple question types:
 * - text: free-form text input
 * - choice: single selection from options
 * - multi: multiple selection from options
 * - confirm: yes/no confirmation
 * - sequence: multiple questions in order
 *
 * Uses the generic `host_api` op `interaction_request` which emits
 * `LiveEvent::InteractionRequest` to the TUI. The TUI renders based on
 * the payload structure and POSTs the response to `/ui-respond`.
 */
import * as fs from "node:fs";

// ── Wire protocol ────────────────────────────────────────────────────────────

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
const replies = new Map<number, ApiResult>();

function pumpUntil(awaitId: number): ApiResult {
  for (;;) {
    const hit = replies.get(awaitId);
    if (hit !== undefined) {
      replies.delete(awaitId);
      return hit;
    }
    const line = reader.readLine();
    if (line === null) throw new Error("host closed");
    const msg = JSON.parse(line) as { t?: string; id?: number } & Record<string, unknown>;
    if (msg.t === "api_result" && typeof msg.id === "number") {
      replies.set(msg.id, msg as unknown as ApiResult);
      continue;
    }
    if (msg.t === "hook" && typeof msg.id === "number") {
      handleHook(msg.id, (msg.payload as Record<string, unknown>) ?? {});
      continue;
    }
    if (msg.t === "shutdown") throw new Error("shutdown");
  }
}

function hostRequest(op: string, payload: unknown): ApiResult {
  const id = requestId++;
  writeMsg({ t: "request", id, op, payload });
  return pumpUntil(id);
}

// ── Question types ───────────────────────────────────────────────────────────

interface QuestionOption {
  label: string;
  value?: string;
  description?: string;
}

interface BaseQuestion {
  header?: string;
  required?: boolean;
}

interface TextQuestion extends BaseQuestion {
  type: "text";
  question: string;
  placeholder?: string;
  default?: string;
}

interface ChoiceQuestion extends BaseQuestion {
  type: "choice";
  question: string;
  options: QuestionOption[];
  allow_custom?: boolean;
}

interface MultiQuestion extends BaseQuestion {
  type: "multi";
  question: string;
  options: QuestionOption[];
  min?: number;
  max?: number;
}

interface ConfirmQuestion extends BaseQuestion {
  type: "confirm";
  question: string;
  default?: boolean;
}

interface SequenceQuestion extends BaseQuestion {
  type: "sequence";
  questions: QuestionSpec[];
}

type QuestionSpec = TextQuestion | ChoiceQuestion | MultiQuestion | ConfirmQuestion | SequenceQuestion;

// ── Tool result ──────────────────────────────────────────────────────────────

interface ToolResult {
  content: Array<{ type: string; text: string }>;
  is_error: boolean;
}

function ok(text: string): ToolResult {
  return { content: [{ type: "text", text }], is_error: false };
}

function err(text: string): ToolResult {
  return { content: [{ type: "text", text }], is_error: true };
}

// ── Question execution ───────────────────────────────────────────────────────

function executeQuestion(spec: QuestionSpec, session: string): ToolResult {
  switch (spec.type) {
    case "text":
      return executeText(spec, session);
    case "choice":
      return executeChoice(spec, session);
    case "multi":
      return executeMulti(spec, session);
    case "confirm":
      return executeConfirm(spec, session);
    case "sequence":
      return executeSequence(spec, session);
    default:
      return err(`Unknown question type: ${(spec as { type: string }).type}`);
  }
}

function executeText(q: TextQuestion, session: string): ToolResult {
  const payload: Record<string, unknown> = {
    type: "text",
    question: q.question,
  };
  if (q.header) payload.header = q.header;
  if (q.placeholder) payload.placeholder = q.placeholder;
  if (q.default) payload.default = q.default;

  const r = hostRequest("interaction_request", { session, payload });
  if (!r.ok) return err(`interaction_request: ${r.error}`);

  const answer = r.result?.payload as Record<string, unknown> | undefined;
  if (answer?.cancelled) return ok("User cancelled.");
  
  const value = answer?.value ?? "";
  return ok(`User answered: ${value}`);
}

function executeChoice(q: ChoiceQuestion, session: string): ToolResult {
  const payload: Record<string, unknown> = {
    type: "choice",
    question: q.question,
    options: q.options,
  };
  if (q.header) payload.header = q.header;
  if (q.allow_custom) payload.allow_custom = true;

  const r = hostRequest("interaction_request", { session, payload });
  if (!r.ok) return err(`interaction_request: ${r.error}`);

  const answer = r.result?.payload as Record<string, unknown> | undefined;
  if (answer?.cancelled) return ok("User cancelled.");

  const selected = answer?.value;
  if (typeof selected === "string") {
    return ok(`User selected: ${selected}`);
  }
  return ok(`User selected: ${JSON.stringify(selected)}`);
}

function executeMulti(q: MultiQuestion, session: string): ToolResult {
  const payload: Record<string, unknown> = {
    type: "multi",
    question: q.question,
    options: q.options,
  };
  if (q.header) payload.header = q.header;
  if (q.min !== undefined) payload.min = q.min;
  if (q.max !== undefined) payload.max = q.max;

  const r = hostRequest("interaction_request", { session, payload });
  if (!r.ok) return err(`interaction_request: ${r.error}`);

  const answer = r.result?.payload as Record<string, unknown> | undefined;
  if (answer?.cancelled) return ok("User cancelled.");

  const selected = answer?.value;
  if (Array.isArray(selected)) {
    return ok(`User selected: ${selected.join(", ")}`);
  }
  return ok(`User selected: ${JSON.stringify(selected)}`);
}

function executeConfirm(q: ConfirmQuestion, session: string): ToolResult {
  const payload: Record<string, unknown> = {
    type: "confirm",
    question: q.question,
  };
  if (q.header) payload.header = q.header;
  if (q.default !== undefined) payload.default = q.default;

  const r = hostRequest("interaction_request", { session, payload });
  if (!r.ok) return err(`interaction_request: ${r.error}`);

  const answer = r.result?.payload as Record<string, unknown> | undefined;
  if (answer?.cancelled) return ok("User cancelled.");

  const confirmed = answer?.value === true || answer?.value === "yes";
  return ok(confirmed ? "User confirmed: Yes" : "User declined: No");
}

function executeSequence(q: SequenceQuestion, session: string): ToolResult {
  const results: string[] = [];
  
  for (let i = 0; i < q.questions.length; i++) {
    const subQ = q.questions[i];
    const result = executeQuestion(subQ, session);
    
    if (result.is_error) {
      return result; // Propagate error
    }
    
    const text = result.content[0]?.text ?? "";
    if (text.includes("cancelled")) {
      return ok(`Sequence cancelled at question ${i + 1}/${q.questions.length}`);
    }
    
    results.push(`Q${i + 1}: ${text}`);
  }
  
  return ok(`Sequence completed:\n${results.join("\n")}`);
}

// ── Legacy support ───────────────────────────────────────────────────────────

function executeLegacy(args: Record<string, unknown>, session: string): ToolResult {
  // Support old format: {question: string, choices?: string[]}
  const question = args.question as string;
  const choices = args.choices as string[] | undefined;
  
  if (choices && choices.length > 0) {
    // Convert to choice question
    return executeChoice({
      type: "choice",
      question,
      options: choices.map(c => ({ label: c })),
      allow_custom: true,
    }, session);
  }
  
  // Default to text question
  return executeText({
    type: "text",
    question,
    placeholder: args.placeholder as string | undefined,
  }, session);
}

// ── Hook handler ─────────────────────────────────────────────────────────────

function handleHook(id: number, payload: Record<string, unknown>): void {
  const name = String(payload.tool ?? "");
  const args = (payload.args as Record<string, unknown>) ?? {};
  const session = String(payload.session ?? "");

  if (name !== "ask_user") {
    writeMsg({
      t: "result",
      id,
      content: [{ type: "text", text: `kn9t-ask-user: unknown tool ${name}` }],
      is_error: true,
    });
    return;
  }

  // Validate required field
  const question = args.question;
  if (typeof question !== "string" && !args.questions) {
    writeMsg({
      t: "result",
      id,
      content: [{ type: "text", text: 'ask_user requires "question" or "questions"' }],
      is_error: true,
    });
    return;
  }

  let result: ToolResult;

  // Check if it's the new format with explicit type
  if (args.type) {
    result = executeQuestion(args as unknown as QuestionSpec, session);
  } else if (args.questions && Array.isArray(args.questions)) {
    // Sequence shorthand
    result = executeSequence({
      type: "sequence",
      questions: args.questions as QuestionSpec[],
    }, session);
  } else {
    // Legacy format
    result = executeLegacy(args, session);
  }

  writeMsg({ t: "result", id, ...result });
}

// ── Main ─────────────────────────────────────────────────────────────────────

function main(): void {
  const hello = reader.readLine();
  if (hello === null) process.exit(1);
  
  const h = JSON.parse(hello) as { t?: string };
  if (h.t !== "hello") {
    console.error("expected hello");
    process.exit(1);
  }

  writeMsg({
    t: "hello",
    name: "kn9t-ask-user",
    capabilities: ["host_api"],
    tools: [
      {
        name: "ask_user",
        description: `Ask the human a question and wait for their reply.

Supports multiple question types:
- text: Free-form text input (default)
- choice: Single selection from options  
- multi: Multiple selection from options
- confirm: Yes/No confirmation
- sequence: Multiple questions in order

Examples:
  Simple text: {"question": "What's your name?"}
  
  Choice: {"type": "choice", "question": "Pick one", "options": [
    {"label": "Option A", "description": "First option"},
    {"label": "Option B", "description": "Second option"}
  ]}
  
  Multi-select: {"type": "multi", "question": "Select all that apply", "options": [...]}
  
  Confirm: {"type": "confirm", "question": "Delete these files?"}
  
  Sequence: {"questions": [
    {"type": "text", "question": "Project name?"},
    {"type": "choice", "question": "Language?", "options": [...]}
  ]}

Use for genuine ambiguity, not to avoid deciding.`,
        schema: {
          type: "object",
          properties: {
            type: {
              type: "string",
              enum: ["text", "choice", "multi", "confirm", "sequence"],
              description: "Question type. Default: text",
            },
            question: {
              type: "string",
              description: "The question to ask (required for text/choice/multi/confirm)",
            },
            header: {
              type: "string",
              description: "Short header/title (max 30 chars)",
            },
            options: {
              type: "array",
              items: {
                type: "object",
                properties: {
                  label: { type: "string", description: "Display text (1-5 words)" },
                  value: { type: "string", description: "Return value (default: label)" },
                  description: { type: "string", description: "Explanation of this option" },
                },
                required: ["label"],
              },
              description: "Options for choice/multi questions",
            },
            questions: {
              type: "array",
              description: "Sub-questions for sequence type",
            },
            allow_custom: {
              type: "boolean",
              description: "Allow custom input for choice questions",
            },
            placeholder: {
              type: "string",
              description: "Placeholder text for text input",
            },
            default: {
              description: "Default value",
            },
          },
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
    }
  }
}

try {
  main();
} catch (e) {
  console.error("kn9t-ask-user fatal:", e);
  process.exit(1);
}
