import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { sha256Hex, verifyChecksum, ChecksumMismatchError } from "./checksum.js";

function tempFileWith(content: string): string {
  const p = path.join(os.tmpdir(), `berimor-checksum-test-${crypto.randomUUID()}`);
  fs.writeFileSync(p, content);
  return p;
}

test("sha256Hex совпадает с независимо посчитанным хэшем", () => {
  const p = tempFileWith("hello berimor");
  const expected = crypto.createHash("sha256").update("hello berimor").digest("hex");
  assert.equal(sha256Hex(p), expected);
  fs.rmSync(p);
});

test("verifyChecksum не бросает на совпадающем хэше", () => {
  const p = tempFileWith("ok");
  const hex = crypto.createHash("sha256").update("ok").digest("hex");
  assert.doesNotThrow(() => verifyChecksum(p, hex));
  fs.rmSync(p);
});

test("verifyChecksum бросает ChecksumMismatchError на несовпадающем хэше", () => {
  const p = tempFileWith("tampered");
  const wrongHex = crypto.createHash("sha256").update("original").digest("hex");
  assert.throws(() => verifyChecksum(p, wrongHex), ChecksumMismatchError);
  fs.rmSync(p);
});
