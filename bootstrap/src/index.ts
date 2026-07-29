#!/usr/bin/env node
/**
 * berimor — bootstrap-точка входа.
 *
 * Определяет платформу; при первом запуске скачивает и верифицирует
 * нативный артефакт; дальше делегирует ему выполнение с передачей
 * аргументов. Источник: arch/deployment.md §5. ROADMAP: D3, D4.
 */
import { detectPlatform } from "./platform.js";
import { downloadAsset } from "./download.js";
import { verifyArtifact } from "./verify.js";

async function main(): Promise<void> {
  const version = process.env.BERIMOR_VERSION ?? "0.0.0";
  const info = detectPlatform(version);

  console.log(`[berimor] целевой артефакт: ${info.assetName}`);

  // TODO(ROADMAP D3): проверить локальный кэш; при отсутствии — скачать,
  // сверить с доверенным списком (arch/deployment.md §4), верифицировать
  // через verifyArtifact(), атомарно распаковать — только затем делегировать
  // выполнение нативному бинарнику с передачей process.argv.
  await downloadAsset(info.assetName, "");
  verifyArtifact("", "");
}

main().catch((err) => {
  console.error(`[berimor] ${(err as Error).message}`);
  process.exitCode = 1;
});
