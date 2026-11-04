// The Network Test page (spec/15-dashboard.md §10.4). Markup is built with
// the DOM API and textContent only: hosts, descriptions and errors are text
// that came from people or from the network.
(function () {
  "use strict";
  var form = document.getElementById("test-form");
  if (!form) { return; }
  var message = document.getElementById("test-message");
  var resultBox = document.getElementById("test-result");
  var current = null;

  function say(text, kind) { message.textContent = text; message.className = "form-message " + (kind || ""); }
  function el(tag, text, cls) {
    var e = document.createElement(tag);
    if (text !== undefined && text !== null) { e.textContent = String(text); }
    if (cls) { e.className = cls; }
    return e;
  }
  function fmt(n) { return n === null || n === undefined ? "—" : String(n); }

  form.elements.source.addEventListener("change", function () {
    var parts = form.elements.source.value.split("|");
    if (parts.length === 2) { form.elements.host.value = parts[0]; form.elements.port.value = parts[1]; }
  });

  function showResult(r) {
    resultBox.replaceChildren();
    resultBox.hidden = false;
    var dns = r.dns.resolved ? "resolved to " + r.dns.addresses.join(", ") + " in " + r.dns.resolve_time_ms + " ms"
                             : "did not resolve: " + (r.dns.error || "");
    resultBox.appendChild(el("p", (r.success ? "Reachable" : "Not reachable") + " — " + r.host + ":" + r.port, "state " + (r.success ? "healthy" : "critical")));
    resultBox.appendChild(el("p", "DNS " + dns));
    resultBox.appendChild(el("p", r.success_count + " of " + r.attempts.length + " connects succeeded (" + r.loss_percent +
      "% loss). Min " + fmt(r.min_latency_ms) + " ms, average " + fmt(r.avg_latency_ms) + " ms, max " + fmt(r.max_latency_ms) + " ms."));
    r.attempts.forEach(function (a, i) {
      resultBox.appendChild(el("div", "Attempt " + (i + 1) + ": " + (a.success ? a.latency_ms + " ms" : a.error), "muted"));
    });
    if (r.history_warning) { resultBox.appendChild(el("p", r.history_warning, "form-message error")); }
  }

  form.addEventListener("submit", async function (event) {
    event.preventDefault();
    say("Testing…");
    try {
      var response = await fetch("/api/network-test/run", {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ host: form.elements.host.value, port: form.elements.port.value })
      });
      var data = await response.json();
      if (!response.ok) { throw new Error(data.error || "The test failed."); }
      say("");
      showResult(data);
      loadEndpoints();
    } catch (error) { say(error.message, "error"); }
  });

  async function loadEndpoints() {
    var body = document.getElementById("endpoints");
    var data = await (await fetch("/api/network-test/list")).json();
    body.replaceChildren();
    document.getElementById("endpoints-empty").hidden = data.endpoints.length > 0;
    data.endpoints.forEach(function (e) {
      var row = el("tr");
      row.appendChild(el("td", e.host + ":" + e.port, "mono"));
      row.appendChild(el("td", e.description || ""));
      row.appendChild(el("td", e.latest.success ? "Reachable" : "Not reachable"));
      row.appendChild(el("td", fmt(e.latest.avg_latency_ms), "num"));
      row.appendChild(el("td", fmt(e.latest.loss_percent), "num"));
      var actions = el("td");
      var view = el("button", "History", "secondary");
      view.addEventListener("click", function () { showHistory(e); });
      var del = el("button", "Remove", "danger");
      del.addEventListener("click", async function () {
        if (!window.confirm("Remove this endpoint and its history?")) { return; }
        await fetch("/api/network-test/history?host=" + encodeURIComponent(e.host) + "&port=" + e.port, { method: "DELETE" });
        document.getElementById("history-panel").hidden = true;
        loadEndpoints();
      });
      actions.appendChild(view); actions.appendChild(document.createTextNode(" ")); actions.appendChild(del);
      row.appendChild(actions);
      body.appendChild(row);
    });
  }

  async function showHistory(e) {
    var data = await (await fetch("/api/network-test/history?host=" + encodeURIComponent(e.host) + "&port=" + e.port)).json();
    current = data;
    document.getElementById("history-panel").hidden = false;
    document.getElementById("history-title").textContent = "History — " + e.host + ":" + e.port;
    var svg = document.getElementById("history-chart");
    svg.replaceChildren();
    var points = data.samples.filter(function (s) { return s.avg_latency_ms !== null; });
    if (points.length < 2) { return; }
    var max = Math.max.apply(null, points.map(function (s) { return s.avg_latency_ms; })) || 1;
    var coords = points.map(function (s, i) {
      return (i / (points.length - 1) * 390 + 5) + "," + (75 - s.avg_latency_ms / max * 70);
    }).join(" ");
    var line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
    line.setAttribute("points", coords);
    line.setAttribute("fill", "none");
    line.setAttribute("stroke", "#325083");
    line.setAttribute("stroke-width", "2");
    svg.appendChild(line);
  }

  document.getElementById("history-export").addEventListener("click", function () {
    if (!current) { return; }
    var lines = ["timestamp,success,avg_latency_ms,min_latency_ms,max_latency_ms,loss_percent"];
    current.samples.forEach(function (s) {
      lines.push([s.timestamp, s.success, fmt(s.avg_latency_ms), fmt(s.min_latency_ms), fmt(s.max_latency_ms), s.loss_percent].join(","));
    });
    var link = document.createElement("a");
    link.href = URL.createObjectURL(new Blob([lines.join("\n")], { type: "text/csv" }));
    link.download = "network-test-" + current.host + "-" + current.port + ".csv";
    link.click();
  });

  loadEndpoints();
})();
