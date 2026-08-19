import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

const projectRoot = process.cwd();
const bundleDirs = ["macos", "dmg"].map((bundleType) =>
  path.join(projectRoot, "src-tauri", "target", "release", "bundle", bundleType),
);

for (const bundleDir of bundleDirs) {
  if (!fs.existsSync(bundleDir)) {
    continue;
  }
  for (const entry of fs.readdirSync(bundleDir)) {
    if (entry.endsWith(".dmg")) {
      fs.rmSync(path.join(bundleDir, entry), { force: true });
    }
  }
}

const tauriCommand = process.platform === "win32" ? "tauri.cmd" : "tauri";
const result = spawnSync(tauriCommand, ["build", "--bundles", "dmg"], {
  cwd: projectRoot,
  env: {
    ...process.env,
    LANG: "en_US.UTF-8",
    LC_ALL: "en_US.UTF-8",
  },
  stdio: "inherit",
});

if (result.error) {
  console.error(`Failed to run ${tauriCommand}: ${result.error.message}`);
  process.exit(1);
}

process.exit(result.status ?? 1);
