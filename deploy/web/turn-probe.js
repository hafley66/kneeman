// Browser-side TURN/ICE diagnostics. Wraps window.RTCPeerConnection BEFORE Godot's wasm loads so
// every peer connection Godot creates reports its `icecandidateerror` events to the relay's /ev sink.
// This is the authoritative answer to "why did no relay candidate form": the browser hands us the
// TURN server url + the STUN/TURN error code (401 auth, 403 forbidden, 300/701 timeout, 600 unreachable)
// that ICE hit while gathering. Loads with a plain (non-defer) script tag so it patches the global
// before anything constructs a PeerConnection. Fire-and-forget via sendBeacon; never blocks.
(function () {
  var Native = window.RTCPeerConnection || window.webkitRTCPeerConnection;
  if (!Native) return;
  var t0 = (window.performance && performance.now()) ? performance.now() : Date.now();

  function beacon(obj) {
    try {
      obj.sid = "jsprobe";
      obj.cs = Math.round(((window.performance && performance.now()) ? performance.now() : Date.now()) - t0);
      navigator.sendBeacon("/ev", JSON.stringify(obj));
    } catch (e) {}
  }

  function Wrapped(cfg) {
    var pc = new Native(cfg);
    try {
      // Count how many TURN servers the config actually carried into the browser (sanity vs client log).
      var turns = 0, servers = (cfg && cfg.iceServers) || [];
      for (var i = 0; i < servers.length; i++) {
        var u = servers[i].urls;
        u = Array.isArray(u) ? u : [u];
        for (var j = 0; j < u.length; j++) if (("" + u[j]).indexOf("turn:") === 0 || ("" + u[j]).indexOf("turns:") === 0) turns++;
      }
      beacon({ ev: "ice_pc", turns: turns });
      pc.addEventListener("icecandidateerror", function (e) {
        beacon({ ev: "ice_err", url: e.url || "", code: e.errorCode || 0, txt: ("" + (e.errorText || "")).slice(0, 140), addr: (e.address || "") + ":" + (e.port || 0) });
      });
      pc.addEventListener("iceconnectionstatechange", function () {
        if (pc.iceConnectionState === "failed" || pc.iceConnectionState === "connected") beacon({ ev: "ice_state", st: pc.iceConnectionState });
      });
    } catch (e) {}
    return pc;
  }
  Wrapped.prototype = Native.prototype;
  if (Native.generateCertificate) Wrapped.generateCertificate = Native.generateCertificate.bind(Native);
  window.RTCPeerConnection = Wrapped;
  if (window.webkitRTCPeerConnection) window.webkitRTCPeerConnection = Wrapped;
})();
