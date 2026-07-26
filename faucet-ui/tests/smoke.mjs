#!/usr/bin/env node
// SPDX-License-Identifier: MIT OR Apache-2.0
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const qtMcpRoot = process.env.LOGOS_QT_MCP || resolve(testDirectory, "../result-mcp");
const { test, run } = await import(resolve(qtMcpRoot, "test-framework/framework.mjs"));

// The onboarding this release deletes. Every one of these strings was on the
// first screen of v0.2, which asked the user to choose a password that the
// underlying API accepted and then ignored, while key material went to a
// plaintext file. Asserting their absence is the point of the test: a
// regression here is not a cosmetic one.
const deletedSurface = [
  "Important: this testnet wallet is not encrypted",
  "Create wallet",
  "Recover an account",
  "Wallet password (not encryption)",
  "Confirm password",
  "I understand that this wallet file contains plaintext key material.",
  "Save your recovery phrase",
  "I saved my recovery phrase.",
  "Initialize account",
  "Fund an existing public account",
  "Target balance",
  "Stop after this claim",
  "Claim 150 LEZ",
];

async function expectAbsent(app, texts) {
  const present = [];
  for (const text of texts) {
    const found = await app.findByProperty("text", text);
    if (found.matches && found.matches.length > 0)
      present.push(text);
  }
  if (present.length > 0)
    throw new Error(`Deleted onboarding surface is still reachable: ${JSON.stringify(present)}`);
}

test("LEZ Faucet opens straight into the one-address request flow", async (app) => {
  await app.waitFor(
    async () => {
      await app.expectTexts([
        "LEZ Faucet",
        "Public LEZ address",
        "Request 150 LEZ",
        "Faucet pool",
      ]);
    },
    { timeout: 15000, interval: 500, description: "LEZ Faucet request screen" },
  );
});

test("no credential or account-creation surface exists", async (app) => {
  await expectAbsent(app, deletedSurface);
});

test("the button is inert until a public address is entered", async (app) => {
  // Nothing can be requested from an empty field: one address in, one credit
  // out, and no way to press the button before there is an address.
  const buttons = await app.findByProperty("text", "Request 150 LEZ");
  if (!buttons.matches || buttons.matches.length === 0)
    throw new Error("The request button is missing");
  const properties = await app.getProperties(buttons.matches[0].id);
  const enabled = (properties.properties || []).find((entry) => entry.name === "enabled");
  if (enabled && enabled.value === true)
    throw new Error("The request button is enabled with no address entered");
});

run();
