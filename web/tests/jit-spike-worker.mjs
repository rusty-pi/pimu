// The worker half of jit-spike.html: the spike in each build this page has.
import init, { jit_spike as plainSpike } from "../pkg/pimu.js";
import { loadThreaded } from "../threads.mjs";

self.addEventListener("message", async ({ data }) => {
  if (data?.type !== "start") return;
  await init();
  const result = { isolated: self.crossOriginIsolated, plain: JSON.parse(plainSpike()) };
  try {
    const glue = await loadThreaded();
    if (glue) result.threaded = JSON.parse(glue.jit_spike());
  } catch (error) {
    result.threadedError = String(error);
  }
  self.postMessage({ type: "result", ...result });
});
