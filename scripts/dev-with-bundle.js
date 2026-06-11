#!/usr/bin/env node
/**
 * dev-with-bundle.js
 *
 * Wraps `tauri dev` and watches the compiled debug binary.
 * Whenever the binary is (re)compiled, this script recreates the
 * macOS .app bundle next to it so the Dock icon is always correct.
 */

const { spawn, execSync } = require("child_process");
const fs = require("fs");
const path = require("path");

const ROOT = path.join(__dirname, "..");
const BINARY = path.join(ROOT, "src-tauri/target/debug/waypoint");
const APP_BUNDLE = path.join(ROOT, "src-tauri/target/debug/waypoint.app");
const ICNS_SRC = path.join(ROOT, "src-tauri/icons/icon.icns");
const LSREGISTER =
  "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

const PLIST = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>waypoint</string>
    <key>CFBundleDisplayName</key>
    <string>Waypoint</string>
    <key>CFBundleIdentifier</key>
    <string>com.waypoint.desktop</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleExecutable</key>
    <string>waypoint</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
</dict>
</plist>`;

function log(msg) {
  process.stdout.write(`\x1b[36m[dev-bundle]\x1b[0m ${msg}\n`);
}

function createBundle() {
  if (!fs.existsSync(BINARY)) return;

  const contents = path.join(APP_BUNDLE, "Contents");
  const macos = path.join(contents, "MacOS");
  const resources = path.join(contents, "Resources");

  fs.mkdirSync(macos, { recursive: true });
  fs.mkdirSync(resources, { recursive: true });

  // Binary
  fs.copyFileSync(BINARY, path.join(macos, "waypoint"));
  fs.chmodSync(path.join(macos, "waypoint"), 0o755);

  // Icon
  if (fs.existsSync(ICNS_SRC)) {
    fs.copyFileSync(ICNS_SRC, path.join(resources, "AppIcon.icns"));
  }

  // Info.plist
  fs.writeFileSync(path.join(contents, "Info.plist"), PLIST);

  // Touch bundle so Finder notices
  const now = new Date();
  try { fs.utimesSync(APP_BUNDLE, now, now); } catch (_) {}

  // Register with Launch Services (non-fatal)
  try {
    execSync(`"${LSREGISTER}" -f "${APP_BUNDLE}"`, { stdio: "ignore" });
  } catch (_) {}

  log(`Updated → ${APP_BUNDLE}`);
}

// ── Poll for binary changes every 2 s ────────────────────────────────────────
let lastMtime = null;
const watcher = setInterval(() => {
  try {
    if (!fs.existsSync(BINARY)) return;
    const { mtimeMs } = fs.statSync(BINARY);
    if (mtimeMs !== lastMtime) {
      lastMtime = mtimeMs;
      createBundle();
    }
  } catch (_) {}
}, 2000);

// Run once immediately in case binary already exists
createBundle();

// ── Spawn tauri dev ───────────────────────────────────────────────────────────
log("Starting tauri dev…");
const tauri = spawn(
  process.platform === "win32" ? "npx.cmd" : "npx",
  ["tauri", "dev"],
  { cwd: ROOT, stdio: "inherit" }
);

tauri.on("close", (code) => {
  clearInterval(watcher);
  process.exit(code ?? 0);
});

function shutdown() {
  clearInterval(watcher);
  tauri.kill("SIGINT");
}
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
