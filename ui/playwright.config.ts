import { defineConfig, devices } from '@playwright/test';

/**
 * The e2e suite runs against a real static build served by a plain file
 * server, not against the dev server.
 *
 * The reason is specific: adapter-static produces a `build/` directory and the
 * ticket's acceptance command is a build, so the thing worth testing is the
 * built artifact. A dev-server test would pass while the static export was
 * broken, and a broken static export is exactly the failure the desktop shell
 * would hit (spec 3.3) with a webview pointed at a directory on disk.
 */
export default defineConfig({
  testDir: 'e2e',
  // One worker: the suite asserts on DOM node counts, and a second suite
  // running concurrently competes for CPU on a busy machine, which changes
  // how many rows have rendered by the time the assertion runs. That would
  // make a correct grid look flaky, and a flaky assertion gets deleted.
  workers: 1,
  fullyParallel: false,
  reporter: [['list']],
  timeout: 30_000,
  use: {
    baseURL: 'http://127.0.0.1:4173',
    trace: 'off'
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] }
    }
  ],
  webServer: {
    // `vite preview` serves the built directory, which is the artifact under
    // test. It needs no backend: the tests stub the network, which is also
    // how the grid is developed -- the backend is a placeholder crate today.
    command: 'node node_modules/vite/bin/vite.js preview --port 4173 --host 127.0.0.1',
    url: 'http://127.0.0.1:4173',
    reuseExistingServer: !process.env.CI,
    timeout: 60_000
  }
});
