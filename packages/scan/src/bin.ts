#!/usr/bin/env node
/**
 * The executable: `main` with the process's own arguments, streams and environment.
 */
import { main } from "./cli.js";

main(process.argv.slice(2), {
  stdout: (text) => process.stdout.write(text),
  stderr: (text) => process.stderr.write(text),
  env: process.env,
  cwd: process.cwd(),
  isTTY: process.stdout.isTTY === true,
}).then((code) => {
  process.exitCode = code;
});
