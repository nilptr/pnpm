---
"pacquet": patch
---

Sped up installs from a warm store. The store integrity check of a package's files now runs as the files are linked into `node_modules`, so the install no longer reads every file's metadata in a separate pass first.
