import * as MC from './core.js';
import { $, closeAll, fill, h, icon, spin, toast } from './ui.js';

// ---- net ----
export const S = MC.createStore(), HOST = location.host, KEY = 'minicore.token';
let ws = null, ready = false;
// Callbacks main.js binds: message(msg, effects), lost() once the socket is gone, render() when the state changed locally
// and reset() when the signed-out device forgets what it showed.
const on = {};
export const bind = callbacks => Object.assign(on, callbacks);
export const repaint = () => on.render();

export const tz = () => new Date().getTimezoneOffset();
export const hostNow = () => Date.now() + S.skew;
export const turnStatus = () => S.state.status;
export const viewed = () => S.state.session || S.state.draft;
// An unconfirmed send that went back to the composer is no longer tracked.
export const abandon = id => S.intents.delete(id);
// Ids keep counting so a late reply never matches a new request.
const resetStore = () => Object.assign(S, MC.createStore(), { nid: S.nid });
export const online = () => ready && ws && ws.readyState === 1;
export function send(msg) { ws.send(JSON.stringify(msg)); }
function hangUp() {
  if (ws) { const s = ws; ws = null; s.onclose = s.onmessage = null; s.close(); lost(); }
}
function open(first) {
  const s = ws = new WebSocket((location.protocol === 'https:' ? 'wss:' : 'ws:') + '//' + HOST + '/ws');
  s.onopen = () => s.send(JSON.stringify(first()));
  s.onmessage = e => {
    const msg = JSON.parse(e.data), fx = MC.reduce(S, msg, Date.now());
    if (fx.welcome) ready = true;
    on.message(msg, fx);
    if (fx.fs) { const cb = fsWait.get(msg.id); fsWait.delete(msg.id); cb(msg); }
    if (fx.result) { const cb = waits.get(msg.id); waits.delete(msg.id); cb(msg); pump(); }
  };
  s.onclose = () => { if (ws === s) { ws = null; lost(); closed(); } };
}
const socketGone = () => !ws || ws.readyState > 1;
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

// ---- connection ----
let halted = false, attempt = 0, retryT = 0, how = 'hello', screen = '';
export const isHalted = () => halted;
export const isMain = () => screen === 'main';
const teardown = () => { clearTimeout(retryT); hangUp(); };
function start(first, kind) { teardown(); how = kind; open(first); }

// ---- screens ----
function show(name) {
  screen = name;
  $('signin').hidden = name !== 'signin';
  $('gate').hidden = name !== 'gate';
  $('app').hidden = name !== 'main';
  if (name !== 'main') closeAll();
}
function gateScreen(g) {
  const eicon = h('div', 'eicon' + (g.icon === 'warn' ? ' warn' : ''), icon(g.icon, 'lg'));
  fill($('gate'), h('div', 'col', g.spinner ? h('img', { class: 'appicon', src: '/icon.svg', alt: '' }) : eicon, h('h1', '', g.title), g.text && h('p', 'sub', g.text),
    g.spinner && spin(), g.action && h('div', 'gate-acts', h('button', { class: 'btn primary block', onclick: () => gateAction(g.action) }, g.label),
      g.signin && h('button', { class: 'btn ghost', onclick: signOut }, 'Sign in with a password'))));
  show('gate');
}
function gateAction(a) {
  if (a === 'reload') location.reload();
  else { halted = false; attempt = 0; connecting(); connect(); }
}
function connecting() {
  if (S.state) return show('main');
  gateScreen(attempt < 2
    ? { spinner: true, title: 'Connecting…', text: HOST }
    : { icon: 'cloudoff', title: MC.unreachable(HOST), text: 'Check that MiniCore is running with remote control on and that this device can reach your computer. Retrying…', action: 'retry', label: 'Retry now', signin: true });
}

