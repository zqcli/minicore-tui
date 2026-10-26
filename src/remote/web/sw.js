'use strict';
// Network first, so the next online load always uses what the host serves now; the cache only opens the app while
// the host is unreachable. The WebSocket and other origins are never touched.
const CACHE = 'minicore-3';
const SHELL = ['/', '/manifest.webmanifest', '/icon.svg', '/icon-192.png', '/icon-512.png', '/icon-maskable.png', '/apple-touch-icon.png'];

addEventListener('install', e => {
  skipWaiting();
  e.waitUntil(caches.open(CACHE).then(c => c.addAll(SHELL)));
});
addEventListener('activate', e => e.waitUntil(caches.keys()
  .then(keys => Promise.all(keys.filter(k => k !== CACHE).map(k => caches.delete(k))))
  .then(() => clients.claim())));
addEventListener('fetch', e => {
  const url = new URL(e.request.url);
  if (e.request.method !== 'GET' || url.origin !== location.origin || url.pathname === '/ws') return;
  // Keyed by path: the server ignores the query string too.
  const key = url.pathname, cached = () => caches.match(key);
  e.respondWith(fetch(e.request).then(r => {
    if (r.ok) {
      const copy = r.clone();
      e.waitUntil(caches.open(CACHE).then(c => c.put(key, copy)));
    }
    // A tunnel error page (5xx) while the host sleeps must not replace the app; redirects such as an
    // access-proxy sign-in pass through.
    if (r.status < 500) return r;
    return cached().then(c => c || r);
  }, () => cached().then(c => c || Response.error())));
});
// A notification tap focuses the app (opening it if needed) and asks it to show that session.
addEventListener('notificationclick', e => {
  e.notification.close();
  const id = e.notification.data && e.notification.data.session;
  e.waitUntil(clients.matchAll({ type: 'window' }).then(list => {
    const c = list[0];
    if (!c) return clients.openWindow('/');
    c.postMessage({ open: id });
    return c.focus();
  }));
});
