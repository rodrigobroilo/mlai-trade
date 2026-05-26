import { rmSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(new URL("..", import.meta.url).pathname);

for (const entry of [".vite-build", "assets", "index.html", "robots.txt"]) {
  rmSync(resolve(root, entry), { force: true, recursive: true });
}
