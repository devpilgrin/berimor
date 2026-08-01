/**
 * Локальный кэш скачанных и распакованных платформенных артефактов.
 *
 * Источник: docs/arch/deployment.md §2 (bootstrap кэширует результат
 * определения платформы и — здесь — сами скачанные бинарники, повторный
 * запуск с уже установленной версией не должен снова обращаться к сети).
 * ROADMAP: D3.
 */
import path from "node:path";
import type { Platform } from "./platform.js";

export interface CacheDirEnv {
  home: string;
  xdgCacheHome?: string;
  localAppData?: string;
}

export function resolveCacheDir(platform: Platform, env: CacheDirEnv): string {
  if (platform === "win32") {
    const base = env.localAppData ?? path.join(env.home, "AppData", "Local");
    return path.join(base, "berimor", "bin");
  }
  if (platform === "darwin") {
    return path.join(env.home, "Library", "Caches", "berimor", "bin");
  }
  const base = env.xdgCacheHome ?? path.join(env.home, ".cache");
  return path.join(base, "berimor", "bin");
}

export function currentEnv(): CacheDirEnv {
  return {
    home: process.env.HOME ?? process.env.USERPROFILE ?? "",
    xdgCacheHome: process.env.XDG_CACHE_HOME,
    localAppData: process.env.LOCALAPPDATA,
  };
}
