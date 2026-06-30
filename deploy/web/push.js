// Lobby push opt-in. Loaded `defer` so it never blocks the Godot canvas boot.
//
// Nothing about the deployment is baked in here:
//   - the VAPID public key is fetched from `/vapid` at runtime (server derives it from its private
//     key), so rotating keys or changing hosts needs no client rebuild;
//   - the API origin is same-origin by default but can be overridden at build/deploy time by setting
//     window.PUSH_API_BASE (e.g. via the export head_include) for a split-host setup;
//   - the room comes from the `?room=` query, matching how the game itself reads it.

(() => {
  const API = (window.PUSH_API_BASE || '').replace(/\/$/, ''); // '' => same origin
  const room = new URLSearchParams(location.search).get('room') || 'default';

  if (!('serviceWorker' in navigator) || !('PushManager' in window)) return;

  const b64ToBytes = (s) => {
    const pad = '='.repeat((4 - (s.length % 4)) % 4);
    const raw = atob((s + pad).replace(/-/g, '+').replace(/_/g, '/'));
    return Uint8Array.from(raw, (c) => c.charCodeAt(0));
  };

  // Top-left, tucked just under the in-game status chip (which sits at ~14,10). Keeping it up here
  // leaves the bottom corners clear for the mobile stick + buttons.
  const btn = document.createElement('button');
  btn.textContent = '🔔 ping me on lobby';
  btn.style.cssText =
    'position:fixed;left:14px;top:50px;z-index:9999;padding:6px 10px;border:0;border-radius:8px;' +
    'background:#1b2a4a;color:#cfe0ff;font:600 12px system-ui;cursor:pointer;opacity:.85';
  const setLabel = (t, off) => { btn.textContent = t; btn.style.opacity = off ? '.5' : '.85'; };

  async function enable() {
    setLabel('…', true);
    try {
      const key = (await (await fetch(`${API}/vapid`)).json()).publicKey;
      if (!key) { setLabel('push off (server)', true); btn.disabled = true; return; }

      const reg = await navigator.serviceWorker.register('sw.js'); // scope = /game/
      await navigator.serviceWorker.ready;

      if ((await Notification.requestPermission()) !== 'granted') { setLabel('🔔 ping me on lobby'); return; }

      const sub =
        (await reg.pushManager.getSubscription()) ||
        (await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64ToBytes(key) }));

      const r = await fetch(`${API}/subscribe`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ room, subscription: sub.toJSON() }),
      });
      setLabel(r.ok ? `✅ pinging for '${room}'` : 'subscribe failed', !r.ok);
    } catch (e) {
      console.error('[push]', e);
      setLabel('push failed', true);
    }
  }

  btn.addEventListener('click', enable);
  // If already subscribed in this browser, re-register the (possibly new) room silently and reflect it.
  navigator.serviceWorker.getRegistration().then(async (reg) => {
    const sub = reg && (await reg.pushManager.getSubscription());
    if (!sub) return;
    fetch(`${API}/subscribe`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ room, subscription: sub.toJSON() }),
    }).catch(() => {});
    setLabel(`✅ pinging for '${room}'`);
  });

  addEventListener('DOMContentLoaded', () => document.body.appendChild(btn));
})();
