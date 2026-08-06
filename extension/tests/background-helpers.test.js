const test = require("node:test");
const assert = require("node:assert/strict");

const {
  addBoundedMedia,
  cookieHeader,
  cookieQuery,
  enrichCaptureJob,
  isHostDisabled,
  normalizeDisabledHosts,
} = require("../background-helpers.js");

test("cookie lookup is scoped to each captured target, not the source page", async () => {
  const queries = [];
  const job = await enrichCaptureJob(
    { url: "https://files.example/download.zip", kind: "http" },
    { url: "https://account.example/private", cookieStoreId: "firefox-container-2" },
    async (query) => {
      queries.push(query);
      return [{ name: "target", value: "only" }];
    },
  );

  assert.deepEqual(queries, [
    { url: "https://files.example/download.zip", storeId: "firefox-container-2" },
  ]);
  assert.equal(job.cookie, "target=only");
  assert.equal(job.referrer, "https://account.example/private");
});

test("an existing request cookie is preserved without a second lookup", async () => {
  let called = false;
  const job = await enrichCaptureJob(
    { url: "https://example.org/file", cookie: "signed=request" },
    { url: "https://example.org/" },
    async () => {
      called = true;
      return [];
    },
  );
  assert.equal(job.cookie, "signed=request");
  assert.equal(called, false);
});

test("cookie queries omit an unavailable container id", () => {
  assert.deepEqual(cookieQuery("https://example.org/", undefined), {
    url: "https://example.org/",
  });
});

test("cookie headers stay inside the native-message byte limit", () => {
  const cookies = [
    { name: "first", value: "one" },
    { name: "second", value: "two" },
  ];
  assert.equal(cookieHeader(cookies, 9), "first=one");
  assert.equal(cookieHeader([{ name: "x", value: "é" }], 4), "x=é");
  assert.equal(cookieHeader([{ name: "x", value: "é" }], 3), "");
});

test("media storage is deduplicated, bounded, and rejects local schemes", () => {
  let list = [];
  for (let index = 0; index < 120; index += 1) {
    list = addBoundedMedia(list, { url: `https://cdn.example/${index}.m3u8`, type: "hls" });
  }
  assert.equal(list.length, 100);
  assert.equal(list[0].url, "https://cdn.example/20.m3u8");
  assert.equal(addBoundedMedia(list, list[0]).length, 100);
  assert.deepEqual(addBoundedMedia([], { url: "file:///etc/passwd", type: "file" }), []);
});

test("disabled hosts are normalized and matched exactly", () => {
  const hosts = normalizeDisabledHosts([
    " Example.COM. ",
    "https://media.example.net/path",
    "example.com",
    "not a host",
  ]);
  assert.deepEqual(hosts, ["example.com", "media.example.net"]);
  assert.equal(isHostDisabled("https://EXAMPLE.com/file", hosts), true);
  assert.equal(isHostDisabled("https://sub.example.com/file", hosts), false);
});
