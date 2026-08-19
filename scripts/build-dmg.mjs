import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const projectRoot = process.cwd();
const packageInfo = JSON.parse(
  fs.readFileSync(path.join(projectRoot, "package.json"), "utf8"),
);
const releaseBundleDir = path.join(projectRoot, "src-tauri", "target", "release", "bundle");
const macosBundleDir = path.join(releaseBundleDir, "macos");
const dmgBundleDir = path.join(releaseBundleDir, "dmg");
const appPath = path.join(macosBundleDir, "waypoint.app");
const targetArch = process.arch === "arm64" ? "aarch64" : process.arch === "x64" ? "x86_64" : process.arch;
const dmgPath = path.join(
  dmgBundleDir,
  `${packageInfo.name}_${packageInfo.version}_${targetArch}.dmg`,
);
const buildEnv = {
  ...process.env,
  LANG: "en_US.UTF-8",
  LC_ALL: "en_US.UTF-8",
};

for (const bundleDir of [macosBundleDir, dmgBundleDir]) {
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
const appBuild = spawnSync(tauriCommand, ["build", "--bundles", "app"], {
  cwd: projectRoot,
  env: buildEnv,
  stdio: "inherit",
});

if (appBuild.error) {
  console.error(`Failed to run ${tauriCommand}: ${appBuild.error.message}`);
  process.exit(1);
}
if (appBuild.status !== 0 || !fs.existsSync(appPath)) {
  process.exit(appBuild.status ?? 1);
}

const stagingDir = fs.mkdtempSync(path.join(os.tmpdir(), "waypoint-dmg-"));
let dmgStatus = 1;
try {
  fs.cpSync(appPath, path.join(stagingDir, "waypoint.app"), { recursive: true });
  fs.symlinkSync("/Applications", path.join(stagingDir, "Applications"));

  const dmgBuild = spawnSync("hdiutil", [
    "create",
    "-volname",
    "waypoint",
    "-srcfolder",
    stagingDir,
    "-format",
    "UDZO",
    "-ov",
    dmgPath,
  ], {
    cwd: projectRoot,
    env: buildEnv,
    stdio: "inherit",
  });

  if (dmgBuild.error) {
    console.error(`Failed to run hdiutil: ${dmgBuild.error.message}`);
  } else {
    dmgStatus = dmgBuild.status ?? 1;
  }
} finally {
  fs.rmSync(stagingDir, { recursive: true, force: true });
}

process.exit(dmgStatus);
