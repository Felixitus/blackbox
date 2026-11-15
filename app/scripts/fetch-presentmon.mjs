// Downloads the pinned PresentMon release that the installer ships with.
//
// Runs before every app build (see tauri.conf.json). The file is fetched
// only when missing or when its checksum does not match, and the build fails
// if the download does not match the pinned SHA-256: a different binary must
// never end up in the installer.
//
// Updating PresentMon: change VERSION and SHA256 (from the GitHub release
// page, field "digest") and update licenses/PresentMon-LICENSE.txt.
import { createHash } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const VERSION = "2.6.0";
const SHA256 = "b2a706bc6ad475749e3b7e3409263aa1e6906d45bdcf993f6dbc0f660188f1af";
const URL = `https://github.com/GameTechDev/PresentMon/releases/download/v${VERSION}/PresentMon-${VERSION}-x64.exe`;

const target = join(dirname(fileURLToPath(import.meta.url)), "..", "src-tauri", "vendor", "PresentMon.exe");
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

async function existingIsValid() {
  try {
    return sha256(await readFile(target)) === SHA256;
  } catch {
    return false;
  }
}

if (await existingIsValid()) {
  console.log(`PresentMon ${VERSION}: present, checksum matches.`);
} else {
  console.log(`PresentMon ${VERSION}: downloading ${URL}`);
  const res = await fetch(URL, { redirect: "follow" });
  if (!res.ok) {
    console.error(`Download failed: HTTP ${res.status}`);
    process.exit(1);
  }
  const bytes = Buffer.from(await res.arrayBuffer());
  const actual = sha256(bytes);
  if (actual !== SHA256) {
    console.error(`Checksum mismatch!\n  expected ${SHA256}\n  got      ${actual}\nThe file is not used.`);
    process.exit(1);
  }
  await mkdir(dirname(target), { recursive: true });
  await writeFile(`${target}.part`, bytes);
  await rename(`${target}.part`, target);
  console.log(`PresentMon ${VERSION}: downloaded and verified (${(bytes.length / 1024).toFixed(0)} KB).`);
}
