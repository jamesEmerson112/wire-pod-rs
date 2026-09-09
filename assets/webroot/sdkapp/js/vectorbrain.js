// vectorbrain.js - Vector Brain dashboard for sdkapp/settings.html.
//
// Owns the new card surface only: identity header, battery readout, camera frame,
// live log with its level chips, and the settings drawer. The sixteen legacy tiles
// inside #vbDrawer are still driven by main.js / settings.js / faces.js / heyvector.js,
// so nothing in here may throw into those: every entry point is guarded and a missing
// element means "this page does not have that part" rather than an error.
//
// Reuses two existing globals instead of reimplementing them:
//   getBatteryStatus(serial)     - sdkapp/js/common.js
//   getBatteryPercentage(volts)  - ../js/battery.js  (mirrored by bwBatteryPercent in
//                                  batterywatchdog.go; keep the single source of truth)

(function () {
  "use strict";

  var STATUS_POLL_MS = 2000;
  var BATTERY_POLL_MS = 5000;
  var LOG_POLL_MS = 1000;
  var LOG_MAX_ROWS = 12;
  var CAM_RETRY_MS = 20000;
  // A multipart camera response that ends cleanly fires no "error" event, so a dead
  // stream has to be spotted by frames drying up instead.
  var CAM_STALL_MS = 12000;
  // A stream that never delivers its FIRST frame - which is what a docked, sleeping
  // robot does - also fires no "error"; the request just hangs open. Without a
  // separate deadline for that case camFrames stays 0, the camFrames >= 2 guard in
  // camStalled never trips, and the panel sits black with no explanation.
  var CAM_FIRST_FRAME_MS = 8000;
  // Consecutive failed attempts before the camera stops dialling on its own. Each
  // attempt costs a request and a gRPC stream server-side, and a docked robot will
  // not wake by itself, so retrying forever is pure waste.
  var CAM_MAX_RETRIES = 3;
  var STATUS_FAIL_LIMIT = 3;
  // Matches the default of APIConfig.Battery.GoHomePercent (batterywatchdog.go), so
  // the readout turns red before the watchdog sends the robot home, not after.
  var BATT_LOW_PCT = 25;

  // Built from char codes rather than written literally so this file stays pure ASCII:
  // settings.html declares no <meta charset>, so nothing here depends on how it is decoded.
  var MIDDOT = String.fromCharCode(183);
  var EMDASH = String.fromCharCode(8212);
  var BOLT = String.fromCharCode(9889);

  // Matches the plan's tokens: accent #00ff80, alert #ff6b6b, amber for the middle state.
  // The status values the server can report. Anything else is folded into
  // "unknown" so the value is safe to interpolate into a class name.
  var KNOWN_STATUS = {
    online: true,
    offline: true,
    disconnected: true,
    unknown: true
  };

  // 1x1 transparent GIF. Assigning this aborts an in-flight multipart camera stream
  // without the browser re-requesting the document URL, which img.src = "" does.
  var BLANK_GIF =
    "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

  // main.js:13 already assigns the implicit global "esn"; keep our own scoped copy.
  var vbEsn = "";
  try {
    vbEsn = new URLSearchParams(window.location.search).get("serial") || "";
  } catch (e) {
    vbEsn = "";
  }

  function el(id) {
    return document.getElementById(id);
  }

  // Server-supplied strings end up in class names (level-INFO, comp-stt); keep them
  // to a safe alphabet so a stray value cannot smuggle extra selectors in.
  function safeToken(v) {
    if (!v) {
      return "";
    }
    return String(v).replace(/[^A-Za-z0-9_-]/g, "");
  }

  function two(n) {
    return ("0" + n).slice(-2);
  }

  // webroot/js/main.js defines logTimeString, but that file is not loaded on this page.
  // Use it when something else has provided it, otherwise format locally.
  function timeString(t) {
    if (typeof window.logTimeString === "function") {
      try {
        return window.logTimeString(t);
      } catch (e) {
        // fall through to the local formatter
      }
    }
    var d = new Date(t);
    if (isNaN(d.getTime())) {
      return "";
    }
    return two(d.getHours()) + ":" + two(d.getMinutes()) + ":" + two(d.getSeconds());
  }

  function fetchJSON(url) {
    return fetch(url).then(function (response) {
      if (!response.ok) {
        throw new Error("http " + response.status);
      }
      return response.json();
    });
  }

  // Self-rescheduling poller. Chains on completion rather than using setInterval so a
  // slow call (get_battery opens an SDK connection and waits up to 15s) can never stack.
  function makePoller(fn, ms) {
    var timer = null;
    var busy = false;

    function schedule(delay) {
      if (timer !== null) {
        clearTimeout(timer);
      }
      timer = setTimeout(run, delay);
    }

    function run() {
      timer = null;
      if (busy || document.hidden) {
        schedule(ms);
        return;
      }
      busy = true;
      var done = function () {
        busy = false;
        schedule(ms);
      };
      var result = null;
      try {
        result = fn();
      } catch (e) {
        result = null;
      }
      if (result && typeof result.then === "function") {
        result.then(done, done);
      } else {
        done();
      }
    }

    return {
      start: function () {
        schedule(0);
      },
      kick: function () {
        if (!busy) {
          schedule(0);
        }
      }
    };
  }

  // ---------------------------------------------------------------- identity + status

  var statusFails = 0;
  var lastStatus = "unknown";

  function lastHeardText(bot) {
    if (!bot || typeof bot.timesince !== "number" || bot.timesince < 0) {
      return "last heard unknown";
    }
    return "last heard " + bot.timesince + "s ago";
  }

  // The unwired items get the same amber TODO chip the header readouts use, rather
  // than the words "TODO" inline in the sentence.
  function todoBadge() {
    var badge = document.createElement("span");
    badge.className = "vb-badge vb-badge-todo";
    badge.textContent = "TODO";
    return badge;
  }

  function renderSubline(bot) {
    var sub = el("vbSubline");
    if (!sub) {
      return;
    }
    sub.textContent = "";
    if (!vbEsn) {
      sub.appendChild(
        document.createTextNode(
          "no robot selected " + MIDDOT + " open this page as settings.html?serial=<esn>"
        )
      );
      return;
    }
    var sep = " " + MIDDOT + " ";
    var frag = document.createDocumentFragment();
    frag.appendChild(document.createTextNode((bot && bot.ip ? bot.ip : "no ip") + sep + "firmware "));
    frag.appendChild(todoBadge());
    frag.appendChild(document.createTextNode(sep + "uptime "));
    frag.appendChild(todoBadge());
    frag.appendChild(document.createTextNode(sep + lastHeardText(bot)));
    sub.appendChild(frag);
  }

  function renderStatus(bot) {
    var serialEl = el("vbSerial");
    if (serialEl) {
      serialEl.textContent = vbEsn || "";
    }

    var status = bot && bot.status ? safeToken(bot.status) : "unknown";
    if (!KNOWN_STATUS[status]) {
      status = "unknown";
    }
    if (status === "online" && status !== lastStatus) {
      // A robot that has just come back is a fresh chance for the camera, so let
      // maybeRetryCam dial again rather than staying held from the old state. Only
      // this transition: resetting on every change would also reset on the way out
      // and on the "unknown" that a failing status poll renders, so a flapping
      // robot or a flaky server would re-arm the budget forever.
      camFailures = 0;
    }
    lastStatus = status;

    var textEl = el("vbStatusText");
    if (textEl) {
      // Without ?serial= nothing on this page can resolve to a robot; say so instead
      // of reporting "Unknown" forever.
      textEl.textContent = vbEsn
        ? status.charAt(0).toUpperCase() + status.slice(1)
        : "No serial";
    }

    var pill = el("vbStatusPill");
    if (pill) {
      // style.css carries .vb-pill-offline / -disconnected / -unknown; the bare
      // .vb-pill rule is already the online (green) state, so vb-pill-online
      // deliberately has no rule of its own. The dot is coloured by those same
      // rules, which is why nothing is set inline here.
      pill.className = "vb-pill vb-pill-" + status;
      pill.setAttribute("data-status", status);
    }

    renderSubline(bot);
  }

  function pollStatus() {
    if (!el("vbStatusPill") && !el("vbStatusText") && !el("vbSubline")) {
      return null;
    }
    if (!vbEsn) {
      renderStatus(null);
      return null;
    }
    return fetchJSON("/api/get_bot_status")
      .then(function (bots) {
        statusFails = 0;
        var bot = null;
        // The endpoint answers the JSON literal null when no robots are known.
        if (Array.isArray(bots)) {
          for (var i = 0; i < bots.length; i++) {
            if (bots[i] && bots[i].esn === vbEsn) {
              bot = bots[i];
              break;
            }
          }
        }
        renderStatus(bot);
        maybeRetryCam();
      })
      .catch(function () {
        // Hold the last good reading briefly, then admit we have lost the server
        // rather than leaving a stale "Online" pill up forever.
        statusFails++;
        if (statusFails >= STATUS_FAIL_LIMIT) {
          renderStatus(null);
        }
      });
  }

  // ------------------------------------------------------------------------- battery

  function renderBattery(status) {
    var pctEl = el("vbBattPct");
    var voltsEl = el("vbBattVolts");

    if (!status) {
      if (pctEl) {
        pctEl.textContent = "--";
        pctEl.classList.remove("vb-batt-low");
      }
      if (voltsEl) {
        voltsEl.textContent = "";
      }
      return;
    }

    var volts = typeof status.battery_volts === "number" ? status.battery_volts : null;

    var percent = null;
    // Only called with a real reading. getBatteryPercentage answers a flat 70 for a
    // missing voltage (battery.js "assume a reasonable battery percentage"), and the
    // protobuf JSON omits battery_volts entirely for a genuine 0.0V, so feeding it a
    // null would print a fabricated 70% on exactly the robots that are in trouble.
    if (volts !== null && typeof window.getBatteryPercentage === "function") {
      try {
        // Deliberately reused, not reimplemented: the Go watchdog mirrors this curve.
        percent = window.getBatteryPercentage(volts);
      } catch (e) {
        percent = null;
      }
    }

    if (pctEl) {
      pctEl.textContent = percent === null ? "--" : percent + "%";
      pctEl.classList.toggle("vb-batt-low", percent !== null && percent <= BATT_LOW_PCT);
    }

    if (voltsEl) {
      var parts = [];
      if (volts !== null) {
        parts.push(volts.toFixed(2) + "V");
      }
      if (status.is_on_charger_platform) {
        parts.push(BOLT);
      }
      voltsEl.textContent = parts.join(" ");
    }
  }

  function pollBattery() {
    if (!vbEsn || (!el("vbBattPct") && !el("vbBattVolts"))) {
      return null;
    }
    if (typeof window.getBatteryStatus !== "function") {
      return null;
    }
    // getBatteryStatus calls .json() unconditionally, so it throws on the plain-text
    // "error: ..." body /api-sdk/ returns for an unreachable robot.
    return Promise.resolve()
      .then(function () {
        return window.getBatteryStatus(vbEsn);
      })
      .then(function (status) {
        renderBattery(status && typeof status === "object" ? status : null);
      })
      .catch(function () {
        renderBattery(null);
      });
  }

  // -------------------------------------------------------------------------- camera

  var camStarted = false;
  var camErrored = false;
  var camStopping = false;
  var camLastTry = 0;
  var camFrames = 0;
  var camLastFrame = 0;
  var camFailures = 0;
  // Whether the current attempt has already been counted, so the two places that
  // notice a failure cannot count the same one twice.
  var camCounted = false;

  function setCamOff(off) {
    var frame = el("vbCamFrame");
    if (frame) {
      frame.classList.toggle("vb-cam-off", !!off);
      // The overlay advertises CLICK TO RETRY, and the frame is a plain div: not
      // focusable, not operable by keyboard. Give it the affordance while it is
      // offering one and take it away again with the offer.
      if (off) {
        frame.setAttribute("tabindex", "0");
        frame.setAttribute("role", "button");
        frame.setAttribute("aria-label", "Camera offline, click to retry");
      } else {
        frame.removeAttribute("tabindex");
        frame.removeAttribute("role");
        frame.removeAttribute("aria-label");
      }
    }
  }

  function startCam() {
    var img = el("vbCam");
    if (!img || !vbEsn) {
      return;
    }
    // Always cache-busted: a retry must issue a fresh request rather than let the
    // browser reuse the aborted one.
    var src = "/cam-stream?serial=" + encodeURIComponent(vbEsn) + "&_=" + Date.now();
    camStarted = true;
    camStopping = false;
    camErrored = false;
    camCounted = false;
    camFrames = 0;
    camLastFrame = Date.now();
    camLastTry = camLastFrame;
    setCamOff(false);
    img.src = src;
  }

  function stopCam() {
    var img = el("vbCam");
    if (!img) {
      return;
    }
    // Aborts the multipart response so the Go handler's r.Context().Done() branch runs
    // and calls EnableImageStreaming(false).
    camStopping = true;
    try {
      img.src = BLANK_GIF;
      img.removeAttribute("src");
    } catch (e) {
      // ignore
    }
    camStarted = false;
    camFrames = 0;
    // Left in the errored state on purpose: every restart path (pageshow after a
    // bfcache restore, the tab becoming visible, the status poll) checks this flag,
    // and a torn-down stream that claims to be healthy never comes back.
    camErrored = true;
    setCamOff(true);
  }

  // A /cam-stream response that ends cleanly - which is what the Go handler does to
  // the first request as soon as a second tab or a reload asks for the same robot -
  // fires no "error" event, so the browser just keeps painting the last frame.
  // Frames arriving is the only reliable liveness signal. The camFrames >= 2 guard
  // means a browser that fires "load" once for the whole stream instead of once per
  // part is never falsely restarted.
  function camStalled() {
    return (
      camStarted &&
      !camStopping &&
      camFrames >= 2 &&
      Date.now() - camLastFrame > CAM_STALL_MS
    );
  }

  function camNeverArrived() {
    return (
      camStarted &&
      !camStopping &&
      camFrames === 0 &&
      Date.now() - camLastTry > CAM_FIRST_FRAME_MS
    );
  }

  // One failure per attempt, wherever it is noticed. The stall detectors and the
  // img "error" listener both come through here: counting only in the detectors
  // left camFailures at zero for every HTTP-level failure, because the Go handler
  // reports those as a plaintext 200 body that the <img> rejects with an "error"
  // event long before the first-frame deadline. The ceiling and the backoff below
  // then never engaged on that path at all.
  function markCamFailure() {
    if (!camCounted) {
      camCounted = true;
      camFailures++;
    }
    camErrored = true;
    setCamOff(true);
  }

  function maybeRetryCam() {
    if (document.hidden) {
      return;
    }
    // Notice the failure whatever the robot's status is: a stream that hangs while
    // the robot is away otherwise leaves the panel showing a black frame with the
    // crosshair and caption of a working one, and never restores the overlay.
    if (camStalled() || camNeverArrived()) {
      markCamFailure();
    }
    if (!camErrored) {
      return;
    }
    // Dial only when there is something to dial.
    if (lastStatus !== "online") {
      return;
    }
    // Hold instead of dialling forever. Cleared by the robot coming online, by the
    // tab becoming visible again, or by clicking the frame.
    if (camFailures >= CAM_MAX_RETRIES) {
      return;
    }
    // Back off as failures repeat. Every attempt against a robot whose camera will
    // not wake holds a hung request and a gRPC stream open server-side, so retrying
    // at a flat 20s forever is not free. Caps at six times the base interval.
    if (Date.now() - camLastTry < CAM_RETRY_MS * Math.min(camFailures || 1, 6)) {
      return;
    }
    startCam();
  }

  function retryCamNow() {
    if (!camErrored) {
      return;
    }
    // The overlay asks to be clicked, so it will be double-clicked; a second
    // request inside the server's half-second handshake is pure waste.
    if (Date.now() - camLastTry < 1000) {
      return;
    }
    // Explicit user intent outranks the retry ceiling.
    camFailures = 0;
    startCam();
  }

  function initCam() {
    var img = el("vbCam");
    if (!img) {
      return;
    }
    var frame = el("vbCamFrame");
    if (frame) {
      frame.addEventListener("click", retryCamNow);
      frame.addEventListener("keydown", function (ev) {
        if (ev.key === "Enter" || ev.key === " " || ev.keyCode === 13 || ev.keyCode === 32) {
          ev.preventDefault();
          retryCamNow();
        }
      });
    }
    img.addEventListener("error", function () {
      if (camStopping) {
        return;
      }
      markCamFailure();
    });
    img.addEventListener("load", function () {
      // stopCam assigns a blank GIF to abort the stream; that load is not a frame.
      if (camStopping) {
        return;
      }
      camFrames++;
      camLastFrame = Date.now();
      camErrored = false;
      camFailures = 0;
      camCounted = false;
      setCamOff(false);
    });
    startCam();
  }

  // ------------------------------------------------------------------------ live log

  var logLevel = "";
  var logSince = 0;
  // Keys of the entries already shown at exactly logSince, see pollLogs.
  var logBoundary = {};
  var logRows = [];
  // Bumped by selectChip so a response for the previous filter, already in flight
  // when the chip was clicked, can be discarded instead of polluting the buffer.
  var logGen = 0;

  function entryKey(entry) {
    return [entry.t, entry.level, entry.comp, entry.bot, entry.msg].join("\u0001");
  }

  function makeCell(className, text) {
    var span = document.createElement("span");
    span.className = className;
    span.textContent = text;
    return span;
  }

  function makeLogRow(entry) {
    var row = document.createElement("div");
    row.className = "vb-log-row";

    row.appendChild(makeCell("vb-lt", typeof entry.t === "number" ? timeString(entry.t) : ""));

    var level = safeToken(entry.level);
    row.appendChild(makeCell("vb-ll" + (level ? " level-" + level : ""), level));

    var comp = safeToken(entry.comp);
    row.appendChild(makeCell("vb-lc" + (comp ? " comp-" + comp : ""), comp || EMDASH));

    row.appendChild(makeCell("vb-lb", entry.bot ? String(entry.bot) : EMDASH));
    row.appendChild(makeCell("vb-lm", entry.msg ? String(entry.msg) : ""));

    return row;
  }

  function renderLogs() {
    var body = el("vbLogBody");
    if (!body) {
      return;
    }
    // Every tick rebuilds the rows; only re-pin to the bottom if the reader was
    // already there, otherwise scrolling back through history is impossible.
    var pinned = body.scrollHeight - body.scrollTop - body.clientHeight <= 4;
    var prevTop = body.scrollTop;
    body.textContent = "";

    // A chip click clears the buffer, so the box would otherwise sit blank
    // until the next poll returns.
    if (logRows.length === 0) {
      body.appendChild(makeCell("vb-log-empty", "waiting for log output..."));
      return;
    }

    var frag = document.createDocumentFragment();
    for (var i = 0; i < logRows.length; i++) {
      frag.appendChild(makeLogRow(logRows[i]));
    }
    body.appendChild(frag);
    body.scrollTop = pinned ? body.scrollHeight : prevTop;
  }

  function pollLogs() {
    if (!el("vbLogBody")) {
      return null;
    }
    var gen = logGen;
    // The server filters with a strict "e.TimeMS > since", so anything written in the
    // same millisecond as the newest entry of a response - but after that response was
    // built - would be skipped forever. Ask from one millisecond earlier and drop the
    // entries already shown at that boundary.
    var since = logSince > 0 ? logSince - 1 : 0;
    return fetchJSON(
      "/api/get_logs_json?level=" + encodeURIComponent(logLevel) + "&since=" + since
    )
      .then(function (logs) {
        // A chip click since this request went out already cleared the buffer for a
        // different level; this response belongs to the old filter.
        if (gen !== logGen) {
          return;
        }
        if (!Array.isArray(logs) || logs.length === 0) {
          return;
        }
        var added = 0;
        var maxT = logSince;
        for (var i = 0; i < logs.length; i++) {
          var entry = logs[i];
          if (!entry) {
            continue;
          }
          var t = typeof entry.t === "number" ? entry.t : 0;
          if (t < logSince) {
            continue;
          }
          if (t === logSince && logBoundary[entryKey(entry)]) {
            continue;
          }
          logRows.push(entry);
          added++;
          if (t > maxT) {
            maxT = t;
          }
        }
        if (added === 0) {
          return;
        }
        if (maxT !== logSince) {
          logSince = maxT;
          logBoundary = {};
        }
        for (var j = 0; j < logs.length; j++) {
          if (logs[j] && logs[j].t === logSince) {
            logBoundary[entryKey(logs[j])] = true;
          }
        }
        while (logRows.length > LOG_MAX_ROWS) {
          logRows.shift();
        }
        renderLogs();
      })
      .catch(function () {
        // transient; the next tick retries from the same cursor
      });
  }

  function selectChip(chip, chips) {
    // Re-clicking the active chip would otherwise re-download the server's whole
    // 500-entry ring just to keep the last twelve rows.
    if (chip.classList.contains("vb-chip-on")) {
      return;
    }
    for (var i = 0; i < chips.length; i++) {
      chips[i].classList.remove("vb-chip-on");
    }
    chip.classList.add("vb-chip-on");

    logLevel = chip.getAttribute("data-level") || "";
    // Reset the cursor and the buffer so the new filter applies to history immediately
    // instead of only to rows that arrive from now on. The generation bump makes any
    // in-flight response for the old level a no-op, which is what stops it from
    // re-filling the buffer and advancing the cursor past the history just discarded.
    logGen++;
    logSince = 0;
    logBoundary = {};
    logRows = [];
    renderLogs();
    logPoller.kick();
  }

  function initLogChips() {
    var wrap = el("vbLogChips");
    if (!wrap) {
      return;
    }
    var chips = Array.prototype.slice.call(wrap.querySelectorAll(".vb-chip"));
    if (!chips.length) {
      return;
    }

    var active = null;
    for (var i = 0; i < chips.length; i++) {
      (function (chip) {
        chip.addEventListener("click", function () {
          selectChip(chip, chips);
        });
      })(chips[i]);
      if (chips[i].classList.contains("vb-chip-on")) {
        active = chips[i];
      }
    }

    if (!active) {
      // Default to the "All" chip, which is the one with an empty data-level.
      for (var j = 0; j < chips.length; j++) {
        if (!chips[j].getAttribute("data-level")) {
          active = chips[j];
          break;
        }
      }
      if (!active) {
        active = chips[0];
      }
      active.classList.add("vb-chip-on");
    }
    logLevel = active.getAttribute("data-level") || "";
  }

  // -------------------------------------------------------------------------- drawer

  var drawerOpen = false;

  function setDrawer(open) {
    var drawer = el("vbDrawer");
    var scrim = el("vbScrim");
    var gear = el("vbGear");

    if (drawer) {
      drawer.classList.toggle("vb-open", open);
      drawer.setAttribute("aria-hidden", open ? "false" : "true");
      // aria-hidden on its own leaves the sixteen tiles and every control in the
      // fourteen sections focusable; inert (with visibility:hidden in style.css as
      // the fallback) actually takes the closed drawer out of the tab order.
      if (open) {
        drawer.removeAttribute("inert");
      } else {
        drawer.setAttribute("inert", "");
      }
    }
    if (scrim) {
      scrim.classList.toggle("vb-open", open);
    }
    if (gear) {
      gear.setAttribute("aria-expanded", open ? "true" : "false");
    }
    // The drawer is a fixed full-height overlay; without this the card keeps
    // scrolling behind it.
    if (document.body && document.body.classList) {
      document.body.classList.toggle("vb-locked", open);
    }
    if (!open) {
      // showSection('section-stim') sets the global stimRunning, POSTs
      // begin_event_stream and starts a 500ms poller in main.js. That teardown is
      // cooperative: the poller stops itself and POSTs stop_event_stream once it
      // observes the flag go false. Until the drawer existed, the only way out of
      // the section was clicking another tile, which took showSection's else
      // branch. Closing the drawer is a new exit and has to do the same, or the
      // poller and the robot's event stream keep running behind a hidden drawer.
      if (window.stimRunning) {
        window.stimRunning = false;
      }
      // And put the sections back the way the drawer starts. Leaving one displayed
      // means reopening presents a Stim panel frozen on its last twelve datapoints
      // with its poller and the robot's event stream stopped and nothing saying so,
      // and re-selecting the tile that is already showing looks like a no-op.
      var sections = document.getElementsByClassName("toggleable-section");
      for (var i = 0; i < sections.length; i++) {
        sections[i].style.display = "none";
      }
    }
    drawerOpen = open;
  }

  function initDrawer() {
    var gear = el("vbGear");
    var closeBtn = el("vbDrawerClose");
    var scrim = el("vbScrim");

    if (gear) {
      gear.addEventListener("click", function (ev) {
        ev.preventDefault();
        setDrawer(!drawerOpen);
      });
    }
    if (closeBtn) {
      closeBtn.addEventListener("click", function (ev) {
        ev.preventDefault();
        setDrawer(false);
      });
    }
    if (scrim) {
      scrim.addEventListener("click", function () {
        setDrawer(false);
      });
    }

    document.addEventListener("keydown", function (ev) {
      if (!drawerOpen) {
        return;
      }
      if (ev.key === "Escape" || ev.key === "Esc" || ev.keyCode === 27) {
        setDrawer(false);
      }
    });

    setDrawer(false);
  }

  // ------------------------------------------------------------------- network

  // Rolling window behind the sparklines and the summary stats. At NET_POLL_MS that
  // is about a minute and a half of history: long enough to show a wifi dropout,
  // short enough that the numbers still describe what the link is doing now.
  var NET_SAMPLES = 30;
  var NET_POLL_MS = 3000;
  // How long RUN measures over. Throughput is the difference of two byte counters,
  // so the window has to be long enough that one frame either side cannot dominate.
  var NET_RUN_MS = 5000;
  // A measured result stays on screen this long before the live rate takes the
  // readout back, so pressing RUN does not produce a number that vanishes on the
  // next poll.
  var NET_RUN_HOLD_MS = 15000;
  // Floors for the sparkline y-axis. Without them a healthy link draws as dramatic
  // noise, because a two millisecond spread gets amplified to fill the whole box.
  var NET_FLOOR_MS = 25;
  var NET_FLOOR_BPS = 32 * 1024;

  var ELLIPSIS = String.fromCharCode(8230);

  // Latency in ms and down rate in bytes/sec. A failed probe pushes null into
  // netRtt: it still takes a slot so the loss figure counts it, but every statistic
  // skips it.
  var netRtt = [];
  var netDown = [];
  // Previous reading of the robot's byte counter, for differencing into a rate.
  var netPrev = null;
  var netTarget = "";
  var netProbeName = "";
  var netError = "";
  var netRunning = false;
  var netHoldUntil = 0;

  // /api-sdk/ reports an unreachable robot as a plain-text "error: ..." body with
  // HTTP 200. Handing that to .json() gets a SyntaxError whose message is about
  // JSON tokens rather than about the robot, and that message is what would end up
  // printed in the panel, so read the body as text and check it first. The other
  // pollers on this page can get away with letting .json() reject because they only
  // need to know that something failed, not what.
  function fetchProbe(url) {
    return fetch(url)
      .then(function (response) {
        if (!response.ok) {
          throw new Error("http " + response.status);
        }
        return response.text();
      })
      .then(function (text) {
        var body = String(text).replace(/^\s+/, "");
        if (body.slice(0, 6) === "error:") {
          throw new Error(body.slice(6).replace(/^\s+/, ""));
        }
        try {
          return JSON.parse(body);
        } catch (e) {
          throw new Error("unreadable response");
        }
      });
  }

  function netURL() {
    return "/api-sdk/net_probe?serial=" + encodeURIComponent(vbEsn);
  }

  function netPush(arr, value) {
    arr.push(value);
    while (arr.length > NET_SAMPLES) {
      arr.shift();
    }
  }

  function netNumbers(arr) {
    var out = [];
    for (var i = 0; i < arr.length; i++) {
      if (typeof arr[i] === "number" && isFinite(arr[i])) {
        out.push(arr[i]);
      }
    }
    return out;
  }

  function netMean(values) {
    var total = 0;
    for (var i = 0; i < values.length; i++) {
      total += values[i];
    }
    return total / values.length;
  }

  // Mean absolute difference between consecutive samples. Deliberately not the
  // RFC 3550 smoothed estimator: this one needs no warm-up and can be explained in
  // one line, which matters more for a dashboard than statistical pedigree.
  function netJitter(values) {
    if (values.length < 2) {
      return null;
    }
    var total = 0;
    for (var i = 1; i < values.length; i++) {
      total += Math.abs(values[i] - values[i - 1]);
    }
    return total / (values.length - 1);
  }

  function netRate(bytesPerSec) {
    if (typeof bytesPerSec !== "number" || !isFinite(bytesPerSec) || bytesPerSec < 0) {
      return "--";
    }
    if (bytesPerSec >= 1024 * 1024) {
      return (bytesPerSec / (1024 * 1024)).toFixed(1) + " MB/s";
    }
    return Math.round(bytesPerSec / 1024) + " kB/s";
  }

  function netShort(text) {
    var s = String(text === null || text === undefined ? "" : text).replace(/\s+/g, " ");
    s = s.replace(/^\s+|\s+$/g, "");
    if (s.length > 40) {
      s = s.slice(0, 39) + ELLIPSIS;
    }
    return s;
  }

  // Rewrites a polyline in place. The SVG is authored as viewBox "0 0 200 34" with
  // preserveAspectRatio="none", so these coordinates live in that fixed space and
  // the browser stretches them to whatever width the panel happens to have.
  function renderSpark(id, samples, floor) {
    var line = el(id);
    if (!line) {
      return;
    }
    var pts = netNumbers(samples);
    if (pts.length < 2) {
      line.setAttribute("points", "");
      return;
    }
    var max = floor;
    for (var i = 0; i < pts.length; i++) {
      if (pts[i] > max) {
        max = pts[i];
      }
    }
    var out = [];
    for (var j = 0; j < pts.length; j++) {
      var x = (j * 200) / (pts.length - 1);
      // Two units of headroom top and bottom so the stroke is not clipped.
      var y = 32 - (pts[j] / max) * 30;
      out.push(x.toFixed(1) + "," + y.toFixed(1));
    }
    line.setAttribute("points", out.join(" "));
  }

  function setText(id, text) {
    var node = el(id);
    if (node) {
      node.textContent = text;
    }
  }

  function renderNet() {
    var rtts = netNumbers(netRtt);
    var downs = netNumbers(netDown);
    var holding = Date.now() < netHoldUntil;

    // The newest sample, not the newest good one. A robot that has just gone away
    // must not leave its last healthy latency sitting in the header next to a loss
    // count that is climbing, which reads as a working link. The window statistics
    // below still describe the samples that did land.
    var lastRtt = netRtt.length ? netRtt[netRtt.length - 1] : null;
    var lastDown = netDown.length ? netDown[netDown.length - 1] : null;
    setText("vbLatency", typeof lastRtt === "number" ? String(Math.round(lastRtt)) : "--");
    if (!holding) {
      setText("vbThroughput", typeof lastDown === "number" ? String(Math.round(lastDown / 1024)) : "--");
      setText("vbDownLabel", typeof lastDown === "number" ? netRate(lastDown) : "--");
    }

    if (rtts.length) {
      var lo = Math.min.apply(null, rtts);
      var hi = Math.max.apply(null, rtts);
      setText("vbLatLabel", Math.round(lo) + " / " + Math.round(netMean(rtts)) + " / " + Math.round(hi) + " ms");
    } else {
      setText("vbLatLabel", "--");
    }

    var jitter = netJitter(rtts);
    setText("vbNetJitter", jitter === null ? "--" : jitter.toFixed(1) + " ms");
    setText("vbNetTarget", netTarget || "--");

    var lost = 0;
    for (var i = 0; i < netRtt.length; i++) {
      if (netRtt[i] === null) {
        lost++;
      }
    }
    setText("vbNetLoss", netRtt.length ? lost + " / " + netRtt.length : "--");
    setText("vbNetProbe", netError ? netShort(netError) : (netProbeName || "--"));

    var probeEl = el("vbNetProbe");
    if (probeEl) {
      probeEl.classList.toggle("vb-kvv-accent", !netError);
    }

    renderSpark("vbLatSpark", netRtt, NET_FLOOR_MS);
    renderSpark("vbDownSpark", netDown, NET_FLOOR_BPS);
  }

  // Throughput is the difference between two readings of a counter that only climbs,
  // over the time between them. Differencing here rather than server-side is why
  // net_probe can stay stateless and why the averaging window is the page's choice.
  function netDelta(probe) {
    var now = Date.now();
    var prev = netPrev;
    netPrev = { bytes: probe.camBytes, at: now };
    if (!prev) {
      return null;
    }
    var seconds = (now - prev.at) / 1000;
    // A counter that went backwards means the server restarted, not a negative rate.
    if (seconds <= 0 || probe.camBytes < prev.bytes) {
      return null;
    }
    return (probe.camBytes - prev.bytes) / seconds;
  }

  function pollNet() {
    if (!vbEsn || !el("vbLatency")) {
      return null;
    }
    return fetchProbe(netURL())
      .then(function (probe) {
        if (!probe || typeof probe.rttMs !== "number") {
          throw new Error("bad probe");
        }
        netPush(netRtt, probe.rttMs);
        netPush(netDown, netDelta(probe));
        netTarget = typeof probe.target === "string" ? probe.target : "";
        // Named by the server rather than hardcoded here, so the row always says
        // which RPC produced the number above it.
        netProbeName = typeof probe.probe === "string" ? probe.probe : "";
        netError = "";
      })
      .catch(function (e) {
        // /api-sdk/ answers an unreachable robot with a plain-text "error: ..." body
        // and HTTP 200, so .json() rejecting is the failure signal here, the same
        // one the battery and stim pollers already rely on.
        netPush(netRtt, null);
        netPush(netDown, null);
        netPrev = null;
        netError = e && e.message ? e.message : "probe failed";
      })
      .then(function () {
        renderNet();
      });
  }

  function runThroughput() {
    if (netRunning) {
      return;
    }
    var btn = el("vbNetRun");
    if (!vbEsn) {
      setText("vbDownLabel", "no serial");
      return;
    }
    netRunning = true;
    if (btn) {
      btn.disabled = true;
      btn.textContent = ELLIPSIS;
    }
    // Held from the start, not just once the result lands, so the poller's own
    // render does not wipe this out on the very next tick.
    netHoldUntil = Date.now() + NET_RUN_MS + NET_RUN_HOLD_MS;
    setText("vbDownLabel", "measuring" + ELLIPSIS);

    var first = null;
    var startedAt = 0;

    fetchProbe(netURL())
      .then(function (probe) {
        if (!probe || !probe.camOn) {
          // Nothing is streaming, so there is no traffic on the link to measure.
          // Say that rather than report the honest but useless 0 kB/s an idle
          // counter would give.
          throw new Error("camera off");
        }
        first = probe;
        startedAt = Date.now();
        return new Promise(function (resolve) {
          setTimeout(resolve, NET_RUN_MS);
        });
      })
      .then(function () {
        return fetchProbe(netURL());
      })
      .then(function (second) {
        var seconds = (Date.now() - startedAt) / 1000;
        var delta = second.camBytes - first.camBytes;
        if (seconds <= 0 || delta < 0) {
          throw new Error("counter reset");
        }
        var rate = delta / seconds;
        netHoldUntil = Date.now() + NET_RUN_HOLD_MS;
        setText("vbDownLabel", netRate(rate) + " over " + Math.round(seconds) + "s");
        setText("vbThroughput", String(Math.round(rate / 1024)));
      })
      .catch(function (e) {
        netHoldUntil = Date.now() + NET_RUN_HOLD_MS;
        setText("vbDownLabel", netShort(e && e.message ? e.message : "failed"));
      })
      .then(function () {
        netRunning = false;
        if (btn) {
          btn.disabled = false;
          btn.textContent = "RUN";
        }
      });
  }

  function initNetRun() {
    var btn = el("vbNetRun");
    if (btn) {
      btn.addEventListener("click", runThroughput);
    }
  }

  // ---------------------------------------------------------------------------- init

  var statusPoller = makePoller(pollStatus, STATUS_POLL_MS);
  var batteryPoller = makePoller(pollBattery, BATTERY_POLL_MS);
  var logPoller = makePoller(pollLogs, LOG_POLL_MS);
  var netPoller = makePoller(pollNet, NET_POLL_MS);

  function init() {
    // Only the settings dashboard has this card; anywhere else, do nothing at all.
    if (!el("vbCard")) {
      return;
    }

    renderStatus(null);
    renderBattery(null);
    initDrawer();
    initLogChips();
    renderLogs();
    initCam();
    initNetRun();
    renderNet();

    statusPoller.start();
    batteryPoller.start();
    logPoller.start();
    netPoller.start();

    document.addEventListener("visibilitychange", function () {
      if (document.hidden) {
        // The robot keeps encoding and sending frames for as long as the stream is
        // open, so a backgrounded tab is a real drain on a robot this fork also
        // watches for low battery. Drop the stream and pick it up again on return.
        stopCam();
        return;
      }
      camFailures = 0;
      startCam();
      statusPoller.kick();
      batteryPoller.kick();
      logPoller.kick();
      netPoller.kick();
    });

    window.addEventListener("beforeunload", stopCam);
    window.addEventListener("pagehide", stopCam);
    // A bfcache restore (browser Back) resurrects the page with the camera torn down
    // by pagehide and no load or error event to notice it.
    window.addEventListener("pageshow", function (ev) {
      if (ev && ev.persisted) {
        startCam();
      }
    });

    window.vectorBrain = {
      serial: vbEsn,
      refresh: function () {
        statusPoller.kick();
        batteryPoller.kick();
        logPoller.kick();
        netPoller.kick();
      },
      openDrawer: function () {
        setDrawer(true);
      },
      closeDrawer: function () {
        setDrawer(false);
      }
    };
  }

  function safeInit() {
    try {
      init();
    } catch (e) {
      // This script loads alongside main.js; an exception here must never take the
      // legacy tiles down with it.
      if (window.console && console.error) {
        console.error("vectorbrain: init failed", e);
      }
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", safeInit);
  } else {
    safeInit();
  }
})();
