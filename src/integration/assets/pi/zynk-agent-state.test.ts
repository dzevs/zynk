// zynk fork — bun test suite for the STATE-ONLY pi integration extension in
// zynk-agent-state.ts. The old footer-receiver / receipt machinery (parseZynkFooter,
// eligibleZynkReceipt, recordZynkReceipt, the `pi.on("input")` transform-strip, etc.)
// was REMOVED: pi is now Zynk state-only like every other agent. The visible message
// HEADER is prepended server-side and is NEVER stripped by the receiver — there is no
// receiver in this extension anymore. `delivery_status` never auto-advances to
// `received` from a delivered/visible header; receipt remains a dormant, server-
// authoritative capability that nothing in this asset fires.
//
// These tests:
//   1. statically assert the receiver/footer/receipt surface is fully gone (no exports,
//      no `pi.on("input")`, no `action: "transform"`, no `zynk.message_received`, no
//      footer markers / hash helpers, no `node:crypto` import);
//   2. drive the state-only default export against a fake `pi` event API + a real Unix
//      socket server, asserting it emits the right `pane.report_agent` /
//      `pane.release_agent` JSON-RPC over the lifecycle hooks.

import { EventEmitter } from "node:events";
import { createServer } from "node:net";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, describe, beforeAll, afterAll } from "bun:test";

const ASSET_PATH = join(import.meta.dir, "zynk-agent-state.ts");
const ASSET_SRC = readFileSync(ASSET_PATH, "utf8");

// ---------------------------------------------------------------------------
// 1. Static guards: the receiver / footer / receipt surface must be GONE.
// ---------------------------------------------------------------------------

describe("receiver/footer/receipt surface is fully removed (state-only)", () => {
  test("no footer/receipt/transform tokens remain in the asset source", () => {
    const forbidden = [
      'pi.on("input"',
      'action: "transform"',
      "zynk.message_received",
      "parseZynkFooter",
      "eligibleZynkReceipt",
      "isValidZynkFooter",
      "verifyZynkBodyHash",
      "zynkSha256Hex",
      "isEligibleInputSource",
      "recordZynkReceipt",
      "sendReceiptRequest",
      "classifyReceiptOutcome",
      "parseReceiptResponse",
      "zynkBackoffMs",
      "zynkDedupAdd",
      "ZYNK_FOOTER_START_MARKER",
      "ZYNK_FOOTER_END_MARKER",
      "ZYNK_FOOTER_VERSION",
      "ZYNK_RETRYABLE_RECEIPT_CODES",
      "ZYNK_DEDUP_CAP",
      "--- zynk receipt footer v1 ---",
      "--- end zynk receipt footer ---",
      "node:crypto",
      "createHash",
    ];
    for (const token of forbidden) {
      expect(ASSET_SRC.includes(token)).toBe(false);
    }
  });

  test("the module exports nothing besides the default state-only hook", async () => {
    const mod = await import("./zynk-agent-state.ts");
    expect(typeof mod.default).toBe("function");
    expect(Object.keys(mod).sort()).toEqual(["default"]);
  });
});

// ---------------------------------------------------------------------------
// 2. State-only lifecycle: identity markers + behavior over a real socket.
// ---------------------------------------------------------------------------

describe("install markers + identity are preserved", () => {
  test("integration id stays pi and version is bumped to 4", () => {
    expect(ASSET_SRC).toContain("// ZYNK_INTEGRATION_ID=pi");
    expect(ASSET_SRC).toContain("// ZYNK_INTEGRATION_VERSION=4");
  });

  test("ZYNK_* env reads keep the ZYNK_* fallback", () => {
    expect(ASSET_SRC).toContain("process.env.ZYNK_ENV ?? process.env.ZYNK_ENV");
    expect(ASSET_SRC).toContain(
      "process.env.ZYNK_SOCKET_PATH ?? process.env.ZYNK_SOCKET_PATH",
    );
    expect(ASSET_SRC).toContain(
      "process.env.ZYNK_PANE_ID ?? process.env.ZYNK_PANE_ID",
    );
  });

  test("host-protocol source stays zynk:pi and report methods are state-only", () => {
    expect(ASSET_SRC).toContain('const source = "zynk:pi"');
    expect(ASSET_SRC).toContain("pane.report_agent");
    expect(ASSET_SRC).toContain("pane.release_agent");
    // session-ref reporting (report_agent_session via withSessionRef) is kept.
    expect(ASSET_SRC).toContain("agent_session_path: currentAgentSessionPath");
    expect(ASSET_SRC).toContain("agent_session_id: currentAgentSessionId");
  });
});

// A minimal fake of the pi extension API: `pi.on(name, handler)` for lifecycle
// hooks and `pi.events.on(name, handler)` for the zynk:blocked event channel.
function makeFakePi() {
  const onHandlers = new Map<string, (...args: any[]) => any>();
  const events = new EventEmitter();
  const pi = {
    on(name: string, handler: (...args: any[]) => any) {
      onHandlers.set(name, handler);
    },
    events,
  };
  return {
    pi,
    emit(name: string, ...args: any[]) {
      const h = onHandlers.get(name);
      return h ? h(...args) : undefined;
    },
    has(name: string) {
      return onHandlers.has(name);
    },
  };
}

// Stand up a real Unix-domain JSON-RPC sink: collect every newline-framed request,
// reply with a trivial ack so the asset's `sendRequest` resolves promptly.
function makeSocketSink() {
  const dir = mkdtempSync(join(tmpdir(), "zynk-pi-test-"));
  const sockPath = join(dir, "z.sock");
  const requests: any[] = [];
  const server = createServer((conn) => {
    let buf = "";
    conn.on("data", (chunk) => {
      buf += chunk.toString();
      let nl: number;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl);
        buf = buf.slice(nl + 1);
        if (line.trim().length === 0) continue;
        try {
          const req = JSON.parse(line);
          requests.push(req);
          conn.write(`${JSON.stringify({ id: req.id, result: { ok: true } })}\n`);
        } catch {
          // ignore non-JSON noise
        }
      }
    });
  });
  return { server, sockPath, requests };
}

