const test = require("node:test");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const root = path.resolve(__dirname, "../..");
const extension = path.join(root, "extension");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");
const json = (file) => JSON.parse(read(file));

test("every extension locale has the complete non-empty English key set", () => {
  const english = json("extension/_locales/en/messages.json");
  const expected = Object.keys(english).sort();
  const localeRoot = path.join(extension, "_locales");
  const locales = fs.readdirSync(localeRoot).sort();
  assert.deepEqual(locales, ["ar", "de", "en", "es", "fr", "ja", "ko", "ru", "tr", "zh"]);
  for (const locale of locales) {
    const messages = json(`extension/_locales/${locale}/messages.json`);
    assert.deepEqual(Object.keys(messages).sort(), expected, `${locale} locale keys`);
    for (const [key, value] of Object.entries(messages)) {
      assert.equal(typeof value.message, "string", `${locale}.${key} message type`);
      assert.notEqual(value.message.trim(), "", `${locale}.${key} message is empty`);
    }
  }
});

test("all statically referenced extension messages exist", () => {
  const messages = json("extension/_locales/en/messages.json");
  const referenced = new Set();
  for (const name of ["background.js", "options.js", "popup.js", "options.html", "popup.html"]) {
    const source = read(`extension/${name}`);
    for (const match of source.matchAll(/\bt\(["']([^"']+)["']\)|data-i18n=["']([^"']+)["']/g)) {
      referenced.add(match[1] || match[2]);
    }
  }
  for (const key of referenced) assert.ok(messages[key], `missing English message ${key}`);
});

test("application, browser manifests, and native-host IDs stay synchronized", () => {
  const firefox = json("extension/manifest.json");
  const chromium = json("extension/manifest.chrome.json");
  const cargoVersion = /^version = "([0-9]+\.[0-9]+\.[0-9]+)"/m.exec(read("Cargo.toml"))[1];
  const versionSurfaces = [
    ["Firefox manifest", firefox.version],
    ["Chromium manifest", chromium.version],
    ["archived desktop package", json("apps/desktop/package.json").version],
    ["archived Tauri config", json("apps/desktop/src-tauri/tauri.conf.json").version],
    ["extension publisher package", json("tools/extension-publisher/package.json").version],
    ["extension publisher lockfile", json("tools/extension-publisher/package-lock.json").version],
    [
      "AppStream release",
      /<release version="([0-9]+\.[0-9]+\.[0-9]+)"/.exec(
        read("packaging/linux/io.minidownloader.app.metainfo.xml"),
      )[1],
    ],
  ];
  for (const directory of ["mini-downloader", "mini-downloader-bin", "mini-downloader-git"]) {
    versionSurfaces.push([
      `${directory} PKGBUILD`,
      /^pkgver=([0-9]+\.[0-9]+\.[0-9]+)/m.exec(
        read(`packaging/aur/${directory}/PKGBUILD`),
      )[1],
    ]);
    versionSurfaces.push([
      `${directory} .SRCINFO`,
      /^\s*pkgver = ([0-9]+\.[0-9]+\.[0-9]+)/m.exec(
        read(`packaging/aur/${directory}/.SRCINFO`),
      )[1],
    ]);
  }
  for (const [surface, version] of versionSurfaces) {
    assert.equal(version, cargoVersion, `${surface} version`);
  }

  const ipc = read("crates/minidl-ipc/src/lib.rs");
  const firefoxId = /pub const EXTENSION_ID: &str = "([^"]+)"/.exec(ipc)[1];
  const developmentId = /pub const CHROME_EXTENSION_ID: &str = "([^"]+)"/.exec(ipc)[1];
  assert.equal(firefox.browser_specific_settings.gecko.id, firefoxId);
  assert.equal(json("packaging/com.minidownloader.host.json").allowed_extensions[0], firefoxId);

  const digest = crypto.createHash("sha256").update(Buffer.from(chromium.key, "base64")).digest();
  const derivedId = [...digest.subarray(0, 16)]
    .flatMap((byte) => [byte >> 4, byte & 15])
    .map((nibble) => String.fromCharCode(97 + nibble))
    .join("");
  assert.equal(derivedId, developmentId);
  assert.equal(read("packaging/chrome-extension-id.txt").trim(), developmentId);
});
