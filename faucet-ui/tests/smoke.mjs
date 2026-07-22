#!/usr/bin/env node
// SPDX-License-Identifier: MIT OR Apache-2.0
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const qtMcpRoot = process.env.LOGOS_QT_MCP || resolve(testDirectory, "../result-mcp");
const { test, run } = await import(resolve(qtMcpRoot, "test-framework/framework.mjs"));

test("LEZ Faucet plugin loads its safe first-run screen", async (app) => {
  await app.waitFor(
    async () => {
      await app.expectTexts([
        "LEZ Faucet",
        "Important: this testnet wallet is not encrypted",
        "Create wallet",
      ]);
    },
    { timeout: 15000, interval: 500, description: "LEZ Faucet first-run screen" },
  );
});

run();
