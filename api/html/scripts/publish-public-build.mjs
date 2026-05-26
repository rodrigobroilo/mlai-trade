import { cpSync, existsSync, readdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(new URL("..", import.meta.url).pathname);
const buildDir = resolve(root, ".vite-build");

if (!existsSync(buildDir)) {
  throw new Error(`missing Vite build directory: ${buildDir}`);
}

for (const entry of readdirSync(buildDir)) {
  cpSync(resolve(buildDir, entry), resolve(root, entry), { recursive: true });
}

rmSync(buildDir, { force: true, recursive: true });