// ---- sign-in ----
let lockUntil = 0, lockT = 0;
function signinScreen(o = {}) {
  show('signin');
  $('shost').textContent = HOST;
  const note = $('snote');
  note.hidden = !o.notice;
  if (o.notice) note.lastChild.textContent = o.notice;
  signinBusy(false);
  if (o.locked) lock(o.locked);
  else if (o.error) signinError(o.error, o);
  else if (!lockT) signinError('');
  const pw = $('pw');
  if (o.shake) { const f = $('pwf'); f.classList.remove('shake'); void f.offsetWidth; f.classList.add('shake'); }
  if (o.error || o.shake) { pw.focus(); pw.select(); } else if (!o.notice) pw.focus({ preventScroll: true });
}
function signinError(text, o = {}) {
  const e = $('serr');
  $('pwf').classList.toggle('bad', !!text && !o.warn && !o.retry);
  e.hidden = !text;
  e.className = 'ferr' + (o.warn ? ' warn' : '');
  fill(e, text && icon(o.warn ? 'info' : 'warn', 'sm'), h('span', '', text), o.retry && h('button', { onclick: doLogin }, 'Retry'));
}
function signinBusy(on) {
  const b = $('sgo');
  b.firstChild.hidden = !on;
  b.lastChild.textContent = on ? 'Connecting…' : 'Connect';
  b.disabled = on || Date.now() < lockUntil;
  $('pw').readOnly = on;
}
function lock(sec) {
  lockUntil = Date.now() + sec * 1000;
  clearInterval(lockT);
  const step = () => {
    const left = Math.ceil((lockUntil - Date.now()) / 1000);
    if (left <= 0) { clearInterval(lockT); lockT = 0; lockUntil = 0; signinError('You can try again now.', { warn: true }); signinBusy(false); return; }
    signinError(MC.lockText(left), { warn: true });
    $('sgo').disabled = true;
  };
  lockT = setInterval(step, 1000);
  step();
}
function doLogin() {
  const pw = $('pw').value;
  if (!pw.trim()) { signinError('Enter the password shown on your computer.'); $('pw').focus(); return; }
  if (Date.now() < lockUntil) return;
  signinError('');
  signinBusy(true);
  halted = false;
  start(() => MC.login(pw, MC.deviceName(navigator.userAgent, navigator.maxTouchPoints)), 'login');
}
$('sf').addEventListener('submit', e => { e.preventDefault(); doLogin(); });
$('pw').addEventListener('input', () => { if (!lockT) signinError(''); });
$('pweye').addEventListener('click', () => {
  const pw = $('pw'), b = $('pweye'), vis = pw.type === 'password';
  pw.type = vis ? 'text' : 'password';
  b.setAttribute('aria-pressed', vis);
  b.setAttribute('aria-label', vis ? 'Hide password' : 'Show password');
  b.firstChild.firstChild.setAttribute('href', vis ? '#i-eyeoff' : '#i-eye');
});
export function signOut() {
  localStorage.removeItem(KEY);
  halted = true;
  teardown();
  resetSession();
  signinScreen({ notice: 'Signed out of this device. Enter the password to connect again.' });
}

function connect() {
  const token = localStorage.getItem(KEY);
  if (!token) return signinScreen();
  start(() => MC.hello(token), 'hello');
  repaint();
}
function closed() {
  if (halted) return;
  if (how === 'login') { signinBusy(false); signinError(MC.unreachable(HOST), { retry: true }); return; }
  attempt++;
  if (!S.state) connecting();
  repaint();
  retryT = setTimeout(connect, MC.backoff(attempt - 1));
}
export function wake(force) {
  if (halted || screen === 'signin' || !localStorage.getItem(KEY)) return;
  if (force || socketGone()) { clearTimeout(retryT); attempt = 0; connect(); }
}
function onGate(g) {
  halted = true;
  teardown();
  if (g.forget) { localStorage.removeItem(KEY); resetSession(); }
  if (g.view === 'signin') signinScreen(g);
  else gateScreen(g);
}
function resetSession() {
  resetStore();
  on.reset();
}
// The connection part of a message: the token to keep, the welcome, and the gate screens. False when the message ended in one.
export function handshake(fx) {
  if (fx.token) { localStorage.setItem(KEY, fx.token); how = 'hello'; }
  if (fx.welcome) { attempt = 0; signinBusy(false); }
  if (fx.gate) { onGate(fx.gate); return false; }
  if (fx.snapshot && screen !== 'main') { show('main'); $('pw').value = ''; }
  return true;
}
export function boot() {
  if (localStorage.getItem(KEY)) { connecting(); connect(); } else signinScreen();
}
