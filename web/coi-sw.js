// A service worker that makes the page cross-origin isolated.
//
// The threaded build needs `SharedArrayBuffer`, which a page only gets when its
// documents and workers are served with `Cross-Origin-Opener-Policy:
// same-origin` and `Cross-Origin-Embedder-Policy: require-corp`. GitHub Pages
// cannot send headers, so this puts them on every response the page gets. A
// host that sends them itself never registers it (coi.mjs).

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

self.addEventListener("fetch", (event) => {
  const request = event.request;
  // A cache-only request from another origin cannot be answered from here.
  if (request.cache === "only-if-cached" && request.mode !== "same-origin") return;
  event.respondWith(
    fetch(request).then((response) => {
      // An opaque response has no headers to add to.
      if (response.status === 0) return response;
      const headers = new Headers(response.headers);
      headers.set("Cross-Origin-Embedder-Policy", "require-corp");
      headers.set("Cross-Origin-Opener-Policy", "same-origin");
      return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
    }),
  );
});
