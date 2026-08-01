/**
 * Атомарная распаковка платформенного архива в кэш.
 *
 * Источник: docs/arch/deployment.md §2 («атомарная распаковка»). ROADMAP: D3.
 * Распаковка — во временную директорию рядом с целевой (та же ФС — условие
 * атомарности rename), затем переименование на финальное имя одной
 * операцией: до переименования частично распакованное содержимое никогда
 * не видно под конечным путём. Используются нативные средства платформы
 * (`tar` на unix, `Expand-Archive` на Windows), не новая npm-зависимость —
 * то же соглашение, что и в D1 (нативные раннеры вместо кросс-инструментов).
 */
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import type { Platform } from "./platform.js";

export class ExtractError extends Error {}

function extractInto(archivePath: string, tmpDir: string, platform: Platform): void {
  if (platform === "win32") {
    const escaped = (p: string) => `'${p.replace(/'/g, "''")}'`;
    const command = `Expand-Archive -LiteralPath ${escaped(archivePath)} -DestinationPath ${escaped(tmpDir)} -Force`;
    const result = spawnSync("powershell", ["-NoProfile", "-NonInteractive", "-Command", command], {
      stdio: "pipe",
      encoding: "utf-8",
    });
    if (result.status !== 0) {
      throw new ExtractError(`Expand-Archive завершился с ошибкой: ${result.stderr || result.error?.message}`);
    }
    return;
  }
  const result = spawnSync("tar", ["-xzf", archivePath, "-C", tmpDir], {
    stdio: "pipe",
    encoding: "utf-8",
  });
  if (result.status !== 0) {
    throw new ExtractError(`tar завершился с ошибкой: ${result.stderr || result.error?.message}`);
  }
}

export function extractAtomically(archivePath: string, destDir: string, platform: Platform): void {
  if (fs.existsSync(destDir)) {
    return;
  }
  const parent = path.dirname(destDir);
  fs.mkdirSync(parent, { recursive: true });

  const tmpDir = `${destDir}.tmp-${randomUUID()}`;
  fs.mkdirSync(tmpDir, { recursive: true });

  try {
    extractInto(archivePath, tmpDir, platform);
    fs.renameSync(tmpDir, destDir);
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code;
    if (code === "ENOTEMPTY" || code === "EEXIST") {
      // Конкурентная/повторная установка уже материализовала destDir между
      // проверкой существования выше и переименованием — принимаем её как
      // валидную (она сама появилась тем же атомарным путём), не ошибка.
      fs.rmSync(tmpDir, { recursive: true, force: true });
      return;
    }
    fs.rmSync(tmpDir, { recursive: true, force: true });
    throw err;
  }
}
