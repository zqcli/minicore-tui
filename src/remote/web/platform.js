import { S, st } from './net.js';
import { actions, refreshSheets } from './ui.js';

const stored = JSON.parse(localStorage.getItem('minicore.prefs') || '{}');
let enterSet = 'enter' in stored;
export const prefs = Object.assign({ theme: 'system', text: 1, wrap: true, thinking: false, enter: matchMedia('(hover: hover) and (pointer: fine)').matches, wake: false, notify: false }, stored);
let installPrompt = null;
export function setPref(k, v) {
  prefs[k] = v; enterSet ||= k === 'enter'; savePrefs(); applyTheme();
  if (k === 'wrap' || k === 'thinking') actions.redraw();
  if (k === 'wake') syncWake();
  refreshSheets();
}
export const installable = () => !!installPrompt;
export function installApp() { installPrompt.prompt(); installPrompt = null; refreshSheets(); }

const savePrefs = () => localStorage.setItem('minicore.prefs', JSON.stringify(enterSet ? prefs : { ...prefs, enter: undefined }));

// While the page is hidden: approvals and finished turns through the service worker, which brings the app back.
export function notify(text, id) {
  if (!document.hidden || !prefs.notify || typeof Notification === 'undefined' || Notification.permission !== 'granted') return;
  navigator.serviceWorker.ready.then(r => r.showNotification('MiniCore', { body: text, tag: id || 'minicore', data: { session: id }, icon: '/icon-192.png' }));
}
export const standalone = () => matchMedia('(display-mode: standalone)').matches || navigator.standalone === true;
// The screen stays on while this view's turn runs and the page is visible, when the setting asks for it.
let wakeLock = null;
export function syncWake() {
  const want = prefs.wake && !!S.state && !!(st().busy || st().compacting) && !document.hidden && 'wakeLock' in navigator;
  if (want && !wakeLock) { wakeLock = 1; navigator.wakeLock.request('screen').then(l => { wakeLock = l; if (!prefs.wake) syncWake(); }, () => { wakeLock = null; }); }
  else if (!want && wakeLock && wakeLock.release) { wakeLock.release(); wakeLock = null; }
}

addEventListener('beforeinstallprompt', e => { e.preventDefault(); installPrompt = e; refreshSheets(); });
addEventListener('appinstalled', () => { installPrompt = null; refreshSheets(); });
