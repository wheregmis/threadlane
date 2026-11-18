const status = document.getElementById("demo-status");
const demo = document.getElementById("live-demo");
if (window.crossOriginIsolated) {
  demo.hidden = false;
  demo.src = demo.dataset.src;
  demo.addEventListener("load", () => { status.hidden = true; });
} else if (!window.isSecureContext || !("serviceWorker" in navigator)) {
  status.textContent = "The interactive preview needs a browser with service workers over HTTPS or localhost. You can explore Threadlane using the desktop app.";
} else {
  // The isolation helper reloads once its service worker is ready.
  setTimeout(() => {
    status.textContent = "The preview could not start. Allow service workers for this site, then reload, or try a regular browser window.";
  }, 15000);
}
