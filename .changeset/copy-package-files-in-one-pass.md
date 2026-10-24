---
"pacquet": patch
---

Sped up installs that copy package files into `node_modules`, such as packages with install scripts. A copied file no longer has its permissions set a second time after it is created, and its bytes are copied in one pass.
