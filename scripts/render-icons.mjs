// Renders the Parley SVG artwork to the PNGs Tauri needs, using Playwright's
// Chromium (already a dev dependency). Then run `npx tauri icon` on the
// 1024px master to produce icon.icns / icon.ico and the standard sizes.
import { chromium } from "playwright";
import { readFile, mkdir } from "node:fs/promises";
import path from "node:path";
import { execFileSync } from "node:child_process";

const root = path.resolve(import.meta.dirname, "..");
const design = path.join(root, "design");
const icons = path.join(root, "src-tauri", "icons");

const renders = [
  ["parley-icon.svg", path.join(design, "parley-icon-1024.png"), 1024],
  ["tray-idle.svg", path.join(icons, "tray-idle.png"), 44],
  ["tray-recording.svg", path.join(icons, "tray-recording.png"), 44],
];

await mkdir(icons, { recursive: true });
const browser = await chromium.launch();
const page = await browser.newPage({ deviceScaleFactor: 1 });
for (const [svgName, out, size] of renders) {
  const svg = await readFile(path.join(design, svgName), "utf8");
  await page.setViewportSize({ width: size, height: size });
  await page.setContent(
    `<html><body style="margin:0;background:transparent">
       <div style="width:${size}px;height:${size}px">${svg.replace(
         "<svg ",
         `<svg width="${size}" height="${size}" `,
       )}</div></body></html>`,
  );
  await page.locator("div").screenshot({ path: out, omitBackground: true });
  console.log(`rendered ${path.relative(root, out)}`);
}
await browser.close();

execFileSync(
  "npx",
  ["tauri", "icon", path.join(design, "parley-icon-1024.png"), "-o", icons],
  { stdio: "inherit", cwd: root },
);
