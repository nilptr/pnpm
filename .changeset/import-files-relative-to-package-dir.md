---
"pacquet": patch
---

Sped up linking package files into `node_modules` on macOS and Linux. pnpm now resolves each file relative to its package directory and the store instead of by its full path, and no longer checks a store file's mode before hardlinking it. On macOS, cloning 40k files got about 24% faster.
