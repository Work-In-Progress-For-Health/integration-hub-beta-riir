// The Contacts page (spec/15-dashboard.md §10.3).
//
// Never builds markup from a string: names, e-mail addresses and notes are
// typed by people and shown to other people. The table is rendered by the
// server, which escapes it; this script posts forms and reloads, and writes
// messages with textContent.
(function () {
  "use strict";

  var form = document.getElementById("contact-form");
  if (!form) { return; }
  var message = document.getElementById("contact-message");
  var importMessage = document.getElementById("import-message");

  function say(el, text, kind) {
    el.textContent = text;
    el.className = "form-message " + (kind || "");
  }

  async function call(method, url, body, contentType) {
    var response = await fetch(url, {
      method: method,
      headers: contentType ? { "Content-Type": contentType } : {},
      body: body === undefined ? undefined : body
    });
    var data = await response.json().catch(function () { return {}; });
    if (!response.ok) {
      var detail = data.errors && data.errors.length ? " " + data.errors.join(" ") : "";
      throw new Error((data.error || "The request failed (" + response.status + ").") + detail);
    }
    return data;
  }

  function reset() {
    form.reset();
    form.elements.id.value = "";
    document.getElementById("form-title").textContent = "Add a contact";
    say(message, "");
  }
  document.getElementById("contact-reset").addEventListener("click", reset);

  form.addEventListener("submit", async function (event) {
    event.preventDefault();
    var id = form.elements.id.value;
    var payload = {
      first_name: form.elements.first_name.value,
      last_name: form.elements.last_name.value,
      email: form.elements.email.value,
      phone_number: form.elements.phone_number.value,
      organisation_type: form.elements.organisation_type.value,
      organisation_name: form.elements.organisation_name.value,
      notes: form.elements.notes.value,
      flow_ids: Array.prototype.map.call(form.elements.flow_ids.selectedOptions, function (o) { return o.value; })
    };
    say(message, "Saving…");
    try {
      await call(id ? "PUT" : "POST", id ? "/api/contacts/" + encodeURIComponent(id) : "/api/contacts",
        JSON.stringify(payload), "application/json");
      window.location.reload();
    } catch (error) {
      say(message, error.message, "error");
    }
  });

  document.querySelectorAll("[data-edit]").forEach(function (button) {
    button.addEventListener("click", function () {
      var contact = JSON.parse(button.getAttribute("data-edit"));
      form.elements.id.value = contact.id;
      ["first_name", "last_name", "email", "phone_number", "organisation_type", "organisation_name", "notes"]
        .forEach(function (name) { form.elements[name].value = contact[name] || ""; });
      Array.prototype.forEach.call(form.elements.flow_ids.options, function (option) {
        option.selected = (contact.flow_ids || []).indexOf(option.value) !== -1;
      });
      document.getElementById("form-title").textContent = "Edit contact";
      form.scrollIntoView();
    });
  });

  document.querySelectorAll("[data-delete]").forEach(function (button) {
    button.addEventListener("click", async function () {
      if (!window.confirm("Delete this contact? It can be restored for a limited time.")) { return; }
      try {
        await call("DELETE", "/api/contacts/" + encodeURIComponent(button.getAttribute("data-delete")));
        window.location.reload();
      } catch (error) { say(message, error.message, "error"); }
    });
  });

  document.querySelectorAll("[data-restore]").forEach(function (button) {
    button.addEventListener("click", async function () {
      try {
        await call("POST", "/api/contacts/" + encodeURIComponent(button.getAttribute("data-restore")) + "/restore");
        window.location.reload();
      } catch (error) { say(message, error.message, "error"); }
    });
  });

  document.getElementById("import-form").addEventListener("submit", async function (event) {
    event.preventDefault();
    var file = event.target.elements.file.files[0];
    if (!file) { return; }
    say(importMessage, "Importing…");
    try {
      var result = await call("POST", "/api/contacts/import", await file.text(), "text/csv");
      var text = "Created " + result.created + ", updated " + result.updated + ".";
      if (result.errors && result.errors.length) { text += " " + result.errors.join(" "); }
      say(importMessage, text, result.errors && result.errors.length ? "error" : "ok");
      if (result.created || result.updated) { window.setTimeout(function () { window.location.reload(); }, 1500); }
    } catch (error) {
      say(importMessage, error.message, "error");
    }
  });
})();
