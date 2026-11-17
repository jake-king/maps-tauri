// Builds crates/coords to WebAssembly with wasm-pack, on any platform: npm runs package scripts with
// sh on Linux and macOS but cmd.exe on Windows, so the PATH set-up cannot live in package.json.
// ~/.cargo/bin goes on PATH because Tauri's dev loop starts npm without a login shell.
import { spawnSync } from "node:child_process";
import { homedir } from "node:os";
import { delimiter, join } from "node:path";

const env = { ...process.env, PATH: [join(homedir(), ".cargo", "bin"), process.env.PATH ?? process.env.Path].join(delimiter) };
const args = ["build", "crates/coords", "--release", "--target", "web", "--out-dir", "../../src/wasm", "--no-pack", "--", "--features", "wasm"];
const r = spawnSync("wasm-pack", args, { env, stdio: "inherit", shell: process.platform === "win32" });
if (r.error) {
  console.error(`wasm-pack: ${r.error.message}`);
  process.exit(1);
}
process.exit(r.status ?? 1);
