// The alarm pauses page (spec/15-dashboard.md §10.2.8).
//
// Deliberately small, and deliberately never builds markup from a string:
// the reason and requested-by fields are typed by an operator, so the lists
// are rendered by the server (which escapes them) and this script only
// posts the form and reloads. Messages go in with textContent.
(function () {
  "use strict";

  var form = document.getElementById("pause-form");
  if (!form) { return; }
  var message = document.getElementById("pause-message");

  function say(text, kind) {
    message.textContent = text;
    message.className = "form-message " + (kind || "");
  }

  function sync() {
    var scope = form.elements.scope_type.value;
    var endMode = form.elements.end_mode.value;
    form.querySelectorAll("[data-for-scope]").forEach(function (el) {
      el.hidden = el.getAttribute("data-for-scope") !== scope;
    });
    form.querySelectorAll("[data-for-end]").forEach(function (el) {
      el.hidden = el.getAttribute("data-for-end") !== endMode;
    });
  }
  form.elements.scope_type.addEventListener("change", sync);
  form.elements.end_mode.addEventListener("change", sync);
  sync();

  function selected(name) {
    return Array.prototype.map.call(form.elements[name].selectedOptions, function (o) { return o.value; });
  }

  async function send(url, body) {
    var response = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body || {})
    });
    var data = await response.json().catch(function () { return {}; });
    if (!response.ok) { throw new Error(data.error || "The request failed (" + response.status + ")."); }
    return data;
  }

  form.addEventListener("submit", async function (event) {
    event.preventDefault();
    var scope = form.elements.scope_type.value;
    var payload = {
      scope_type: scope,
      targets: [],
      start: form.elements.start.value || null,
      end_mode: form.elements.end_mode.value,
      duration_minutes: Number(form.elements.duration_minutes.value),
      end: form.elements.end.value,
      reason: form.elements.reason.value,
      requested_by: form.elements.requested_by.value
    };
    if (scope === "flows") { payload.targets = selected("flows"); }
    if (scope === "rule") {
      payload.targets = selected("rules").map(function (value) {
        var parts = value.split("|");
        return { alarm_type: parts[0], rule_id: parts.slice(1).join("|") };
      });
    }
    say("Saving…");
    try {
      await send("/api/alarm-pauses", payload);
      window.location.reload();
    } catch (error) {
      say(error.message, "error");
    }
  });

  document.querySelectorAll("[data-cancel-pause]").forEach(function (button) {
    button.addEventListener("click", async function () {
      try {
        await send("/api/alarm-pauses/" + encodeURIComponent(button.getAttribute("data-cancel-pause")) + "/cancel");
        window.location.reload();
      } catch (error) {
        say(error.message, "error");
      }
    });
  });
})();
