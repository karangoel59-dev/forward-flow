import { execFileSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = readFileSync(join(root, "src-tauri/icon-src/icon.svg"), "utf8");
const defs = source.match(/<defs>[\s\S]*?<\/defs>/)?.[0];
const mark = source.match(/<g id="mark">[\s\S]*?<\/g>/)?.[0];
if (!defs || !mark) throw new Error("Icon source must contain defs and a mark group.");

const canvas = (content) => `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">${content}</svg>\n`;
const centeredMark = (scale) => `<g transform="translate(512 512) scale(${scale}) translate(-549 -479)">${mark}</g>`;
const scratch = mkdtempSync(join(tmpdir(), "forward-flow-icons-"));

try {
  writeFileSync(join(scratch, "desktop.svg"), source);
  const mobile = `${defs}<rect width="1024" height="1024" fill="url(#cover)"/>${centeredMark(1.25)}`;
  writeFileSync(join(scratch, "mobile.svg"), canvas(mobile));
  // Keep the complete mark inside Android's central 66dp safe circle on a 108dp layer.
  writeFileSync(join(scratch, "foreground.svg"), canvas(centeredMark(0.94)));
  writeFileSync(join(scratch, "desktop.json"), JSON.stringify({ default: "desktop.svg", bg_color: "#636849" }));
  writeFileSync(join(scratch, "mobile.json"), JSON.stringify({ default: "mobile.svg", bg_color: "#636849", android_fg: "foreground.svg" }));

  const cli = join(root, "node_modules/@tauri-apps/cli/tauri.js");
  for (const platform of ["desktop", "mobile"]) {
    execFileSync(process.execPath, [cli, "icon", join(scratch, `${platform}.json`), "--output", join(scratch, platform)], { cwd: root, stdio: "inherit" });
  }

  // Tauri 2.11 emits 49px legacy hdpi icons; render those at Android's required 72px.
  for (const [name, mask] of [
    ["ic_launcher", '<rect x="85" y="85" width="854" height="854" rx="85"/>'],
    ["ic_launcher_round", '<circle cx="512" cy="512" r="471"/>'],
  ]) {
    const input = join(scratch, `${name}.svg`);
    const output = join(scratch, name);
    writeFileSync(input, canvas(`<defs><clipPath id="mask">${mask}</clipPath></defs><g clip-path="url(#mask)">${mobile}</g>`));
    execFileSync(process.execPath, [cli, "icon", input, "--png", "72", "--output", output], { cwd: root, stdio: "inherit" });
    cpSync(join(output, "72x72.png"), join(scratch, "mobile/android/mipmap-hdpi", `${name}.png`));
  }

  const destination = join(root, "src-tauri/icons");
  cpSync(join(scratch, "desktop"), destination, { recursive: true });
  for (const platform of ["android", "ios"]) {
    cpSync(join(scratch, "mobile", platform), join(destination, platform), { recursive: true });
  }
  mkdirSync(join(root, "public"), { recursive: true });
  writeFileSync(join(root, "public/app-icon.svg"), source);
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
