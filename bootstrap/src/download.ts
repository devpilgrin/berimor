/**
 * Скачивание платформенного артефакта с GitHub Releases доверенного репозитория.
 *
 * Источник: docs/arch/deployment.md §2–3, ADR-0017, ADR-0018. ROADMAP: D3.
 */

export async function downloadAsset(assetName: string, destPath: string): Promise<void> {
  void assetName;
  void destPath;
  // TODO(ROADMAP D3): скачать через GitHub Releases API, предварительно
  // сверив репозиторий с записью доверенного списка (deployment.md §4) —
  // доверие к репозиторию разрешает не спрашивать подтверждение на каждый
  // релиз, но не отменяет верификацию конкретного артефакта (verify.ts).
  throw new Error("not implemented: ROADMAP D3");
}
