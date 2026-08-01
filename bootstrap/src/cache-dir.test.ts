import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { resolveCacheDir } from "./cache-dir.js";

test("linux: XDG_CACHE_HOME задан", () => {
  const dir = resolveCacheDir("linux", { home: "/home/u", xdgCacheHome: "/custom/cache" });
  assert.equal(dir, path.join("/custom/cache", "berimor", "bin"));
});

test("linux: XDG_CACHE_HOME не задан — падаем на ~/.cache", () => {
  const dir = resolveCacheDir("linux", { home: "/home/u" });
  assert.equal(dir, path.join("/home/u", ".cache", "berimor", "bin"));
});

test("darwin: Library/Caches", () => {
  const dir = resolveCacheDir("darwin", { home: "/Users/u" });
  assert.equal(dir, path.join("/Users/u", "Library", "Caches", "berimor", "bin"));
});

test("win32: LOCALAPPDATA задан", () => {
  const dir = resolveCacheDir("win32", { home: "C:\\Users\\u", localAppData: "C:\\Users\\u\\AppData\\Local" });
  assert.equal(dir, path.join("C:\\Users\\u\\AppData\\Local", "berimor", "bin"));
});

test("win32: LOCALAPPDATA не задан — падаем на home/AppData/Local", () => {
  const dir = resolveCacheDir("win32", { home: "C:\\Users\\u" });
  assert.equal(dir, path.join("C:\\Users\\u", "AppData", "Local", "berimor", "bin"));
});
