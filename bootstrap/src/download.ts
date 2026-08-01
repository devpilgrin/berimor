/**
 * Скачивание платформенного артефакта с GitHub Releases репозитория проекта.
 *
 * Источник: docs/arch/deployment.md §2–3. ROADMAP: D3. Верификация — не
 * здесь: вызывающий код (index.ts) сверяет скачанный файл с пином из
 * checksums-manifest.ts ДО распаковки (checksum.ts) — эта функция только
 * переносит байты, ничего не проверяет и не исполняет.
 */
import fs from "node:fs";

export const RELEASE_REPOSITORY = "devpilgrin/berimor";

export function releaseAssetUrl(version: string, assetName: string): string {
  return `https://github.com/${RELEASE_REPOSITORY}/releases/download/v${version}/${assetName}`;
}

export class DownloadError extends Error {}

export async function downloadAsset(
  url: string,
  destPath: string,
  fetchImpl: typeof fetch = fetch,
): Promise<void> {
  const response = await fetchImpl(url);
  if (!response.ok) {
    throw new DownloadError(`скачивание ${url} завершилось с HTTP ${response.status}`);
  }
  const buffer = Buffer.from(await response.arrayBuffer());
  fs.writeFileSync(destPath, buffer);
}
