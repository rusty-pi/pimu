// The ARM cores on worker threads, for a page that can have them.
//
// The threaded build (pkg-mt/) is the emulator linked for shared memory. It
// needs `SharedArrayBuffer`, which a page only gets when it is cross-origin
// isolated (coi-serviceworker.mjs arranges that), and worker threads for the
// cores to run on. WebAssembly has no threads of its own, so the page starts
// them: each loads the module on the same memory and joins a pool on the Rust
// side. The page does it, not the emulator's worker, because not every browser
// lets a worker start workers, and one that does loads the script only while
// the creator's event loop turns, which the emulator then blocks for a whole
// slice at a time.
//
// Two halves talk over the emulator worker's message channel. In the worker,
// `loadThreaded` compiles the module, makes the memory and asks the page to
// start the threads (`spawn-threads`); on the page, `ThreadPool` starts them
// and the page answers `threads-spawned`.

export const CORE_THREADS = 4;
const STACK_BYTES = 2 * 1024 * 1024;

export const canHaveThreads = () =>
  globalThis.crossOriginIsolated === true && typeof SharedArrayBuffer === "function";

// ---- the emulator worker's half ---------------------------------------------

function askPageForThreads(request) {
  return new Promise((resolve, reject) => {
    const answer = ({ data }) => {
      if (data?.type !== "threads-spawned") return;
      self.removeEventListener("message", answer);
      data.error ? reject(new Error(data.error)) : resolve();
    };
    self.addEventListener("message", answer);
    self.postMessage({ type: "spawn-threads", ...request });
  });
}

// The threaded build with its core threads running, or null: no isolation, no
// build, no workers, or a build that does not count right.
export async function loadThreaded() {
  if (!canHaveThreads()) return null;
  try {
    const glue = await import("./pkg-mt/pimu.js");
    const module = await WebAssembly.compileStreaming(fetch(new URL("./pkg-mt/pimu_bg.wasm", import.meta.url)));
    const wasm = await glue.default({ module_or_path: module, thread_stack_size: STACK_BYTES });
    await askPageForThreads({ module, memory: wasm.memory, stack: STACK_BYTES, count: CORE_THREADS });
    if (!glue.threads_built() || glue.pool_threads() < CORE_THREADS || !glue.parallel_probe()) {
      throw new Error("the threaded build did not pass its check");
    }
    return glue;
  } catch (error) {
    console.warn("cores stay on one thread:", error);
    return null;
  }
}

// ---- the page's half ---------------------------------------------------------

export class ThreadPool {
  #workers = [];

  // Start `count` workers on `module` and `memory`; resolves when all have
  // loaded the module and are waiting for work.
  async start({ module, memory, stack, count }) {
    const starting = Array.from({ length: count }, () => this.#startOne(module, memory, stack));
    await Promise.all(starting);
  }

  #startOne(module, memory, stack) {
    return new Promise((resolve, reject) => {
      const worker = new Worker(new URL("./pool-thread.mjs", import.meta.url), { type: "module" });
      this.#workers.push(worker);
      worker.onmessage = () => resolve();
      worker.onerror = (event) => reject(new Error(event.message || "a core thread did not start"));
      worker.postMessage({ module, memory, stack });
    });
  }

  terminate() {
    for (const worker of this.#workers) worker.terminate();
    this.#workers = [];
  }
}
