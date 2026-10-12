// Captures the screenshot gallery (src/screenshots) with Playwright and runs a
// few smoke checks on the interactive mock library. Start `npm run dev` first.
import { chromium } from "playwright";
import { mkdir } from "node:fs/promises";
import path from "node:path";

const OUTPUT_DIR =
  process.env.SCREENSHOT_OUTPUT_DIR ??
  path.join(process.env.HOME, "parley-screenshots");
const baseUrl = process.env.SCREENSHOT_BASE_URL ?? "http://localhost:1420";

const SCREENS = [
  "01-library",
  "02-library-recording",
  "03-library-transcribing",
  "04-library-failed",
  "05-library-empty",
  "06-transcription",
  "06b-profile-editor-gladia",
  "06c-profile-delete-confirm",
  "07-vocabulary",
  "08-recording-settings",
  "09-claude",
  "10-permissions",
  "11-widget-idle",
  "12-widget-recording",
  "13-widget-paused",
  "14-widget-transcribing",
  "15-library-delete-confirm",
  "16-widget-marked",
  "17-widget-note",
];

// Screens captured from another screen's root after an interaction.
const DERIVED = {
  "06b-profile-editor-gladia": [
    "06-transcription",
    "Edit profile Gladia",
    ".profile-editor-dialog",
  ],
  "06c-profile-delete-confirm": [
    "06-transcription",
    "Delete profile Groq",
    ".config-reset-dialog",
  ],
  "15-library-delete-confirm": [
    "01-library",
    "Delete meeting",
    ".config-reset-dialog",
  ],
};

function fail(message) {
  throw new Error(message);
}

async function overflowing(page, root) {
  return page.locator(root).evaluate((el) => {
    const selectors = [
      ".layout",
      ".main-content",
      ".record-bar",
      ".meeting-item",
      ".meeting-toolbar",
      ".meeting-actions",
      ".settings-section",
      ".widget",
      ".widget-row",
      ".widget-live",
      ".widget-note",
      ".marker-body",
      ".marker-composer",
      ".transcript-tools",
      ".profile-list",
    ];
    return selectors.flatMap((selector) =>
      [...el.querySelectorAll(selector)]
        .filter((node) => node.scrollWidth > node.clientWidth + 1)
        .map((node) => ({
          selector,
          client: node.clientWidth,
          scroll: node.scrollWidth,
        })),
    );
  });
}

async function waitFor(check, message, debug, timeoutMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await check()) return;
    await new Promise((r) => setTimeout(r, 50));
  }
  if (debug) await debug();
  fail(message);
}

async function smoke(page) {
  const root = page.locator('[id="01-library"]');
  const title = root.locator(".title-input");
  // Selecting another meeting swaps the detail pane.
  await root.getByRole("button", { name: /Vendor call/ }).click();
  await waitFor(
    async () =>
      (await title.inputValue()) === "Vendor call — observability renewal",
    "selecting a meeting did not open it",
    async () => console.log("title is", await title.inputValue()),
  );
  await root.getByRole("button", { name: /Q4 budget review/ }).click();
  // Search filters the list (titles and transcript text).
  const search = root.getByRole("searchbox", { name: "Search meetings" });
  await search.fill("reserved capacity");
  await waitFor(
    async () => (await root.locator(".meeting-item").count()) === 1,
    "search did not filter the list",
  );
  await search.fill("zzz-nothing");
  await root.getByText("No matches").waitFor();
  await search.fill("");
  // The in-transcript filter narrows the turns; speakers keep their colours.
  const find = root.getByRole("searchbox", { name: "Find in transcript" });
  const turnsBefore = await root.locator(".turn").count();
  await find.fill("observability");
  const turnsAfter = await root.locator(".turn").count();
  if (!(turnsAfter > 0 && turnsAfter < turnsBefore)) {
    fail(`transcript filter: ${turnsBefore} → ${turnsAfter}`);
  }
  await find.fill("");
  // Highlights and notes sit inline; the filter finds notes by their text.
  const markers = await root.locator(".marker").count();
  if (markers !== 3) fail(`expected 3 inline markers, got ${markers}`);
  await find.fill("quote");
  await waitFor(
    async () =>
      (await root.locator(".marker").count()) === 1 &&
      (await root.locator(".turn").count()) === 1,
    "the transcript filter did not match a note by its text",
  );
  await find.fill("");
  // A note opens for editing with its text; Escape leaves it unchanged.
  await root.getByRole("button", { name: "Edit note at 00:00:57" }).click();
  const edit = root.getByRole("textbox", { name: "Edit note at 00:00:57" });
  if (!(await edit.inputValue()).startsWith("Send Finance"))
    fail("note editor did not start with the note text");
  await edit.press("Escape");
  await root.getByText("Send Finance the reserved-capacity numbers").waitFor();
  // The library composer captures the note's moment when it opens.
  await root.getByRole("button", { name: "Note", exact: true }).click();
  await root.getByRole("textbox", { name: "Note at 00:00:00" }).press("Escape");
  const colours = await root
    .locator(".speaker-chip")
    .evaluateAll((chips) => [
      ...new Set(chips.map((c) => getComputedStyle(c).color)),
    ]);
  if (colours.length !== 3)
    fail(`expected 3 speaker colours, got ${colours.length}`);
  // Title editing commits on Enter.
  await title.fill("Renamed in smoke test");
  await title.press("Enter");
  if ((await title.inputValue()) !== "Renamed in smoke test")
    fail("title edit was reverted");
  console.log("smoke checks passed");
}

async function main() {
  await mkdir(OUTPUT_DIR, { recursive: true });
  const only = process.argv[2];
  const browser = await chromium.launch();
  const page = await browser.newPage({
    viewport: { width: 1200, height: 800 },
    deviceScaleFactor: 2,
  });
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
  await page.goto(`${baseUrl}/screenshots.html`, { waitUntil: "networkidle" });
  await page.waitForTimeout(300);

  for (const id of SCREENS) {
    if (only && only !== id) continue;
    const derived = DERIVED[id];
    const rootId = derived ? derived[0] : id;
    const selector = `[id="${rootId}"]`;
    await page.locator(selector).scrollIntoViewIfNeeded();
    if (derived) {
      await page
        .locator(selector)
        .getByRole("button", { name: derived[1], exact: true })
        .click();
      await page.locator(derived[2]).waitFor({ state: "visible" });
      await page.waitForTimeout(150);
    }
    const bad = await overflowing(page, selector);
    if (bad.length)
      fail(
        `${id} has horizontally overflowing regions: ${JSON.stringify(bad)}`,
      );
    const out = path.join(OUTPUT_DIR, `${id}.png`);
    await page.locator(selector).screenshot({ path: out });
    console.log(`Saved ${out}`);
    if (derived) {
      await page.keyboard.press("Escape");
      await page.locator(derived[2]).waitFor({ state: "detached" });
    }
  }

  if (!only) await smoke(page);
  await browser.close();
  const relevant = errors.filter(
    (e) => !/Failed to load resource|ERR_|mixed\.wav|mic\.wav/.test(e),
  );
  if (relevant.length) fail(`console errors:\n${relevant.join("\n")}`);
  console.log(`\nDone — screenshots in ${OUTPUT_DIR}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
