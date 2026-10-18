// Gets the page cross-origin isolated when the host cannot (see coi-sw.js).
//
// The first visit has no service worker yet, so the page registers it, waits
// for it to take over and reloads once; every later visit is isolated from the
// start. A browser that refuses a service worker, or a page that stays
// un-isolated after the reload, runs as before: the cores take turns.

const RELOADED = "pimu-coi-reload";

async function isolate() {
  if (globalThis.crossOriginIsolated || !("serviceWorker" in navigator) || !globalThis.isSecureContext) return;
  try {
    await navigator.serviceWorker.register(new URL("./coi-sw.js", import.meta.url));
    await navigator.serviceWorker.ready;
    if (sessionStorage.getItem(RELOADED)) return;
    sessionStorage.setItem(RELOADED, "1");
    location.reload();
    await new Promise(() => {});
  } catch (error) {
    console.warn("the page stays un-isolated:", error);
  }
}

await isolate();
