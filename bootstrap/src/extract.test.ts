import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { extractAtomically } from "./extract.js";

const fixturesDir = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "fixtures", "golden", "bootstrap");

function tempDestDir(): string {
  return path.join(os.tmpdir(), `berimor-extract-test-${crypto.randomUUID()}`);
}

test("unix: tar.gz распаковывается в целевую директорию", () => {
  const archive = path.join(fixturesDir, "sample.tar.gz");
  const dest = tempDestDir();
  extractAtomically(archive, dest, "linux");
  const content = fs.readFileSync(path.join(dest, "hello.txt"), "utf-8");
  assert.equal(content, "berimor bootstrap fixture\n");
  fs.rmSync(dest, { recursive: true, force: true });
});

test("повторный вызов на уже существующую целевую директорию — не падает", () => {
  const archive = path.join(fixturesDir, "sample.tar.gz");
  const dest = tempDestDir();
  extractAtomically(archive, dest, "linux");
  assert.doesNotThrow(() => extractAtomically(archive, dest, "linux"));
  const content = fs.readFileSync(path.join(dest, "hello.txt"), "utf-8");
  assert.equal(content, "berimor bootstrap fixture\n");
  fs.rmSync(dest, { recursive: true, force: true });
});

test("win32: zip распаковывается в целевую директорию (только на Windows)", (t) => {
  if (os.platform() !== "win32") {
    t.skip("Expand-Archive недоступен вне Windows — CI (bootstrap job) прогоняет только ubuntu-latest");
    return;
  }
  const archive = path.join(fixturesDir, "sample.zip");
  const dest = tempDestDir();
  extractAtomically(archive, dest, "win32");
  const content = fs.readFileSync(path.join(dest, "hello.txt"), "utf-8");
  assert.equal(content, "berimor bootstrap fixture\n");
  fs.rmSync(dest, { recursive: true, force: true });
});
