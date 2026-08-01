/**
 * Пин SHA-256 платформенных артефактов внутри самого npm-пакета.
 *
 * Публикуется в checksums.json рядом с package.json на этапе `publish` job
 * release.yml (см. комментарий там) — доверие к первой установке приходит
 * из npm-канала, независимого от GitHub Release, который эти же файлы
 * раздаёт (см. docstring verify.ts и ROADMAP D3 про циркулярность). Нет
 * файла или нет записи под конкретный артефакт — явный отказ, не молчаливый
 * пропуск верификации (тот же принцип, что и в остальном проекте).
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export type ChecksumsManifest = Record<string, string>;

function defaultManifestPath(): string {
  const distDir = path.dirname(fileURLToPath(import.meta.url));
  return path.join(distDir, "..", "checksums.json");
}

export class ChecksumsManifestMissingError extends Error {
  constructor(manifestPath: string) {
    super(
      `не найден список контрольных сумм (${manifestPath}) — bootstrap не может верифицировать первую установку, скачивание отклонено`,
    );
    this.name = "ChecksumsManifestMissingError";
  }
}

export class ChecksumMissingForAssetError extends Error {
  constructor(assetName: string, manifestPath: string) {
    super(
      `в списке контрольных сумм (${manifestPath}) нет записи для ${assetName} — скачивание отклонено`,
    );
    this.name = "ChecksumMissingForAssetError";
  }
}

export function loadChecksums(manifestPath: string = defaultManifestPath()): ChecksumsManifest {
  if (!fs.existsSync(manifestPath)) {
    throw new ChecksumsManifestMissingError(manifestPath);
  }
  const raw = fs.readFileSync(manifestPath, "utf-8");
  return JSON.parse(raw) as ChecksumsManifest;
}

export function expectedChecksumFor(
  manifest: ChecksumsManifest,
  assetName: string,
  manifestPath: string = defaultManifestPath(),
): string {
  const hex = manifest[assetName];
  if (!hex) {
    throw new ChecksumMissingForAssetError(assetName, manifestPath);
  }
  return hex;
}
