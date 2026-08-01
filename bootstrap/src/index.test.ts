import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { ensureInstalled } from "./index.js";
import { detectPlatform } from "./platform.js";
import { sha256Hex } from "./checksum.js";
import { ChecksumMismatchError } from "./checksum.js";

const fixturesDir = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "fixtures", "golden", "bootstrap");
const sampleArchive = path.join(fixturesDir, "sample.tar.gz");

function tempDir(): string {
  const p = path.join(os.tmpdir(), `berimor-index-test-${crypto.randomUUID()}`);
  fs.mkdirSync(p, { recursive: true });
  return p;
}

function writeManifest(dir: string, assetName: string, hex: string): string {
  const p = path.join(dir, "checksums.json");
  fs.writeFileSync(p, JSON.stringify({ [assetName]: hex }));
  return p;
}

test("ensureInstalled: нет кэша → скачивает (фейковый fetch) → сверяет хэш → распаковывает", async () => {
  const version = "9.9.9-test";
  const info = detectPlatform(version);
  const archiveBytes = fs.readFileSync(sampleArchive);
  const correctHex = sha256Hex(sampleArchive);

  const workDir = tempDir();
  const manifestPath = writeManifest(workDir, info.assetName, correctHex);
  const cacheDir = path.join(workDir, "cache");

  const fakeFetch = (async () => new Response(new Blob([archiveBytes]), { status: 200 })) as typeof fetch;

  const binaryPath = await ensureInstalled({ version, cacheDir, manifestPath, fetchImpl: fakeFetch });

  const binaryName = info.platform === "win32" ? "berimor.exe" : "berimor";
  assert.equal(binaryPath, path.join(cacheDir, version, binaryName));
  // sample.tar.gz распаковывает hello.txt, не berimor(.exe) — реального
  // бинарника во фикстуре нет, важна только сама распаковка по месту.
  assert.equal(
    fs.readFileSync(path.join(cacheDir, version, "hello.txt"), "utf-8"),
    "berimor bootstrap fixture\n",
  );

  fs.rmSync(workDir, { recursive: true, force: true });
});

test("ensureInstalled: уже скачанный бинарник в кэше — сеть не трогается", async () => {
  const version = "9.9.9-cached";
  const info = detectPlatform(version);
  const binaryName = info.platform === "win32" ? "berimor.exe" : "berimor";

  const workDir = tempDir();
  const cacheDir = path.join(workDir, "cache");
  fs.mkdirSync(path.join(cacheDir, version), { recursive: true });
  fs.writeFileSync(path.join(cacheDir, version, binaryName), "already here");

  const fetchThatMustNotBeCalled = (async () => {
    throw new Error("сеть не должна была вызываться — бинарник уже в кэше");
  }) as unknown as typeof fetch;

  const binaryPath = await ensureInstalled({
    version,
    cacheDir,
    manifestPath: path.join(workDir, "does-not-matter.json"),
    fetchImpl: fetchThatMustNotBeCalled,
  });

  assert.equal(binaryPath, path.join(cacheDir, version, binaryName));
  fs.rmSync(workDir, { recursive: true, force: true });
});

test("ensureInstalled: подменённый архив (неверный хэш) — отказ без распаковки", async () => {
  const version = "9.9.9-tampered";
  const info = detectPlatform(version);
  const archiveBytes = fs.readFileSync(sampleArchive);
  const wrongHex = crypto.createHash("sha256").update("не тот файл").digest("hex");

  const workDir = tempDir();
  const manifestPath = writeManifest(workDir, info.assetName, wrongHex);
  const cacheDir = path.join(workDir, "cache");

  const fakeFetch = (async () => new Response(new Blob([archiveBytes]), { status: 200 })) as typeof fetch;

  await assert.rejects(
    () => ensureInstalled({ version, cacheDir, manifestPath, fetchImpl: fakeFetch }),
    ChecksumMismatchError,
  );

  assert.equal(fs.existsSync(path.join(cacheDir, version)), false);
  fs.rmSync(workDir, { recursive: true, force: true });
});
