// One worker thread of the pool the ARM cores run on (threads.mjs starts it).
// It loads the emulator on the shared memory it is handed, then serves the pool
// for good: `pool_thread` does not return.

import init, { pool_thread } from "./pkg-mt/pimu.js";

self.onmessage = async ({ data: { module, memory, stack } }) => {
  await init({ module_or_path: module, memory, thread_stack_size: stack });
  self.postMessage("ready");
  pool_thread();
};
