// minimap.js - the navigation panel of sdkapp/settings.html, a small live /navmap.
//
// Polls /api-navmap/snapshot for the dashboard's ?serial= and draws the answer the way
// crates/wirepod-server/src/navmap/navmap.html does: nose-up, every quad filled with
// the colour the robot sent, and his pose as an oriented triangle. The full page stays
// the place to zoom, pan and read details, and the panel links to it.
//
// Every snapshot renews the 15 s lease on his map feed and touches his idle timer, so
// the panel polls every 2 s, only while the tab is visible, and never has two requests
// in flight. It shares nothing with vectorbrain.js, whose poller keeps its timer
// running while the tab is hidden.

(function () {
  "use strict";

  var POLL_MS = 2000;
  // A request that never settles would hold the one-at-a-time slot for good.
  var FETCH_TIMEOUT_MS = 8000;
  var KNOWN_STATUS = { starting: true, waiting_for_map: true, streaming: true };
  // NavNodeContentType numbers behind each legend entry, named as navmap.html names them.
  var KINDS = [
    { key: "clear", id: "vbMapSwClear", contents: [1, 2] },
    { key: "obstacle", id: "vbMapSwObstacle", contents: [3, 4, 5, 6] },
    { key: "cliff", id: "vbMapSwCliff", contents: [7] },
    { key: "unknown", id: "vbMapSwUnknown", contents: [0] }
  ];
  // The panel's ground, --vb-page in style.css, and the full page's marker colours.
  var BG = "#0e0f12";
  var AMBER = "#E8A322";
  var TEAL = "#0FB6C2";
  var OUTLINE = "rgba(95,112,120,0.8)";

  // Char codes keep this file pure ASCII; settings.html declares no charset.
  var MIDDOT = String.fromCharCode(183);
  var THETA = String.fromCharCode(952);
  var DEG = String.fromCharCode(176);

  var serial = "";
  try {
    serial = new URLSearchParams(window.location.search).get("serial") || "";
  } catch (e) {
    serial = "";
  }

  var canvas = null;
  var ctx = null;
  var state = { map: null, robot: null, status: "", error: "", mapKey: null };
  var groups = [];
  var swatches = KINDS.map(function () {
    return null;
  });
  var poll = { interval: null, busy: false };

  function el(id) {
    return document.getElementById(id);
  }

  function setText(node, text) {
    if (node && node.textContent !== text) {
      node.textContent = text;
    }
  }

  function isNum(v) {
    return typeof v === "number" && isFinite(v);
  }

  // rgba is packed 0xRRGGBBAA and reaches 2^32, so shift with >>> to stay unsigned.
  function rgbaCss(c) {
    var r = (c >>> 24) & 255;
    var g = (c >>> 16) & 255;
    var b = (c >>> 8) & 255;
    var a = c & 255;
    return "rgba(" + r + "," + g + "," + b + "," + (a / 255).toFixed(3) + ")";
  }

  // ---------------------------------------------------------------- snapshot

  function fetchSnapshot() {
    var ctl = typeof AbortController === "function" ? new AbortController() : null;
    var timer = ctl
      ? setTimeout(function () {
          ctl.abort();
        }, FETCH_TIMEOUT_MS)
      : null;
    var url = "/api-navmap/snapshot?serial=" + encodeURIComponent(serial);
    var opts = { cache: "no-store" };
    if (ctl) {
      opts.signal = ctl.signal;
    }
    return fetch(url, opts)
      .then(
        function (res) {
          if (!res.ok) {
            throw new Error("HTTP " + res.status);
          }
          return res.json().catch(function () {
            throw new Error("bad reply from the server");
          });
        },
        function () {
          throw new Error(
            ctl && ctl.signal.aborted ? "no answer from the server" : "server unreachable"
          );
        }
      )
      .then(
        function (snap) {
          clearTimeout(timer);
          return snap;
        },
        function (err) {
          clearTimeout(timer);
          throw err;
        }
      );
  }

  function applySnapshot(snap) {
    if (!snap || typeof snap !== "object") {
      throw new Error("bad reply from the server");
    }
    state.error = "";
    state.status = typeof snap.status === "string" ? snap.status : "";
    var map = snap.map;
    applyMap(map && map.root && map.root.size_mm > 0 ? map : null);
    var r = snap.robot;
    state.robot = r && isNum(r.x) && isNum(r.y) ? r : null;
    render();
  }

  function applyFailure(err) {
    // The last map and pose stay on screen under the error, as on the full page.
    state.error = (err && err.message) || "page error";
    render();
  }

  function applyMap(map) {
    state.map = map;
    var key = map
      ? map.origin_id + "/" + map.received_ms + "/" + (Array.isArray(map.quads) ? map.quads.length : 0)
      : null;
    if (key === state.mapKey) {
      return;
    }
    state.mapKey = key;
    indexQuads(map && Array.isArray(map.quads) ? map.quads : []);
  }

  // Groups quads by colour for one fill per colour, and finds each legend kind's most
  // common colour, as navmap.html's indexQuads does.
  function indexQuads(quads) {
    var byColour = {};
    var order = [];
    var tallies = KINDS.map(function () {
      return {};
    });
    for (var i = 0; i < quads.length; i++) {
      var q = quads[i];
      if (!Array.isArray(q) || q.length < 5 || !(q[2] > 0)) {
        continue;
      }
      var rgba = q[4];
      if (!byColour[rgba]) {
        byColour[rgba] = [];
        order.push(rgba);
      }
      byColour[rgba].push(q[0], q[1], q[2]);
      for (var k = 0; k < KINDS.length; k++) {
        if (KINDS[k].contents.indexOf(q[3]) >= 0) {
          tallies[k][rgba] = (tallies[k][rgba] || 0) + 1;
        }
      }
    }
    groups = [];
    for (var j = 0; j < order.length; j++) {
      if ((order[j] & 255) > 0) {
        groups.push({ css: rgbaCss(order[j]), coords: byColour[order[j]] });
      }
    }
    swatches = tallies.map(function (t) {
      var best = null;
      var most = 0;
      for (var c in t) {
        if (Object.prototype.hasOwnProperty.call(t, c) && t[c] > most) {
          best = Number(c);
          most = t[c];
        }
      }
      return best;
    });
  }

  // ---------------------------------------------------------------- panel text

  function metres(mm) {
    // Rounded before formatting, so -0.04 m reads 0.0 rather than -0.0.
    return (Math.round(mm / 100) / 10).toFixed(1) + "m";
  }

  function poseText(r) {
    if (!r) {
      return "no pose";
    }
    var deg = Math.round(((isNum(r.angle) ? r.angle : 0) * 180) / Math.PI);
    return (
      "X " + metres(r.x) + " " + MIDDOT + " Y " + metres(r.y) + " " + MIDDOT + " " +
      THETA + " " + (deg === 0 ? 0 : deg) + DEG
    );
  }

  function errorText() {
    if (state.error) {
      return state.error;
    }
    if (state.status && !Object.prototype.hasOwnProperty.call(KNOWN_STATUS, state.status)) {
      return state.status;
    }
    return "";
  }

  function inOtherFrame() {
    return Boolean(state.map && state.robot && state.robot.origin_id !== state.map.origin_id);
  }

  function renderNotes() {
    var map = state.map;
    var err = errorText();
    var note = el("vbMapNote");
    if (note) {
      note.hidden = Boolean(map) && Boolean(serial);
      var text = !serial ? "no robot selected" : err || "waiting for his first map";
      setText(note, text);
      note.title = text;
      note.className = "vb-map-note" + (serial && err ? " vb-map-note-err" : "");
    }
    var errBar = el("vbMapError");
    if (errBar) {
      errBar.hidden = !(map && err);
      var errLine = map && err ? err + " " + MIDDOT + " showing the last map" : "";
      setText(errBar, errLine);
      errBar.title = errLine;
    }
    var frameBar = el("vbMapFrame");
    if (frameBar) {
      var other = inOtherFrame();
      frameBar.hidden = !other;
      var frameLine = other
        ? "he is in frame " + state.robot.origin_id + ", this map is frame " +
          map.origin_id + ", so he is not drawn"
        : "";
      setText(frameBar, frameLine);
      frameBar.title = frameLine;
    }
  }

  function renderLegend() {
    for (var i = 0; i < KINDS.length; i++) {
      var sw = el(KINDS[i].id);
      if (!sw) {
        continue;
      }
      var rgba = state.map ? swatches[i] : null;
      sw.className = "vb-sw vb-sw-map" + (rgba === null ? " vb-sw-none" : "");
      // A layer over the ground colour, so a translucent colour shows as on the map.
      sw.style.backgroundImage =
        rgba === null ? "" : "linear-gradient(" + rgbaCss(rgba) + ", " + rgbaCss(rgba) + ")";
    }
  }

  function render() {
    setText(el("vbMapPose"), poseText(state.robot));
    renderNotes();
    renderLegend();
    draw();
  }

  // ---------------------------------------------------------------- drawing

  // Matches the canvas's backing store to its CSS size at devicePixelRatio.
  function syncSize() {
    var rect = canvas.getBoundingClientRect();
    var dpr = window.devicePixelRatio || 1;
    var w = Math.max(1, Math.round(rect.width * dpr));
    var h = Math.max(1, Math.round(rect.height * dpr));
    if (canvas.width !== w) {
      canvas.width = w;
    }
    if (canvas.height !== h) {
      canvas.height = h;
    }
    return dpr;
  }

  // Nose-up: screen-up is world +x and screen-left is world +y. The canvas y axis
  // points down, so both world axes enter negated. Returns device pixels.
  function toScreen(v, x, y) {
    return [v.w / 2 - (y - v.cy) * v.k, v.h / 2 - (x - v.cx) * v.k];
  }

  function draw() {
    if (!ctx) {
      return;
    }
    var dpr = syncSize();
    var W = canvas.width;
    var H = canvas.height;
    ctx.fillStyle = BG;
    ctx.fillRect(0, 0, W, H);
    var map = state.map;
    if (!map) {
      return;
    }
    // The root square spans the panel's shorter side, less a small margin.
    var v = { w: W, h: H, cx: map.root.cx, cy: map.root.cy, k: (Math.min(W, H) * 0.92) / map.root.size_mm };
    drawQuads(v);
    drawRootOutline(v, map.root, dpr);
    drawOrigin(v, dpr);
    var r = state.robot;
    if (r && r.origin_id === map.origin_id) {
      drawRobot(v, r, dpr);
    }
  }

  function drawQuads(v) {
    var o = toScreen(v, 0, 0);
    var ox = o[0];
    var oy = o[1];
    var k = v.k;
    for (var g = 0; g < groups.length; g++) {
      var q = groups[g].coords;
      ctx.fillStyle = groups[g].css;
      ctx.beginPath();
      for (var i = 0; i < q.length; i += 3) {
        var half = q[i + 2] / 2;
        // Rounding each edge rather than each size lets neighbours share pixel edges.
        var left = Math.round(ox - (q[i + 1] + half) * k);
        var right = Math.round(ox - (q[i + 1] - half) * k);
        var top = Math.round(oy - (q[i] + half) * k);
        var bottom = Math.round(oy - (q[i] - half) * k);
        if (right <= 0 || bottom <= 0 || left >= v.w || top >= v.h) {
          continue;
        }
        ctx.rect(left, top, Math.max(1, right - left), Math.max(1, bottom - top));
      }
      ctx.fill();
    }
  }

  function drawRootOutline(v, root, dpr) {
    var h = root.size_mm / 2;
    var a = toScreen(v, root.cx + h, root.cy + h);
    var b = toScreen(v, root.cx - h, root.cy - h);
    ctx.setLineDash([3 * dpr, 3 * dpr]);
    ctx.strokeStyle = OUTLINE;
    ctx.lineWidth = dpr;
    ctx.strokeRect(Math.round(a[0]) + 0.5, Math.round(a[1]) + 0.5, Math.round(b[0] - a[0]), Math.round(b[1] - a[1]));
    ctx.setLineDash([]);
  }

  function drawOrigin(v, dpr) {
    var p = toScreen(v, 0, 0);
    var arm = 5 * dpr;
    ctx.strokeStyle = TEAL;
    ctx.lineWidth = dpr;
    ctx.beginPath();
    ctx.moveTo(p[0] - arm, p[1]);
    ctx.lineTo(p[0] + arm, p[1]);
    ctx.moveTo(p[0], p[1] - arm);
    ctx.lineTo(p[0], p[1] + arm);
    ctx.stroke();
  }

  function drawRobot(v, r, dpr) {
    var angle = isNum(r.angle) ? r.angle : 0;
    var c = Math.cos(angle);
    var s = Math.sin(angle);
    // His body is about 90 mm long; drawn at that size, held between 14 and 28 CSS px.
    var mmPerCss = dpr / v.k;
    var len = Math.min(Math.max(90, 14 * mmPerCss), 28 * mmPerCss);
    var half = len * 0.33;
    // Corners in his own frame (+x nose, +y left), turned by his heading into the
    // map's frame, then projected like any other world point.
    var corners = [[0.6 * len, 0], [-0.4 * len, half], [-0.4 * len, -half]];
    var pts = corners.map(function (f) {
      return toScreen(v, r.x + f[0] * c - f[1] * s, r.y + f[0] * s + f[1] * c);
    });
    ctx.beginPath();
    ctx.moveTo(pts[0][0], pts[0][1]);
    ctx.lineTo(pts[1][0], pts[1][1]);
    ctx.lineTo(pts[2][0], pts[2][1]);
    ctx.closePath();
    ctx.fillStyle = AMBER;
    ctx.fill();
    ctx.lineWidth = 1.5 * dpr;
    ctx.strokeStyle = BG;
    ctx.stroke();
  }

  // ---------------------------------------------------------------- polling

  function visible() {
    return document.visibilityState === "visible";
  }

  function tick() {
    // A slow reply makes the next ticks skip rather than queue up behind it.
    if (poll.busy || !visible()) {
      return;
    }
    poll.busy = true;
    var done = function () {
      poll.busy = false;
    };
    var request;
    try {
      request = fetchSnapshot();
    } catch (e) {
      request = Promise.reject(e);
    }
    request.then(applySnapshot).catch(applyFailure).then(done, done);
  }

  function start() {
    if (!serial || poll.interval !== null || !visible()) {
      return;
    }
    poll.interval = setInterval(tick, POLL_MS);
    tick();
  }

  function stop() {
    if (poll.interval !== null) {
      clearInterval(poll.interval);
      poll.interval = null;
    }
  }

  // ---------------------------------------------------------------- init

  function init() {
    canvas = el("vbMapCanvas");
    if (!canvas || typeof canvas.getContext !== "function") {
      return;
    }
    ctx = canvas.getContext("2d");
    if (!ctx) {
      return;
    }
    var link = el("vbMapLink");
    if (link) {
      link.href = serial ? "/navmap?serial=" + encodeURIComponent(serial) : "/navmap";
    }
    render();
    if (typeof ResizeObserver === "function") {
      new ResizeObserver(draw).observe(canvas);
    } else {
      window.addEventListener("resize", draw);
    }
    document.addEventListener("visibilitychange", function () {
      if (visible()) {
        start();
      } else {
        stop();
      }
    });
    window.addEventListener("pagehide", stop);
    window.addEventListener("pageshow", start);
    start();
  }

  function safeInit() {
    try {
      init();
    } catch (e) {
      // Loaded beside main.js and vectorbrain.js; a failure here must not reach them.
      if (window.console && console.error) {
        console.error("minimap: init failed", e);
      }
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", safeInit);
  } else {
    safeInit();
  }
})();
