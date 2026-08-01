import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import {
  loadChecksums,
  expectedChecksumFor,
  ChecksumsManifestMissingError,
  ChecksumMissingForAssetError,
} from "./checksums-manifest.js";

function tempManifest(content: object): string {
  const p = path.join(os.tmpdir(), `berimor-checksums-test-${crypto.randomUUID()}.json`);
  fs.writeFileSync(p, JSON.stringify(content));
  return p;
}

test("loadChecksums бросает ChecksumsManifestMissingError на отсутствующем файле", () => {
  const missing = path.join(os.tmpdir(), `berimor-does-not-exist-${crypto.randomUUID()}.json`);
  assert.throws(() => loadChecksums(missing), ChecksumsManifestMissingError);
});

test("loadChecksums читает существующий файл", () => {
  const p = tempManifest({ "asset.tar.gz": "deadbeef" });
  const manifest = loadChecksums(p);
  assert.equal(manifest["asset.tar.gz"], "deadbeef");
  fs.rmSync(p);
});

test("expectedChecksumFor бросает ChecksumMissingForAssetError на отсутствующей записи", () => {
  const p = tempManifest({ "other.tar.gz": "deadbeef" });
  const manifest = loadChecksums(p);
  assert.throws(() => expectedChecksumFor(manifest, "asset.tar.gz", p), ChecksumMissingForAssetError);
  fs.rmSync(p);
});

test("expectedChecksumFor возвращает значение на существующей записи", () => {
  const p = tempManifest({ "asset.tar.gz": "cafebabe" });
  const manifest = loadChecksums(p);
  assert.equal(expectedChecksumFor(manifest, "asset.tar.gz", p), "cafebabe");
  fs.rmSync(p);
});
