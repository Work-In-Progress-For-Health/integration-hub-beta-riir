// The flow source servers screen (spec/15-dashboard.md §10.4.5). The table is
// rendered by the server, which escapes it; this script only posts.
(function () {
  "use strict";
  var form = document.getElementById("source-form");
  if (!form) { return; }
  var message = document.getElementById("source-message");
  var importMessage = document.getElementById("import-message");

  function say(el, text, kind) { el.textContent = text; el.className = "form-message " + (kind || ""); }
  async function call(method, url, body, type) {
    var response = await fetch(url, { method: method, headers: type ? { "Content-Type": type } : {}, body: body });
    var data = await response.json().catch(function () { return {}; });
    if (!response.ok) {
      var more = data.errors && data.errors.length ? " " + data.errors.join(" ") : "";
      throw new Error((data.error || "The request failed (" + response.status + ").") + more);
    }
    return data;
  }
  function reset() {
    form.reset(); form.elements.id.value = "";
    document.getElementById("form-title").textContent = "Add a flow source server";
    say(message, "");
  }
  document.getElementById("source-reset").addEventListener("click", reset);

  form.addEventListener("submit", async function (event) {
    event.preventDefault();
    var id = form.elements.id.value;
    var payload = JSON.stringify({ description: form.elements.description.value, url: form.elements.url.value, port: form.elements.port.value });
    try {
      await call(id ? "PUT" : "POST", id ? "/api/network-test/sources/" + encodeURIComponent(id) : "/api/network-test/sources", payload, "application/json");
      window.location.reload();
    } catch (error) { say(message, error.message, "error"); }
  });

  document.querySelectorAll("[data-edit]").forEach(function (button) {
    button.addEventListener("click", function () {
      var p = button.getAttribute("data-edit").split("|");
      form.elements.id.value = p[0]; form.elements.description.value = p.slice(1, p.length - 2).join("|");
      form.elements.url.value = p[p.length - 2]; form.elements.port.value = p[p.length - 1];
      document.getElementById("form-title").textContent = "Edit flow source server";
    });
  });
  document.querySelectorAll("[data-delete]").forEach(function (button) {
    button.addEventListener("click", async function () {
      if (!window.confirm("Delete this flow source server?")) { return; }
      try { await call("DELETE", "/api/network-test/sources/" + encodeURIComponent(button.getAttribute("data-delete"))); window.location.reload(); }
      catch (error) { say(message, error.message, "error"); }
    });
  });
  document.getElementById("import-form").addEventListener("submit", async function (event) {
    event.preventDefault();
    var file = event.target.elements.file.files[0];
    if (!file) { return; }
    try {
      var r = await call("POST", "/api/network-test/sources/import", await file.text(), "text/csv");
      var text = "Imported " + r.imported + "." + (r.errors.length ? " " + r.errors.join(" ") : "");
      say(importMessage, text, r.errors.length ? "error" : "ok");
      if (r.imported) { window.setTimeout(function () { window.location.reload(); }, 1500); }
    } catch (error) { say(importMessage, error.message, "error"); }
  });
})();
