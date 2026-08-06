const test = require("node:test");
const assert = require("node:assert/strict");

const {
  displaySize,
  preferredSizeUnit,
  sizeToBytes,
  validSizeUnit,
} = require("../options-helpers.js");

test("file-size units convert to exact stored bytes", () => {
  assert.equal(sizeToBytes(1, "KB"), 1024);
  assert.equal(sizeToBytes(1.5, "MB"), 1_572_864);
  assert.equal(sizeToBytes(2, "GB"), 2_147_483_648);
  assert.equal(sizeToBytes(-1, "MB"), null);
  assert.equal(sizeToBytes(Number.NaN, "MB"), null);
  assert.equal(sizeToBytes(1, "TB"), null);
});

test("existing byte settings get a readable unit", () => {
  assert.equal(preferredSizeUnit(512), "B");
  assert.equal(preferredSizeUnit(1024), "KB");
  assert.equal(preferredSizeUnit(1024 * 1024), "MB");
  assert.equal(preferredSizeUnit(1024 * 1024 * 1024), "GB");
  assert.equal(displaySize(1024 * 1024, "MB"), "1");
  assert.equal(displaySize(1536, "KB"), "1.5");
  assert.equal(validSizeUnit("TB"), false);
});
