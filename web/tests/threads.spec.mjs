import { expect, test } from "./fixtures.mjs";

test.describe("without threads", () => {
  test("the option is offered but cannot be ticked", async ({ app }) => {
    await app.open("soc");
    await app.drawer.locator("details summary").click();
    await expect(app.drawer.getByRole("checkbox", { name: /worker threads/ })).toBeDisabled();
  });
});

test.describe("on a page that can have threads", () => {
  test.use({ isolated: true });

  test("ticking the option reaches the request", async ({ app }) => {
    await app.open("soc");
    await app.drawer.locator("details summary").click();
    await app.drawer.getByRole("checkbox", { name: /worker threads/ }).check();
    expect((await app.localState()).advanced.parallel).toBe(true);
    await app.powerOn();
    expect((await app.request()).options.parallel).toBe(true);
  });

  test("the worker's request for threads starts the pool and gets an answer", async ({ app }) => {
    await app.powerOn();
    await app.emit({ type: "spawn-threads", module: {}, memory: {}, stack: 2097152, count: 2 });
    await expect.poll(async () => (await app.workers()).filter((w) => w.url.includes("pool-thread.mjs")).length).toBe(2);
    await app.page.evaluate(() => window.__workers.filter((w) => w.url.includes("pool-thread.mjs")).forEach((w) => w.emit({})));
    await expect
      .poll(async () => (await app.sent()).filter((m) => m.type === "threads-spawned"))
      .toEqual([{ type: "threads-spawned" }]);
  });

  test("a pool that fails is reported to the worker", async ({ app }) => {
    await app.powerOn();
    await app.emit({ type: "spawn-threads", module: {}, memory: {}, stack: 2097152, count: 1 });
    await expect.poll(async () => (await app.workers()).some((w) => w.url.includes("pool-thread.mjs"))).toBe(true);
    await app.page.evaluate(() => window.__workers.find((w) => w.url.includes("pool-thread.mjs")).onerror?.({ message: "no thread" }));
    await expect
      .poll(async () => (await app.sent()).find((m) => m.type === "threads-spawned"))
      .toEqual({ type: "threads-spawned", error: "Error: no thread" });
  });
});
