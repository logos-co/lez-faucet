import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import test from "node:test";

function commandExists(command) {
  return spawnSync(command, ["--version"], { stdio: "ignore" }).status === 0;
}

function designSystemImportPath() {
  const configured = process.env.LOGOS_DESIGN_SYSTEM_IMPORT_PATH;
  if (configured && existsSync(configured))
    return configured;
  if (!existsSync("/nix/store"))
    return "";
  const match = readdirSync("/nix/store")
    .filter((entry) => /-logos-design-system-[^/]+$/.test(entry))
    .filter((entry) => existsSync(`/nix/store/${entry}/lib/Logos/Controls/qmldir`))
    .sort()
    .pop();
  return match ? `/nix/store/${match}/lib` : "";
}

test("FaucetView instantiates in a real QML engine", { timeout: 5000 }, async (t) => {
  const importPath = designSystemImportPath();
  if (!commandExists("qml") || !importPath) {
    t.skip("qml or Logos Design System import path is unavailable");
    return;
  }

  const view = new URL("../../src/qml/FaucetView.qml", import.meta.url).pathname;
  const child = spawn("qml", ["-I", importPath, view], {
    env: { ...process.env, QT_QPA_PLATFORM: "offscreen" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let output = "";
  child.stdout.on("data", (chunk) => { output += chunk; });
  child.stderr.on("data", (chunk) => { output += chunk; });

  await new Promise((resolve, reject) => {
    let intentionallyStopped = false;
    const timer = setTimeout(() => {
      intentionallyStopped = true;
      child.kill("SIGTERM");
    }, 750);
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      clearTimeout(timer);
      if (!intentionallyStopped && code !== 0)
        reject(new Error(`QML exited before the smoke window (${code ?? signal}):\n${output}`));
      else
        resolve();
    });
  });

  assert.doesNotMatch(output, /failed to load|Cannot assign|Type .* unavailable/i, output);
});
