---
name: repo-maintenance
description: "Maintain the semmem repository and verify its supported development workflows."
---

## 1. Scope

Use when maintaining this repository or its independently configured projects.

## 2. Procedure

- Run `task test` for the Rust integration suite, including fault injection, and `task build` for the release executable. Keep dependency resolution locked.
- `plugin/skills/semmem/` is a distributable plugin asset, not a project-maintenance skill. Preserve the plugin-relative path used by plugin skill discovery; repository compatibility rules permit this shipped skill location. Validate plugin JSON and Cargo packaging after changing it.
- Preserve `database/initialize.sql`, existing schema checks, benchmark-result files, socket and HTTP contracts, and audit behavior.
- Building and testing this repository does not install or reactivate the retired service. `install.sh` changes the user service and is outside ordinary verification.
