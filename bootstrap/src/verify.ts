/**
 * Верификация подписи скачанного артефакта.
 *
 * Источник: docs/arch/deployment.md §9, ADR-0025 — bootstrap не реализует
 * криптографическую проверку сам, а вызывает подкоманду `berimor verify`
 * уже доверенного нативного бинарника, а не переизобретает её на другом
 * языке (риск расхождения двух независимых реализаций одной проверки).
 * ROADMAP: D3.
 */
import { spawnSync } from "node:child_process";

export function verifyArtifact(nativeBinaryPath: string, artifactPath: string): boolean {
  const result = spawnSync(nativeBinaryPath, ["verify", artifactPath], {
    stdio: "inherit",
  });
  return result.status === 0;
}
