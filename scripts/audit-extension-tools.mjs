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

// deepmerge 4.3.1 is the newest published version as of 2026-11-10 and has no
// patched release. It is reached only through addons-linter, which runs on
// this repository's own extension files. Keep the exception deliberately exact
// so a new advisory, dependency, or vulnerable path fails.
// Advisories are matched by GHSA id: npm renumbers its numeric ids whenever a
// record is edited, which failed this gate with no real change.
const expected = new Map([
  [
    "deepmerge",
    {
      advisories: new Set(["GHSA-gcqx-w48m-w2h4"]),
      dependencyNames: new Set(),
    },
  ],
  [
    "addons-linter",
    {
      advisories: new Set(),
      dependencyNames: new Set(["deepmerge"]),
    },
  ],
  [
    "web-ext",
    {
      advisories: new Set(),
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
    const advisory = String(cause.url ?? "").split("/").pop() || "unknown";
    if (!allowance.advisories.has(advisory)) {
      unexpected.push(`${name}: unexpected advisory ${advisory}`);
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
  "Accepted only GHSA-gcqx-w48m-w2h4 (deepmerge, low severity) " +
    "in the trusted extension-linting toolchain; no patched deepmerge release exists yet.",
);
