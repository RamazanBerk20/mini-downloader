const test = require("node:test");
const assert = require("node:assert/strict");

const {
  addBoundedMedia,
  cookieHeader,
  cookieQuery,
  createHandoffRegistry,
  defaultReferrer,
  filenameFromCD,
  isBlacklisted,
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
  // Another host learns the origin, never the private page path.
  assert.equal(job.referrer, "https://account.example/");
});

test("a synthesized referrer follows the browser's default policy", () => {
  assert.equal(
    defaultReferrer("https://user:pw@a.example/doc?id=1#frag", "https://a.example/file.zip"),
    "https://a.example/doc?id=1",
  );
  assert.equal(
    defaultReferrer("https://a.example/doc?token=secret", "https://cdn.example/file.zip"),
    "https://a.example/",
  );
  assert.equal(defaultReferrer("https://a.example/doc", "http://cdn.example/file.zip"), undefined);
  assert.equal(defaultReferrer("https://a.example/doc", "magnet:?xt=urn:btih:abc"), undefined);
  assert.equal(defaultReferrer("about:blank", "https://cdn.example/file.zip"), undefined);
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

test("Content-Disposition filenames keep apostrophes and honour RFC 5987", () => {
  const url = "https://files.example/path/fallback.bin";
  assert.equal(filenameFromCD('attachment; filename="Bob\'s report.pdf"', url), "Bob's report.pdf");
  assert.equal(filenameFromCD("attachment; filename=Bob's.pdf", url), "Bob's.pdf");
  assert.equal(filenameFromCD('attachment; filename="say \\"hi\\".txt"', url), 'say "hi".txt');
  assert.equal(
    filenameFromCD("attachment; filename*=UTF-8'en'na%C3%AFve%20file.txt; filename=\"x.txt\"", url),
    "naïve file.txt",
  );
  assert.equal(filenameFromCD("attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf", url), "résumé.pdf");
  assert.equal(filenameFromCD("inline", url), "fallback.bin");
  assert.equal(filenameFromCD("", "https://files.example/a%20b.zip"), "a b.zip");
});

test("Path A handoffs are per URL and cookie store, and settle into states", async () => {
  let clock = 0;
  const registry = createHandoffRegistry({ ttlMs: 5000, now: () => clock });
  const url = "https://files.example/big.iso";
  let accept;
  registry.remember(url, "container-1", new Promise((resolve) => (accept = resolve)));
  registry.remember(url, "container-2", Promise.resolve({ ok: false }));

  assert.equal(registry.recent(url, "container-1").state, "pending");
  assert.equal(registry.recent(url, undefined), null);
  const decision = registry.decide(url, "container-1");
  accept({ ok: true });
  assert.equal(await decision, "accepted");
  // The other container's capture was rejected: Path B takes over.
  assert.equal(await registry.decide(url, "container-2"), "fallback");
  assert.equal(registry.recent(url, "container-2").state, "fallback");
  // A late reply cannot rewrite a decided record.
  assert.equal(registry.recent(url, "container-1").state, "accepted");

  registry.remember(url, undefined, Promise.reject(new Error("host down")));
  assert.equal(await registry.decide(url, undefined), "fallback");

  clock = 6000;
  assert.equal(registry.recent(url, "container-1"), null);
  assert.equal(await registry.decide(url, "container-1"), "none");
});

test("the file-type blacklist sees both the clicked URL and where it redirected", () => {
  const list = ["exe"];
  const clicked = "https://site.example/get/setup.exe";
  const final = "https://cdn.example/blob/8f3a9c";
  assert.equal(isBlacklisted([clicked, final], "", list), true);
  assert.equal(isBlacklisted([final, clicked], "", list), true);
  assert.equal(isBlacklisted([final], "setup.EXE", list), true);
  assert.equal(isBlacklisted([final], "notes.txt", list), false);
  assert.equal(isBlacklisted([clicked], "", []), false);
});
