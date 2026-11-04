// BusWatch's theme control and refresh bar (spec/14-buswatch.md §7.5).
//
// Without this script every control is still a link or a form, so the page
// works; this adds the dashboard's in-place refresh. It never builds markup
// from a string of its own: the new page content is the server's, parsed by
// the browser, and that content was escaped by the server (§7.4).
(function () {
  "use strict";

  // --- theme -------------------------------------------------------------
  var root = document.documentElement;
  function current() {
    var set = root.getAttribute("data-theme");
    if (set) { return set; }
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  var toggle = document.getElementById("theme-toggle");
  if (toggle) {
    toggle.addEventListener("click", function () {
      var next = current() === "dark" ? "light" : "dark";
      root.setAttribute("data-theme", next);
      try { window.localStorage.setItem("buswatch-theme", next); } catch (e) { /* not remembered */ }
    });
  }

  // --- refresh bar -------------------------------------------------------
  var bar = document.getElementById("refresh-bar");
  if (!bar) { return; }
  var now = document.getElementById("refresh-now");
  var autoLabel = document.getElementById("auto-label");
  var auto = document.getElementById("auto-refresh");
  var countdown = document.getElementById("refresh-countdown");
  var status = document.getElementById("refresh-status");
  var updated = document.getElementById("refresh-updated");
  var interval = parseInt(bar.getAttribute("data-interval"), 10) || 30;
  var remaining = interval;
  var busy = false;

  now.hidden = false;
  autoLabel.hidden = false;

  async function refresh() {
    if (busy) { return; }
    busy = true;
    status.textContent = "";
    try {
      // `refresh_all=true` re-reads every queue (§3.1); the result replaces
      // <main> in place, so scroll position and theme survive.
      var response = await fetch("/?refresh_all=true", { headers: { "Accept": "text/html" } });
      if (!response.ok) { throw new Error("The refresh failed (" + response.status + ")."); }
      var doc = new DOMParser().parseFromString(await response.text(), "text/html");
      var fresh = doc.querySelector("main");
      var live = document.querySelector("main");
      if (fresh && live) { live.replaceChildren.apply(live, Array.prototype.slice.call(fresh.childNodes)); }
      // The bar lives inside <main>, so the swap replaced it: re-bind.
      rebind();
    } catch (error) {
      status.textContent = error.message;
    } finally {
      busy = false;
    }
  }

  function rebind() {
    bar = document.getElementById("refresh-bar");
    now = document.getElementById("refresh-now");
    autoLabel = document.getElementById("auto-label");
    auto = document.getElementById("auto-refresh");
    countdown = document.getElementById("refresh-countdown");
    status = document.getElementById("refresh-status");
    updated = document.getElementById("refresh-updated");
    if (!bar) { return; }
    now.hidden = false;
    autoLabel.hidden = false;
    auto.checked = wanted;
    now.addEventListener("click", function () { remaining = interval; refresh(); });
    auto.addEventListener("change", function () { wanted = auto.checked; remaining = interval; paint(); });
    paint();
  }

  var wanted = false;
  function paint() {
    if (countdown) { countdown.textContent = wanted ? remaining + "s" : ""; }
  }

  window.setInterval(function () {
    if (!wanted || busy) { return; }
    remaining -= 1;
    if (remaining <= 0) { remaining = interval; refresh(); }
    paint();
  }, 1000);

  rebind();
})();
