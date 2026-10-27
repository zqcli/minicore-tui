import * as MC from './core.js';
import { HOST, KEY, S, hangUp, open, repaint, resetStore, socketGone } from './net.js';
import { resetTranscript } from './transcript.js';
import { $, actions, closeAll, fill, h, icon, spin } from './ui.js';

// The sign-in and status screens and the connection's life: connect, retry with backoff, wake, sign out.
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
function signOut() {
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
export function closed() {
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
  resetTranscript();
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
actions.signOut = signOut;
