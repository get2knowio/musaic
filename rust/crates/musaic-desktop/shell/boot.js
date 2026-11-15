// Boot page shown while the in-process server starts. When startup fails
// the shell navigates here with `#error=<message>`; this shows it as text.
(function () {
  var h = location.hash;
  if (h.indexOf("#error=") !== 0) return;
  var msg = decodeURIComponent(h.slice(7));
  document.getElementById("status").textContent = "Musaic failed to start.";
  var d = document.getElementById("detail");
  d.textContent = msg;
  d.hidden = false;
})();
