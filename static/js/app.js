// Small progressive enhancements. Everything works without JavaScript; this
// only adds the theme switcher and keeps it in sync.
(function () {
  "use strict";

  function currentTheme() {
    return document.documentElement.getAttribute("data-theme") || "light";
  }

  function syncButtons() {
    var theme = currentTheme();
    document.querySelectorAll("[data-set-theme]").forEach(function (btn) {
      btn.setAttribute("aria-pressed", String(btn.getAttribute("data-set-theme") === theme));
    });
  }

  document.addEventListener("click", function (event) {
    var btn = event.target.closest("[data-set-theme]");
    if (!btn) return;
    var theme = btn.getAttribute("data-set-theme");
    document.documentElement.setAttribute("data-theme", theme);
    try { localStorage.setItem("theme", theme); } catch (e) {}
    syncButtons();
  });

  // Follow OS changes only while the visitor hasn't chosen a theme.
  if (window.matchMedia) {
    matchMedia("(prefers-color-scheme: dark)").addEventListener("change", function (e) {
      var saved = null;
      try { saved = localStorage.getItem("theme"); } catch (err) {}
      if (!saved) {
        document.documentElement.setAttribute("data-theme", e.matches ? "dark" : "light");
        syncButtons();
      }
    });
  }

  // The back button restores htmx's snapshot of the page, but the search box
  // and filters in that snapshot may not match the results (the snapshot is
  // taken after the visitor has already typed the next query). The URL is the
  // source of truth, so re-fill the form from it.
  function syncSearchFormWithUrl() {
    var form = document.querySelector("form.search");
    if (!form) return;
    var params = new URLSearchParams(window.location.search);
    form.querySelectorAll("input[name], select[name]").forEach(function (field) {
      field.value = params.get(field.name) || "";
      if (field.tagName === "SELECT" && field.selectedIndex < 0) field.selectedIndex = 0;
    });
  }
  document.addEventListener("htmx:historyRestore", syncSearchFormWithUrl);

  document.addEventListener("DOMContentLoaded", syncButtons);
  // htmx restores pages from its history cache without a DOMContentLoaded.
  document.addEventListener("htmx:historyRestore", syncButtons);
})();
