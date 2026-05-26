import { chmodSync, existsSync, lstatSync, readdirSync, realpathSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binDir = join(root, "node_modules", ".bin");
const fixed = new Set();

function markExecutable(path) {
  if (fixed.has(path)) return;
  fixed.add(path);
  try {
    const stat = lstatSync(path);
    if (!stat.isFile()) return;
    const executableBits = 0o111;
    if ((stat.mode & executableBits) === executableBits) return;
    chmodSync(path, stat.mode | 0o755);
  } catch (err) {
    if (err?.code !== "ENOENT") {
      console.warn(`warning: unable to mark ${path} executable: ${err.message}`);
    }
  }
}

if (existsSync(binDir)) {
  for (const entry of readdirSync(binDir)) {
    const binPath = join(binDir, entry);
    try {
      const stat = lstatSync(binPath);
      markExecutable(stat.isSymbolicLink() ? realpathSync(binPath) : binPath);
    } catch (err) {
      if (err?.code !== "ENOENT") {
        console.warn(`warning: unable to inspect ${binPath}: ${err.message}`);
      }
    }
  }
}
