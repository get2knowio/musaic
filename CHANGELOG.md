# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project aims to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!--
Add entries under Unreleased as you go. On release, rename the section to the
version + date and start a fresh Unreleased. Categories: Added, Changed,
Deprecated, Removed, Fixed, Security.
-->

## [Unreleased]

### Added

- Continuous-integration workflow (`ci.yml`) running backend (ruff + pytest),
  frontend (eslint + typecheck + vitest + build), and desktop (typecheck) on
  every push and pull request.
- Repository governance files: `SECURITY.md`, `CODEOWNERS`, issue / pull-request
  templates, and this changelog.

### Security

- Resolved outstanding Dependabot alerts across the Python, frontend, and
  desktop dependency trees (Pillow, mcp, shell-quote, and the electron-builder
  toolchain).

[Unreleased]: https://github.com/get2knowio/musaic/commits/main
