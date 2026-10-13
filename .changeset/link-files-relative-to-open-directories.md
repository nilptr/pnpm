---
"pacquet": patch
---

Sped up linking package files into `node_modules` on macOS and Linux. pnpm now opens each target directory once and creates the files in it relative to that directory.
