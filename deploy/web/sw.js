// Service worker for lobby push. Deliberately tiny: no fetch handler, no caching, no app-shell.
// The only reason this SW exists is to receive `push` while the tab is closed and to focus the
// game on click. Anything heavier here would just add startup cost to every page load.

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));

self.addEventListener('push', (e) => {
  let d = {};
  try { d = e.data.json(); } catch (_) {}
  const title = d.title || '🥊 lobby';
  e.waitUntil(self.registration.showNotification(title, {
    body: d.body || 'someone is waiting to fight',
    tag: d.room || 'default',          // collapse repeats per room
    data: { room: d.room || '' },
    renotify: true,
  }));
});

self.addEventListener('notificationclick', (e) => {
  e.notification.close();
  const room = e.notification.data && e.notification.data.room;
  const url = room ? `./?room=${encodeURIComponent(room)}` : './';
  e.waitUntil((async () => {
    const all = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
    for (const c of all) {
      if (c.url.includes('/game/')) { c.focus(); return; }   // already open -> just focus
    }
    return self.clients.openWindow(url);
  })());
});
