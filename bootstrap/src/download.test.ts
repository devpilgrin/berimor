import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { downloadAsset, releaseAssetUrl, DownloadError, RELEASE_REPOSITORY } from "./download.js";

function tempDestPath(): string {
  return path.join(os.tmpdir(), `berimor-download-test-${crypto.randomUUID()}`);
}

test("releaseAssetUrl строит URL на GitHub Releases репозитория проекта", () => {
  const url = releaseAssetUrl("1.2.3", "berimor-1.2.3-linux-x64.tar.gz");
  assert.equal(url, `https://github.com/${RELEASE_REPOSITORY}/releases/download/v1.2.3/berimor-1.2.3-linux-x64.tar.gz`);
});

test("downloadAsset пишет тело успешного ответа в destPath", async () => {
  const dest = tempDestPath();
  const fakeFetch = (async () =>
    new Response(new Blob([Buffer.from("archive-bytes")]), { status: 200 })) as typeof fetch;
  await downloadAsset("https://example.invalid/asset", dest, fakeFetch);
  assert.equal(fs.readFileSync(dest, "utf-8"), "archive-bytes");
  fs.rmSync(dest, { force: true });
});

test("downloadAsset бросает DownloadError на не-2xx ответе", async () => {
  const fakeFetch = (async () => new Response(null, { status: 404 })) as typeof fetch;
  await assert.rejects(
    () => downloadAsset("https://example.invalid/missing", tempDestPath(), fakeFetch),
    DownloadError,
  );
});
