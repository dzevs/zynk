// installed by zynk
// managed by zynk; reinstalling or updating the integration overwrites this file.
// add custom hooks/plugins beside this file instead of editing it.
// ZYNK_INTEGRATION_ID=opencode
// ZYNK_INTEGRATION_VERSION=11

import net from "node:net";

const SOURCE = "zynk:opencode";
const AGENT = "opencode";
let reportSeq = Date.now() * 1000;
let requestChain = Promise.resolve();
let reportedRootSessionID;

// Track each child session against the parent that spawned it, so a child's events
// report under the ROOT session that owns them. Reporting no session at all would
// replace the pane's session anchor instead of preserving it: a full-lifecycle report
// without a session id clears the stored session triple, which breaks a pending
// receipt for the root and lets another client's child drive this pane.
const childParents = new Map();
const CHILD_EVENT_STATES = new Map([
  ["permission.asked", "blocked"],
  ["question.asked", "blocked"],
  ["permission.replied", "working"],
  ["question.replied", "working"],
  ["question.rejected", "working"],
]);

function nextReportSeq() {
  reportSeq += 1;
  return reportSeq;
}

function sessionIDFromProperties(properties) {
  return typeof properties?.sessionID === "string" && properties.sessionID
    ? properties.sessionID
    : undefined;
}

const SESSION_STATE_BY_STATUS = new Map([
  ["idle", "idle"],
  ["active", "working"],
  ["busy", "working"],
  ["pending", "working"],
  ["retry", "working"],
  ["running", "working"],
  ["streaming", "working"],
  ["working", "working"],
]);

function stateFromSessionStatus(status) {
  const kind = typeof status === "string" ? status : status?.type;
  return typeof kind === "string"
    ? SESSION_STATE_BY_STATUS.get(kind.toLowerCase())
    : undefined;
}

function request(method, params) {
  const pending = requestChain.then(() => requestOnce(method, params));
  requestChain = pending.catch(() => {});
  return pending;
}

function requestOnce(method, params) {
  const paneId = process.env.ZYNK_PANE_ID ?? process.env.ZYNK_PANE_ID;
  const socketPath = process.env.ZYNK_SOCKET_PATH ?? process.env.ZYNK_SOCKET_PATH;

  if (!paneId || !socketPath) {
    return Promise.resolve();
  }

  const requestId = `${SOURCE}:${Date.now()}:${Math.floor(Math.random() * 1_000_000)
    .toString()
    .padStart(6, "0")}`;
  const request = {
    id: requestId,
    method,
    params: {
      pane_id: paneId,
      source: SOURCE,
      agent: AGENT,
      seq: nextReportSeq(),
      ...params,
    },
  };

  return new Promise((resolve) => {
    const client = net.createConnection(socketPath, () => {
      client.write(`${JSON.stringify(request)}\n`);
    });

    const finish = () => {
      client.destroy();
      resolve();
    };

    client.setTimeout(500, finish);
    client.on("data", finish);
    client.on("error", finish);
    client.on("end", finish);
    client.on("close", resolve);
  });
}

function reportSession(sessionID) {
  if (!sessionID) {
    return Promise.resolve();
  }
  return request("pane.report_agent_session", { agent_session_id: sessionID });
}

// The owning root of `sessionID`: walk the recorded parent links up to the topmost
// KNOWN ancestor -- the first id that is not itself a tracked child. If the chain runs
// past what this process has seen, that ancestor is returned as-is; the server compares
// it against the root this pane selected and drops anything else, so an incomplete chain
// costs a filtered report rather than a stolen anchor. A cycle returns undefined and the
// caller reports nothing rather than guessing a root.
function rootSessionIDFor(sessionID) {
  const visited = new Set();
  let current = sessionID;
  while (!visited.has(current)) {
    visited.add(current);
    const parent = childParents.get(current);
    if (!parent) {
      return current;
    }
    current = parent;
  }
  return undefined;
}

function reportState(state, sessionID) {
  const params = { state };
  if (sessionID) {
    reportedRootSessionID = sessionID;
    params.agent_session_id = sessionID;
  }
  return request("pane.report_agent", params);
}

export const ZynkAgentStatePlugin = async () => {
  if (
    (process.env.ZYNK_ENV ?? process.env.ZYNK_ENV) !== "1" ||
    !(process.env.ZYNK_SOCKET_PATH ?? process.env.ZYNK_SOCKET_PATH) ||
    !(process.env.ZYNK_PANE_ID ?? process.env.ZYNK_PANE_ID)
  ) {
    return {};
  }

  return {
    "chat.message": async ({ sessionID }) => {
      if (sessionID && childParents.has(sessionID)) {
        return;
      }
      await reportState("working", sessionID);
    },
    event: async ({ event }) => {
      const type = event?.type;
      const properties = event?.properties ?? {};
      const sessionID = sessionIDFromProperties(properties);

      const info = properties.info;
      if (info?.id && info.parentID) {
        childParents.set(info.id, info.parentID);
      }
      if (sessionID && childParents.has(sessionID)) {
        const state = CHILD_EVENT_STATES.get(type);
        if (state) {
          const root = rootSessionIDFor(sessionID);
          if (root) {
            await reportState(state, root);
          }
        }
        return;
      }

      switch (type) {
        case "session.created":
          // Creation is server-global, so an attached client may own it. The
          // TUI plugin separately reports the root selected in this pane.
          reportedRootSessionID = sessionID;
          break;
        case "session.updated":
          if (sessionID && sessionID !== reportedRootSessionID) {
            await reportSession(sessionID);
          }
          break;
        case "session.status": {
          const state = stateFromSessionStatus(properties.status);
          if (state) {
            await reportState(state, sessionID);
          } else {
            await reportSession(sessionID);
          }
          break;
        }
        case "tool.execute.before":
        case "tool.execute.after":
        case "permission.replied":
        case "question.replied":
        case "question.rejected":
        case "session.compacted":
          await reportState("working", sessionID);
          break;
        case "permission.asked":
        case "question.asked":
        case "session.error":
          await reportState("blocked", sessionID);
          break;
        case "session.idle":
          await reportState("idle", sessionID);
          break;
        case "session.deleted":
          break;
        default:
          break;
      }
    },
  };
};
