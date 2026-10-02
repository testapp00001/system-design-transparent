// Loaded synchronously in <head> so the saved theme is applied before the
// first paint (no flash of the wrong theme). Kept tiny on purpose.
(function () {
  var theme = null;
  try { theme = localStorage.getItem("theme"); } catch (e) {}
  if (theme !== "light" && theme !== "dark" && theme !== "reader") {
    theme = window.matchMedia && matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  document.documentElement.setAttribute("data-theme", theme);
})();
