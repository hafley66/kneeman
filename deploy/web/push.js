// Lobby push opt-in, driven from INSIDE the game (the Network page in the pause menu) instead of a
// floating DOM chip. Loaded `defer` so it never blocks the Godot canvas boot.
//
// Nothing about the deployment is baked in here:
//   - the VAPID public key is fetched from `/vapid` at runtime (server derives it from its private
//     key), so rotating keys or changing hosts needs no client rebuild;
//   - the API origin is same-origin by default but can be overridden at build/deploy time by setting
//     window.PUSH_API_BASE (e.g. via the export head_include) for a split-host setup;
//   - the room comes from the `?room=` query, matching how the game itself reads it.
//
// The game reads `window.smashPush.label` each frame (while the Network page is open) and calls
// `window.smashPush.enable()` when the in-menu button is pressed, both via Godot's JavaScriptBridge.
(() => {
  const API = (window.PUSH_API_BASE || '').replace(/\/$/, ''); // '' => same origin
  const room = new URLSearchParams(location.search).get('room') || 'default';

  // The API the game drives. `label` is the human status the Network page mirrors; `enable` runs the
  // subscribe flow. Present on every browser so the game can feature-detect off `label`.
  const api = { label: '', enabled: false, enable };
  window.smashPush = api;

  if (!('serviceWorker' in navigator) || !('PushManager' in window)) {
    api.label = 'push unsupported on this browser';
    api.enable = () => {};
    return;
  }

  const b64ToBytes = (s) => {
    const pad = '='.repeat((4 - (s.length % 4)) % 4);
    const raw = atob((s + pad).replace(/-/g, '+').replace(/_/g, '/'));
    return Uint8Array.from(raw, (c) => c.charCodeAt(0));
  };

  async function enable() {
    api.label = 'subscribing…';
    try {
      const key = (await (await fetch(`${API}/vapid`)).json()).publicKey;
      if (!key) { api.label = 'push off (server has no VAPID key)'; return; }

      const reg = await navigator.serviceWorker.register('sw.js'); // scope = /game/
      await navigator.serviceWorker.ready;

      if ((await Notification.requestPermission()) !== 'granted') { api.label = 'notifications blocked'; return; }

      const sub =
        (await reg.pushManager.getSubscription()) ||
        (await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64ToBytes(key) }));

      const r = await fetch(`${API}/subscribe`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ room, subscription: sub.toJSON() }),
      });
      api.label = r.ok ? `pinging for '${room}'` : 'subscribe failed';
      api.enabled = r.ok;
    } catch (e) {
      console.error('[push]', e);
      api.label = 'push failed';
    }
  }

  // If already subscribed in this browser, re-register the (possibly new) room silently and reflect it.
  navigator.serviceWorker.getRegistration().then(async (reg) => {
    const sub = reg && (await reg.pushManager.getSubscription());
    if (!sub) return;
    fetch(`${API}/subscribe`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ room, subscription: sub.toJSON() }),
    }).catch(() => {});
    api.label = `pinging for '${room}'`;
    api.enabled = true;
  });
})();
