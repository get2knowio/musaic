# Security Policy

## Reporting a vulnerability

Please **do not** open a public issue for security problems.

Report vulnerabilities privately through GitHub's
[private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
on this repository's **Security** tab (**Report a vulnerability**), or by email
to the maintainer listed in [`.github/CODEOWNERS`](.github/CODEOWNERS).

Please include:

- a description of the issue and its impact,
- steps to reproduce (a minimal proof-of-concept if possible),
- affected version / commit, and OS.

We aim to acknowledge reports within a few days and will keep you updated as we
investigate and prepare a fix.

## Scope

Musaic is a **local-first** application: it runs on the user's own machine
against their own music library, and stores its cache under `~/.musaic`. The
most relevant areas for security review are:

- the FastAPI backend on `127.0.0.1:8765` and its request handling,
- the FLAC/metadata mutation pipeline (untrusted tag/cover input from files),
- image handling (cover art is processed with Pillow),
- outbound calls to MusicBrainz / Cover Art Archive / AcoustID, and
- the AI integration (provider API keys, hosted tool calls).

## Supported versions

This project is pre-1.0 and under active development. Security fixes are applied
to the latest `main`; there is no long-term support branch yet.
