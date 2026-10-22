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
    ["threaded build, on threads, wasm JIT", glue && data.jit && (() => {
      glue.set_wasm_jit(data.jit);
      const run = glue.arm_speed(true, data.insns);
      glue.set_wasm_jit(0);
      return run;
    })],
  ].filter(([mode, run]) => run && (!data.only || mode.includes(data.only)));
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
  const jit = glue?.wasm_jit_stats?.();
  self.postMessage({ type: "result", jit: jit && { runs: jit[0], modules: jit[1], compileUs: jit[2], entries: jit[3], retired: jit[4], inline: jit[5], called: jit[6] }, isolated: self.crossOriginIsolated, cores: navigator.hardwareConcurrency, mean, runs });
});
