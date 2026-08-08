import { spawnSync } from "node:child_process";

const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const audit = spawnSync(
  npm,
  [
    "audit",
    "--prefix",
    "tools/extension-publisher",
    "--audit-level=moderate",
    "--json",
  ],
  { encoding: "utf8" },
);

if (audit.error) {
  throw audit.error;
}

let report;
try {
  report = JSON.parse(audit.stdout);
} catch (error) {
  process.stderr.write(audit.stderr);
  throw new Error(`npm audit did not return JSON: ${error.message}`);
}

if (report.error) {
  throw new Error(`npm audit failed: ${JSON.stringify(report.error)}`);
}

const vulnerabilities = report.vulnerabilities ?? {};
if (Object.keys(vulnerabilities).length === 0) {
  console.log("Extension publishing tools have no known vulnerabilities.");
  process.exit(0);
}

// image-size 2.0.2 is the newest published version and, as of 2026-08-08,
// addons-linter 10.10.0 pins it exactly. The affected parser runs only while
// linting this repository's trusted extension images. Keep the exception
// deliberately exact so a new advisory, dependency, or vulnerable path fails.
const expected = new Map([
  [
    "image-size",
    {
      advisorySources: new Set([1138808, 1138809]),
      dependencyNames: new Set(),
    },
  ],
  [
    "addons-linter",
    {
      advisorySources: new Set(),
      dependencyNames: new Set(["image-size"]),
    },
  ],
  [
    "web-ext",
    {
      advisorySources: new Set(),
      dependencyNames: new Set(["addons-linter"]),
    },
  ],
]);

const unexpected = [];
for (const [name, vulnerability] of Object.entries(vulnerabilities)) {
  const allowance = expected.get(name);
  if (!allowance) {
    unexpected.push(`${name}: unexpected vulnerable package`);
    continue;
  }
  for (const cause of vulnerability.via ?? []) {
    if (typeof cause === "string") {
      if (!allowance.dependencyNames.has(cause)) {
        unexpected.push(`${name}: unexpected vulnerable dependency ${cause}`);
      }
      continue;
    }
    if (!allowance.advisorySources.has(Number(cause.source))) {
      unexpected.push(`${name}: unexpected advisory ${cause.source ?? "unknown"}`);
    }
  }
}

for (const name of expected.keys()) {
  if (!vulnerabilities[name]) {
    unexpected.push(`${name}: expected exception chain changed; review the audit policy`);
  }
}

if (unexpected.length > 0) {
  process.stderr.write(`${unexpected.join("\n")}\n`);
  process.stderr.write(`${audit.stdout}\n`);
  process.exit(1);
}

console.warn(
  "Accepted only GHSA-w3rx-r6r6-pgpr and GHSA-5p2g-fcmc-qvqq " +
    "in the trusted extension-linting toolchain; no patched image-size release exists yet.",
);
