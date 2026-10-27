import * as MC from './core.js';
import { toast } from './ui.js';

export const S = MC.createStore(), HOST = location.host, KEY = 'minicore.token';
let ws = null, ready = false;
// Callbacks main.js binds: message(msg, effects), lost() once the socket is gone, closed() when it ended without our asking
// and render() when the state changed locally.
const on = {};
export const bind = callbacks => Object.assign(on, callbacks);
export const repaint = () => on.render();

export const tz = () => new Date().getTimezoneOffset();
export const hostNow = () => Date.now() + S.skew;
export const st = () => S.state.status;
export const cur = () => S.state.session || S.state.draft;
// An unconfirmed send that went back to the composer is no longer tracked.
export const abandon = id => S.intents.delete(id);
// Ids keep counting so a late reply never matches a new request.
export const resetStore = () => Object.assign(S, MC.createStore(), { nid: S.nid });
export const online = () => ready && ws && ws.readyState === 1;
export function send(msg) { ws.send(JSON.stringify(msg)); }
export function hangUp() {
  if (ws) { const s = ws; ws = null; s.onclose = s.onmessage = null; s.close(); lost(); }
}
export function open(first) {
  const s = ws = new WebSocket((location.protocol === 'https:' ? 'wss:' : 'ws:') + '//' + HOST + '/ws');
  s.onopen = () => s.send(JSON.stringify(first()));
  s.onmessage = e => {
    const msg = JSON.parse(e.data), fx = MC.reduce(S, msg, Date.now());
    if (fx.welcome) ready = true;
    on.message(msg, fx);
    if (fx.fs) { const cb = fsWait.get(msg.id); fsWait.delete(msg.id); cb(msg); }
    if (fx.result) { const cb = waits.get(msg.id); waits.delete(msg.id); cb(msg); pump(); }
  };
  s.onclose = () => { if (ws === s) { ws = null; lost(); on.closed(); } };
}
export const socketGone = () => !ws || ws.readyState > 1;
function lost() {
  ready = false;
  MC.disconnect(S);
  for (const cb of fsWait.values()) cb({ ok: false, error: 'io', message: 'Not connected' });
  fsWait.clear();
  failQueries();
  on.lost();
}
// ---- intents and requests ----
// `own` handles the ack where the request was made (a dialog shows its own error).
export function intent(spec, own) {
  if (!online()) { toast('Not connected', 'error'); return false; }
  const msg = MC.intent(S, spec, own);
  send(msg);
  repaint();
  return true;
}
export const pendingKind = k => !!MC.pending(S, k);
export const sending = () => pendingKind('submit') || pendingKind('queue');
const fsWait = new Map();
export function fsCall(body, cb) {
  if (!online()) return cb({ ok: false, error: 'io', message: 'Not connected' });
  const msg = MC.request(S, body);
  fsWait.set(msg.id, cb);
  send(msg);
}
// Queries wait for room (at most four in flight) and fail when the connection drops.
const waits = new Map(), queue = [];
export function ask(spec, cb) { queue.push([spec, cb]); pump(); }
export function pump() {
  while (queue.length && online() && MC.queryRoom(S)) {
    const [spec, cb] = queue.shift(), msg = MC.query(S, spec);
    waits.set(msg.id, cb);
    send(msg);
  }
}
function failQueries() {
  const all = [...waits.values(), ...queue.map(q => q[1])];
  waits.clear(); queue.length = 0;
  for (const cb of all) cb({ ok: false, error: 'Connection lost' });
}

