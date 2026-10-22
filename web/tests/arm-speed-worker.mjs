// The worker half of arm-speed.html: one run of each build and mode per
// round, so a host that slows down partway through costs every mode alike.
import init, { arm_speed as plainSpeed } from "../pkg/pimu.js";
import { loadThreaded } from "../threads.mjs";

self.addEventListener("message", async ({ data }) => {
  if (data?.type !== "start") return;
  await init();
  const glue = await loadThreaded();
  const modes = [
    ["plain, taking turns", () => plainSpeed(false, data.insns)],
    ["threaded build, taking turns", glue && (() => glue.arm_speed(false, data.insns))],
    ["threaded build, on threads", glue && (() => glue.arm_speed(true, data.insns))],
  ].filter(([, run]) => run);
  const runs = [];
  for (let rep = 0; rep < data.reps; rep++) {
    for (const [mode, run] of modes) {
      const [threaded, insns, secs] = run();
      runs.push({ mode, rep, threaded: threaded === 1, insns, secs, mips: insns / secs / 1e6 });
      self.postMessage({ type: "progress", runs });
    }
  }
  const mean = Object.fromEntries(
    modes.map(([mode]) => {
      const mine = runs.filter((r) => r.mode === mode);
      return [mode, mine.reduce((sum, r) => sum + r.secs, 0) / mine.length];
    }),
  );
  self.postMessage({ type: "result", isolated: self.crossOriginIsolated, cores: navigator.hardwareConcurrency, mean, runs });
});
