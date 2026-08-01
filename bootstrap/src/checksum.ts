/**
 * Верификация скачанного архива по SHA-256 из checksums.json (см.
 * checksums-manifest.ts) — решение циркулярности первой установки: см.
 * docstring verify.ts и ROADMAP D3. Сравнение — константным временем, раз
 * один из операндов получен из недоверенного источника (скачанный файл).
 */
import crypto from "node:crypto";
import fs from "node:fs";

export function sha256Hex(filePath: string): string {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(filePath));
  return hash.digest("hex");
}

export class ChecksumMismatchError extends Error {
  constructor(filePath: string, expectedHex: string, actualHex: string) {
    super(
      `контрольная сумма не совпала для ${filePath}: ожидалось ${expectedHex}, получено ${actualHex} — файл не распаковывается`,
    );
    this.name = "ChecksumMismatchError";
  }
}

export function verifyChecksum(filePath: string, expectedHex: string): void {
  const actualHex = sha256Hex(filePath);
  const expected = Buffer.from(expectedHex, "hex");
  const actual = Buffer.from(actualHex, "hex");
  if (expected.length !== actual.length || !crypto.timingSafeEqual(expected, actual)) {
    throw new ChecksumMismatchError(filePath, expectedHex, actualHex);
  }
}
