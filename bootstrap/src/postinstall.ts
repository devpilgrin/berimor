/**
 * postinstall-хук npm.
 *
 * Источник: docs/arch/deployment.md §9 — граница №4: «bootstrap не выполняет
 * ничего, кроме определения платформы, на этапе установки». Это крупнейшая
 * типовая поверхность атаки цепочки поставок для npm-пакетов, поэтому здесь
 * нет ни скачивания, ни исполнения чего-либо — только определение платформы.
 * Скачивание и верификация — на первом запуске (index.ts), не здесь.
 */
import { detectPlatform } from "./platform.js";

try {
  const info = detectPlatform(process.env.npm_package_version ?? "0.0.0");
  console.log(`[berimor] платформа определена: ${info.platform}/${info.arch}`);
} catch (err) {
  console.error(`[berimor] ${(err as Error).message}`);
  process.exitCode = 1;
}
