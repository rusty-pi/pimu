import { expect, test } from "./fixtures.js";

test.describe("the serial console", () => {
  test("what the board prints appears, and a new power-on clears it", async ({ app }) => {
    await app.running();
    await app.emitOutput("Booting Linux on physical CPU 0x0\n");
    await app.emitOutput("login: ");
    await expect(app.term).toHaveText("Booting Linux on physical CPU 0x0\nlogin: ");
    await app.powerButton.click();
    await app.powerButton.click();
    await expect(app.term).toHaveText("");
  });

  test("carriage returns, backspaces and escape sequences are handled", async ({ app }) => {
    await app.running();
    await app.emitOutput("one\r\ntwo\x1b[1;32m green\x1b[0m\r\n");
    await app.emitOutput("abc\b \bd\n");
    await app.emitOutput("\b\bx\n");
    await app.emitOutput("\x1bcreset\x1b]0;title\x07 done\n");
    await expect(app.term).toHaveText("one\ntwo green\nabd\nx\nreset done\n");
  });

  test("a window title is not printed", async ({ app }) => {
    await app.running();
    await app.emitOutput("\x1b]0;pi@raspberrypi: ~\x07$ \x1b]2;other\x1b\\ok\n");
    await expect(app.term).toHaveText("$ ok\n");
  });

  test("a character split over two messages arrives whole", async ({ app }) => {
    await app.running();
    await app.page.evaluate(() => {
      const bytes = new TextEncoder().encode("é");
      const worker = window.__machine();
      worker.emit({ type: "output", bytes: bytes.slice(0, 1) });
      worker.emit({ type: "output", bytes: bytes.slice(1) });
    });
    await expect(app.term).toHaveText("é");
  });

  test("a very long console is trimmed from the top", async ({ app }) => {
    await app.running();
    await app.emitOutput("first\n");
    await app.page.evaluate(() => {
      const worker = window.__machine();
      for (let i = 0; i < 9; i++) worker.emit({ type: "output", bytes: new TextEncoder().encode("x".repeat(50_000) + "\n") });
      worker.emit({ type: "output", bytes: new TextEncoder().encode("last\n") });
    });
    await expect(app.term).toContainText("last");
    const text = await app.term.textContent();
    expect(text.length).toBeLessThan(400_000);
    expect(text).not.toContain("first");
  });

  test("it stays at the bottom while you are there, and where you left it when you scroll up", async ({ app }) => {
    await app.running();
    const lines = (from, to) => Array.from({ length: to - from }, (_, i) => `line ${from + i}`).join("\n") + "\n";
    await app.emitOutput(lines(0, 200));
    await expect.poll(() => app.term.evaluate((el) => el.scrollTop + el.clientHeight >= el.scrollHeight - 6)).toBe(true);
    await app.term.evaluate((el) => (el.scrollTop = 0));
    await app.emitOutput(lines(200, 400));
    await expect(app.term).toContainText("line 399");
    expect(await app.term.evaluate((el) => el.scrollTop)).toBe(0);
  });
});

test.describe("typing at the console", () => {
  const keysSent = async (app) =>
    (await app.sent()).filter((m) => m.type === "keys").map((m) => m.keys.values);

  test("keys go to the board as bytes", async ({ app }) => {
    await app.running();
    await app.term.focus();
    await app.page.keyboard.type("ls");
    await app.page.keyboard.press("Enter");
    await app.page.keyboard.press("Backspace");
    await app.page.keyboard.press("Tab");
    await app.page.keyboard.press("Escape");
    await app.page.keyboard.press("Control+c");
    await app.page.keyboard.press("Control+D");
    await app.page.evaluate(() => document.getElementById("term").dispatchEvent(new KeyboardEvent("keydown", { key: "é", bubbles: true, cancelable: true })));
    expect(await keysSent(app)).toEqual([[108], [115], [13], [127], [9], [27], [3], [4], [195, 169]]);
  });

  test("keys that are not text are left alone", async ({ app }) => {
    await app.running();
    await app.term.focus();
    await app.page.keyboard.press("ArrowUp");
    await app.page.keyboard.press("F5");
    await app.page.keyboard.press("Alt+x");
    expect(await keysSent(app)).toEqual([]);
  });

  test("pasted text is typed, with its newlines as returns", async ({ app }) => {
    await app.running();
    await app.term.focus();
    await app.page.evaluate(() => {
      const data = new DataTransfer();
      data.setData("text", "echo hi\nexit\r\n");
      document.getElementById("term").dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
    });
    const [paste] = (await app.sent()).filter((m) => m.type === "keys");
    expect(paste.keys.__bytes).toBe("echo hi\rexit\r".length);
  });

  test("nothing is sent when the board is off", async ({ app }) => {
    await app.term.focus();
    await app.page.keyboard.type("ls");
    expect(await app.workers()).toHaveLength(0);
    await app.page.evaluate(() => {
      const data = new DataTransfer();
      data.setData("text", "x");
      document.getElementById("term").dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
    });
    expect((await app.workers()).flatMap((w) => w.sent).filter((m) => m.type === "keys")).toEqual([]);
  });

  test("starting the board focuses the console", async ({ app }) => {
    await app.powerOn();
    await expect(app.term).toBeFocused();
  });
});
