import { defineConfig } from "@playwright/test";

// `PW_CHANNEL=chrome` uses an installed Chrome instead of the downloaded Chromium.
export default defineConfig({
  testDir: "tests",
  testMatch: "**/*.spec.js",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL: "http://localhost:4173",
    channel: process.env.PW_CHANNEL || undefined,
    viewport: { width: 1280, height: 900 },
    trace: "retain-on-failure",
  },
  webServer: {
    command: "node tests/server.mjs",
    url: "http://localhost:4173",
    reuseExistingServer: !process.env.CI,
  },
});
