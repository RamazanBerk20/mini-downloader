(function (root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  root.ldmOptionsHelpers = api;
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  const SIZE_UNITS = Object.freeze({
    B: 1,
    KB: 1024,
    MB: 1024 * 1024,
    GB: 1024 * 1024 * 1024,
  });

  function validSizeUnit(unit) {
    return Object.hasOwn(SIZE_UNITS, unit);
  }

  function preferredSizeUnit(bytes) {
    if (bytes >= SIZE_UNITS.GB) return "GB";
    if (bytes >= SIZE_UNITS.MB) return "MB";
    if (bytes >= SIZE_UNITS.KB) return "KB";
    return "B";
  }

  function displaySize(bytes, unit) {
    if (!validSizeUnit(unit)) return "0";
    const value = Math.max(0, Number(bytes) || 0) / SIZE_UNITS[unit];
    return String(Number(value.toFixed(value >= 100 ? 1 : 3)));
  }

  function sizeToBytes(value, unit) {
    if (!Number.isFinite(value) || value < 0 || !validSizeUnit(unit)) return null;
    return Math.min(Number.MAX_SAFE_INTEGER, Math.round(value * SIZE_UNITS[unit]));
  }

  return { SIZE_UNITS, displaySize, preferredSizeUnit, sizeToBytes, validSizeUnit };
});