function waitFor(pred: () => boolean, timeoutMs = 2000): Promise<void> {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const tick = () => {
      if (pred()) return resolve();
      if (Date.now() - start > timeoutMs) {
        return reject(new Error("waitFor timed out"));
      }
      setTimeout(tick, 5);
    };
    tick();
  });
}

describe("state-only lifecycle drives pane.report_agent / pane.release_agent", () => {
  let sink: ReturnType<typeof makeSocketSink>;
  let factory: (pi: any) => void;

  beforeAll(async () => {
    sink = makeSocketSink();
    await new Promise<void>((resolve) => sink.server.listen(sink.sockPath, resolve));
    // The asset reads env at module top-level; set it before (re)importing.
    process.env.ZYNK_ENV = "1";
    process.env.ZYNK_SOCKET_PATH = sink.sockPath;
    process.env.ZYNK_PANE_ID = "pane-test-1";
    // Make idle debounce immediate so the idle report is observable quickly.
    process.env.ZYNK_PI_IDLE_DEBOUNCE_MS = "0";
    const mod = await import(`./zynk-agent-state.ts?live=${Date.now()}`);
    factory = mod.default;
  });

  afterAll(() => {
    sink.server.close();
    delete process.env.ZYNK_ENV;
    delete process.env.ZYNK_SOCKET_PATH;
    delete process.env.ZYNK_PANE_ID;
    delete process.env.ZYNK_PI_IDLE_DEBOUNCE_MS;
  });

  test("registers the expected state-only hooks and NO input receiver", () => {
    const fake = makeFakePi();
    factory(fake.pi);
    expect(fake.has("session_start")).toBe(true);
    expect(fake.has("agent_start")).toBe(true);
    expect(fake.has("agent_end")).toBe(true);
    expect(fake.has("session_shutdown")).toBe(true);
    // The receiver hook is gone.
    expect(fake.has("input")).toBe(false);
  });

  test("agent_start reports working; agent_end reports idle; session_shutdown releases", async () => {
    const fake = makeFakePi();
    factory(fake.pi);

    const before = sink.requests.length;

    // session_start forces an initial (idle) report and records the session ref.
    fake.emit("session_start", {}, {
      sessionManager: {
        getSessionId: () => "sess-xyz",
        getSessionFile: () => "/tmp/pi/session.json",
      },
    });

    // agent_start -> working
    fake.emit("agent_start");
    await waitFor(() =>
      sink.requests
        .slice(before)
        .some((r) => r.method === "pane.report_agent" && r.params?.state === "working"),
    );

    // agent_end with no retryable error -> idle (debounce is 0ms in this suite)
    fake.emit("agent_end", { messages: [] });
    await waitFor(() =>
      sink.requests
        .slice(before)
        .some((r) => r.method === "pane.report_agent" && r.params?.state === "idle"),
    );

    // session_shutdown -> release_agent
    await fake.emit("session_shutdown");
    await waitFor(() =>
      sink.requests.slice(before).some((r) => r.method === "pane.release_agent"),
    );

    const reports = sink.requests
      .slice(before)
      .filter((r) => r.method === "pane.report_agent");
    // Every state report carries pi identity + the captured agent_session ref.
    for (const r of reports) {
      expect(r.params.agent).toBe("pi");
      expect(r.params.source).toBe("zynk:pi");
      expect(r.params.pane_id).toBe("pane-test-1");
      expect(r.params.agent_session_path).toBe("/tmp/pi/session.json");
    }
    // None of the requests is a receipt — that capability is not fired here.
    expect(sink.requests.every((r) => r.method !== "zynk.message_received")).toBe(true);
  });

  test("zynk:blocked active->inactive toggles blocked then clears", async () => {
    const fake = makeFakePi();
    factory(fake.pi);
    const before = sink.requests.length;

    fake.pi.events.emit("zynk:blocked", { active: true, label: "awaiting-approval" });
    await waitFor(() =>
      sink.requests.slice(before).some(
        (r) => r.method === "pane.report_agent" && r.params?.state === "blocked",
      ),
    );
    const blocked = sink.requests
      .slice(before)
      .find((r) => r.params?.state === "blocked");
    expect(blocked.params.message).toBe("awaiting-approval");

    fake.pi.events.emit("zynk:blocked", { active: false });
    await waitFor(() =>
      sink.requests.slice(before).some(
        (r) => r.method === "pane.report_agent" && r.params?.state === "idle",
      ),
    );
  });

  test("retryable provider error at agent_end holds working (not idle)", async () => {
    const fake = makeFakePi();
    factory(fake.pi);
    const before = sink.requests.length;

    fake.emit("agent_start");
    fake.emit("agent_end", {
      messages: [
        {
          role: "assistant",
          stopReason: "error",
          errorMessage: "provider returned error: overloaded",
        },
      ],
    });

    // The retry hold keeps the pane Working; assert no idle report follows for a beat.
    await waitFor(() =>
      sink.requests.slice(before).some(
        (r) => r.method === "pane.report_agent" && r.params?.state === "working",
      ),
    );
    await new Promise((r) => setTimeout(r, 30));
    const idleAfter = sink.requests
      .slice(before)
      .some((r) => r.method === "pane.report_agent" && r.params?.state === "idle");
    expect(idleAfter).toBe(false);
  });
});
