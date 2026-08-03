/**
 * Определение платформы и имени ожидаемого артефакта.
 *
 * Источник: docs/arch/deployment.md §3. ROADMAP: D3.
 * Определение — кодом (os.platform()/os.arch()), без эвристик; неизвестная
 * комбинация — явный отказ, а не попытка угадать ближайшую (deployment.md §3).
 */
import os from "node:os";

export type Platform = "linux" | "darwin" | "win32";
export type Arch = "x64" | "arm64";

export interface PlatformInfo {
  platform: Platform;
  arch: Arch;
  assetName: string;
}

const SUPPORTED_PLATFORMS: Platform[] = ["linux", "darwin", "win32"];
const SUPPORTED_ARCHES: Arch[] = ["x64", "arm64"];

export function detectPlatform(version: string): PlatformInfo {
  const platform = os.platform();
  const arch = os.arch();

  if (!SUPPORTED_PLATFORMS.includes(platform as Platform)) {
    throw new Error(`неподдерживаемая платформа: ${platform}`);
  }
  if (!SUPPORTED_ARCHES.includes(arch as Arch)) {
    throw new Error(`неподдерживаемая архитектура: ${arch}`);
  }

  const ext = platform === "win32" ? "zip" : "tar.gz";
  // Имя артефакта — С `v` префикса версии: именно так называет файлы
  // build-матрица release.yml (berimor-v0.9.0-linux-x64.tar.gz), и
  // checksums.json пайплайн генерирует по фактическим именам. Первая
  // реальная установка v0.9.0 поймала рассинхрон (fail-closed сработал
  // верно: «нет записи в манифесте», не скачивание неизвестно чего).
  const assetName = `berimor-v${version}-${platform}-${arch}.${ext}`;

  return { platform: platform as Platform, arch: arch as Arch, assetName };
}
