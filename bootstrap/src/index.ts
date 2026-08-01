#!/usr/bin/env node
/**
 * berimor — bootstrap-точка входа.
 *
 * Определяет платформу; при первом запуске скачивает и верифицирует
 * нативный артефакт по SHA-256, пришпиленному в checksums.json самого
 * npm-пакета (см. checksums-manifest.ts — почему не через `berimor verify`
 * на первой установке), атомарно распаковывает; дальше и при повторных
 * запусках делегирует нативному бинарнику с передачей аргументов.
 * Источник: docs/arch/deployment.md §5. ROADMAP: D3 (это), D4 (само-
 * обновление — не здесь).
 */
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { detectPlatform } from "./platform.js";
import { resolveCacheDir, currentEnv } from "./cache-dir.js";
import { downloadAsset, releaseAssetUrl } from "./download.js";
import { verifyChecksum } from "./checksum.js";
import { loadChecksums, expectedChecksumFor } from "./checksums-manifest.js";
import { extractAtomically } from "./extract.js";

export interface EnsureInstalledOptions {
  version: string;
  cacheDir: string;
  manifestPath?: string;
  fetchImpl?: typeof fetch;
}

export function ownVersion(): string {
  const distDir = path.dirname(fileURLToPath(import.meta.url));
  const packageJsonPath = path.join(distDir, "..", "package.json");
  const pkg = JSON.parse(fs.readFileSync(packageJsonPath, "utf-8")) as { version: string };
  return pkg.version;
}

/**
 * Гарантирует, что нативный бинарник версии `opts.version` присутствует в
 * `opts.cacheDir` — скачивает, верифицирует по пину и атомарно распаковывает
 * при необходимости; возвращает путь к бинарнику. Параметры инжектируются
 * (не читаются из реального окружения напрямую) ради тестируемости без
 * настоящей сети/npm — см. index.test.ts.
 */
export async function ensureInstalled(opts: EnsureInstalledOptions): Promise<string> {
  const info = detectPlatform(opts.version);
  const binaryName = info.platform === "win32" ? "berimor.exe" : "berimor";
  const targetDir = path.join(opts.cacheDir, opts.version);
  const binaryPath = path.join(targetDir, binaryName);

  if (fs.existsSync(binaryPath)) {
    return binaryPath;
  }

  const manifest = loadChecksums(opts.manifestPath);
  const expectedHex = expectedChecksumFor(manifest, info.assetName, opts.manifestPath);

  const tmpArchivePath = path.join(os.tmpdir(), `${randomUUID()}-${info.assetName}`);
  await downloadAsset(releaseAssetUrl(opts.version, info.assetName), tmpArchivePath, opts.fetchImpl ?? fetch);

  try {
    verifyChecksum(tmpArchivePath, expectedHex);
    extractAtomically(tmpArchivePath, targetDir, info.platform);
  } finally {
    fs.rmSync(tmpArchivePath, { force: true });
  }

  return binaryPath;
}

function delegate(binaryPath: string, args: string[]): void {
  const result = spawnSync(binaryPath, args, { stdio: "inherit" });
  process.exitCode = result.status ?? 1;
}

async function main(): Promise<void> {
  const version = ownVersion();
  const binaryPath = await ensureInstalled({
    version,
    cacheDir: resolveCacheDir(detectPlatform(version).platform, currentEnv()),
  });
  delegate(binaryPath, process.argv.slice(2));
}

const isMainModule = process.argv[1] === fileURLToPath(import.meta.url);
if (isMainModule) {
  main().catch((err) => {
    console.error(`[berimor] ${(err as Error).message}`);
    process.exitCode = 1;
  });
}
