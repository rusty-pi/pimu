// Runs what the emulator's worker does to get its threads, and says how it went.
// The probe runs again a few times: every run hands the cores to the pool anew.
import { loadThreaded } from "../threads.mjs";

const started = performance.now();
const glue = await loadThreaded();
const again = [];
for (let i = 0; glue && i < 5; i++) again.push(glue.parallel_probe());
self.postMessage({
  type: "result",
  isolated: self.crossOriginIsolated,
  threaded: glue !== null,
  pool: glue?.pool_threads() ?? 0,
  again,
  millis: Math.round(performance.now() - started),
});
