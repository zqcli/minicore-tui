import * as MC from './core.js';
import { S, cur, intent, online, pendingKind } from './net.js';
import { notify, prefs } from './platform.js';
import { markDenied } from './transcript.js';
import { $, announce, diffView, findSheet, h, hover, icon, inField, part, set, sheet, spin, toast, topModal } from './ui.js';

// ---- approvals and questions ----
let apprId = null, apprAt = 0, lastKey = 0;
export function syncApproval() {
  const a = S.state.interaction, m = findSheet('appr');
  if (!a) {
    // Answered elsewhere: a denied tool row says so.
    if (apprId && ap.call) markDenied(ap.call);
    apprId = null; ap.call = null;
    if (m) m.close(true);
    renderApprBar();
    return;
  }
  if (a.id !== apprId) {
    apprId = a.id; apprAt = Date.now();
    const t = [...S.blocks].reverse().find(b => b.type === 'tool' && b.state === 'pending');
    ap.call = t ? t.call_id : null;
    announce(a.kind === 'approval' ? 'Approval needed' : 'Question: ' + (a.prompt || ''));
    notify(a.kind === 'approval' ? 'Approval needed: ' + MC.approvalLine(a) : a.prompt || 'A question is waiting', S.sid);
    if (!m) openApproval(a);
  }
  renderApprBar();
}
const ap = { deny: false, reason: '', call: null };
function openApproval() {
  ap.deny = false; ap.reason = '';
  const m = sheet({
    key: 'appr', closeIcon: 'chevd', closeLabel: 'Minimize', footCls: 'col', deps: () => [S.state.interaction, pendingKind('answer'), online(), ap.deny],
    head: hid => {
      const a = S.state.interaction, t = a.kind === 'approval' && MC.approvalTitle(a);
      return h('div', 'ahead', h('div', 'eicon', icon(t ? 'shield' : 'info')), h('div', '',
        h('h2', { id: hid, tabindex: '-1', 'data-focus': true }, t ? [t.text, t.code ? [' ', h('code', '', t.code)] : ''] : a.prompt || 'Question'),
        t && a.prompt && h('p', '', a.prompt)));
    },
    body: () => {
      const a = S.state.interaction, p = MC.pending(S, 'answer');
      if (a.kind !== 'approval') return h('div', 'opts', MC.approvalOptions(a).map(o => h('button', { class: 'btn', 'data-k': 'o' + o.n, disabled: !online() || !!p, onclick: () => answer(o.choice) },
        p && p.spec.choice === o.choice ? spin() : h('b', '', String(o.n)), h('span', '', o.label))));
      const x = MC.approvalBody(a);
      return [x.cmd && [h('pre', 'cmdbox', '$ ' + x.cmd), h('div', 'where2', 'in ' + MC.tilde(cur().workspace))], x.diff && diffView(x.diff, prefs.wrap), x.text && h('pre', 'cmdbox', x.text)];
    },
    foot: () => {
      const a = S.state.interaction, p = MC.pending(S, 'answer'), dis = !online() || !!p;
      if (a.kind !== 'approval') return hover.matches && h('div', 'akeys', 'Press 1–' + a.options.length + ' to answer');
      const ans = (o, cls, fn) => h('button', { class: 'btn ' + cls, 'data-k': o.choice, disabled: dis, onclick: fn || (() => answer(o.choice)) }, p && p.spec.choice === o.choice && spin(), o.label);
      const [allow, session, deny] = MC.approvalOptions(a);
      if (ap.deny) return [h('form', { class: 'reason', onsubmit: e => { e.preventDefault(); answer('deny', ap.reason.trim()); } },
        h('input', { class: 'tf', id: 'why', 'data-k': 'why', value: ap.reason, placeholder: 'Tell the agent why (optional)', 'aria-label': 'Reason for denying', autocomplete: 'off', enterKeyHint: 'send', oninput: e => { ap.reason = e.target.value; } })),
        h('div', 'row2', h('button', { class: 'btn', onclick: () => { ap.deny = false; m.refresh(); } }, 'Back'), ans(deny, 'dangerfill', () => answer('deny', ap.reason.trim())))];
      return [ans(allow, 'primary'), session && ans(session, ''), ans({ ...deny, label: deny.label + '…' }, 'danger', () => { ap.deny = true; m.refresh(); m.el.querySelector('#why').focus(); }),
        hover.matches && h('div', 'akeys', 'Y allow · A allow for this session · N deny')];
    },
    onClose: () => renderApprBar(),
  });
}
// Only a denial is recorded on its tool row, with the reason: it explains why the tool did not run.
function answer(choice, message) {
  const a = S.state.interaction, call = ap.call;
  if (!a || pendingKind('answer')) return;
  intent(MC.intents.answer(a.id, choice, message || undefined), ack => {
    if (!ack.ok) return toast(MC.errorText(ack.error), ack.error === 'stale' ? null : 'error');
    if (call && choice === 'deny') markDenied(call, 'Denied' + (message ? ': “' + message + '”' : ''));
  });
}
const apprPart = part($('apprbar'), b => b && [icon('hand', 'sm'), h('span', '', b), h('button', { class: 'btn sm', onclick: () => openApproval() }, 'Review')]);
function renderApprBar() {
  const a = S.state && S.state.interaction, show = !!a && !findSheet('appr');
  set($('apprbar'), 'hidden', !show);
  apprPart(show ? (a.kind === 'approval' ? 'Approval needed · ' + MC.approvalLine(a) : 'Question · ' + (a.prompt || '')) : null);
}
document.addEventListener('keydown', e => {
  const m = topModal(), a = S.state && S.state.interaction, armedFrom = Math.max(apprAt, lastKey);
  lastKey = Date.now();
  if (!a || !m || m.key !== 'appr' || e.repeat) return;
  const c = MC.approvalKey(a, e.key, e.ctrlKey || e.metaKey || e.altKey, armedFrom, lastKey, inField(e.target));
  if (c != null) { e.preventDefault(); answer(c); }
}, true);

