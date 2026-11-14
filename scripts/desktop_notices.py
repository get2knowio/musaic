"""Write the desktop bundle's NOTICES.txt (third-party licence notices).

    python3 scripts/desktop_notices.py [--target aarch64-apple-darwin] [--out FILE]

Default output: rust/target/desktop-notices/NOTICES.txt (gitignored), which
scripts/package_macos.sh ships as Musaic.app/Contents/Resources/NOTICES.txt
through rust/crates/musaic-desktop/tauri.bundle.conf.json.

Sections, in order:

1. Chromaprint / rusty-chromaprint (notices/chromaprint.txt + the LGPL-2.1
   text in notices/LGPL-2.1.txt). Whether a port of Chromaprint carries
   LGPL obligations is an open question; the notice is included without
   prejudice to it.
2. SQLite (bundled into the binary by libsqlite3-sys; public domain).
3. Rust crates linked into the binary for --target: `cargo about generate
   --frozen --fail --format json` with notices/about.toml (normal
   dependencies of musaic-desktop only, workspace crates skipped). Each
   distinct licence text once, with the crates it covers. `--frozen`: no
   network; run `cargo fetch --locked` (crates.io) first on a fresh
   machine. A licence not in about.toml's `accepted` list fails the run.
4. npm packages bundled into the SPA: every non-dev package in
   frontend/package-lock.json, licence text read from frontend/node_modules
   (run `npm ci` first). A package without a licence file gets its
   package.json licence id and author.
5. IBM Plex Sans / Mono (frontend/public/fonts/*/OFL.txt).

Needs cargo-about on PATH or in ~/.cargo/bin (`cargo install cargo-about
--locked --features cli`). Standard library only.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATE = ROOT / "rust" / "crates" / "musaic-desktop"
NOTICES_DIR = CRATE / "notices"
FRONTEND = ROOT / "frontend"
DEFAULT_OUT = ROOT / "rust" / "target" / "desktop-notices" / "NOTICES.txt"
RULE = "=" * 78


def cargo_about() -> str:
    found = shutil.which("cargo-about") or str(Path.home() / ".cargo" / "bin" / "cargo-about")
    if not Path(found).is_file():
        sys.exit("cargo-about not found: cargo install cargo-about --locked --features cli")
    return found


def rust_section(target: str) -> tuple[list[str], int]:
    out = subprocess.run(
        [cargo_about(), "about", "generate", "--frozen", "--fail", "--format", "json",
         "--target", target,
         "-m", str(CRATE / "Cargo.toml"), "-c", str(NOTICES_DIR / "about.toml")],
        cwd=ROOT / "rust", capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit(f"cargo about failed ({out.returncode}):\n{out.stderr[-4000:]}")
    data = json.loads(out.stdout)
    crates = sorted({(c["package"]["name"], c["package"]["version"], c["license"])
                     for c in data["crates"]})
    # One block per distinct licence text, listing every crate it covers.
    by_text: dict[str, dict] = {}
    for lic in data["licenses"]:
        text = lic["text"].strip()
        key = hashlib.sha256(text.encode()).hexdigest()
        b = by_text.setdefault(key, {"id": lic["id"], "name": lic["name"], "text": text, "used": set()})
        for u in lic["used_by"]:
            b["used"].add(f'{u["crate"]["name"]} {u["crate"]["version"]}')
    lines = [RULE, f"Rust crates linked into the Musaic binary ({target})", RULE, "",
             f"{len(crates)} crates. Licence expression per crate (as declared):", ""]
    lines += [f"  {n} {v}: {lic}" for n, v, lic in crates]
    lines.append("")
    for b in sorted(by_text.values(), key=lambda b: (b["id"], sorted(b["used"]))):
        lines += ["-" * 78, f'{b["name"]} ({b["id"]})', "Used by: " + ", ".join(sorted(b["used"])), "",
                  b["text"], ""]
    return lines, len(crates)


LICENCE_NAMES = ("license", "licence", "copying")


def npm_section() -> tuple[list[str], int]:
    lock = json.loads((FRONTEND / "package-lock.json").read_text())
    pkgs = []
    for key, meta in lock["packages"].items():
        if not key or meta.get("dev"):
            continue
        d = FRONTEND / key
        if not d.is_dir():
            sys.exit(f"{d} missing: (cd frontend && npm ci)")
        pj = json.loads((d / "package.json").read_text())
        files = sorted(f for f in os.listdir(d) if f.lower().startswith(LICENCE_NAMES) and (d / f).is_file())
        if files:
            text = "\n\n".join((d / f).read_text(errors="replace").strip() for f in files)
        else:
            author = pj.get("author")
            if isinstance(author, dict):
                author = author.get("name")
            text = (f"(no licence file in the published package) License: {pj.get('license')}; "
                    f"author: {author or 'not stated'}; {pj.get('repository', {}).get('url', '') if isinstance(pj.get('repository'), dict) else pj.get('repository', '')}")
        pkgs.append((pj["name"], pj["version"], pj.get("license", meta.get("license")), text))
    pkgs.sort()
    lines = [RULE, "JavaScript packages bundled into the Musaic user interface", RULE, "",
             f"{len(pkgs)} packages (non-dev entries of frontend/package-lock.json).", ""]
    for name, ver, lic, text in pkgs:
        lines += ["-" * 78, f"{name} {ver} ({lic})", "", text, ""]
    return lines, len(pkgs)


def fonts_section() -> list[str]:
    lines = [RULE, "Fonts", RULE, ""]
    for d in sorted((FRONTEND / "public" / "fonts").iterdir()):
        ofl = d / "OFL.txt"
        if ofl.is_file():
            lines += ["-" * 78, f"{d.name} (SIL Open Font License 1.1)", "", ofl.read_text().strip(), ""]
    return lines


SQLITE = """SQLite (bundled into the binary through libsqlite3-sys)
------------------------------------------------------
The SQLite source code is in the public domain
(https://sqlite.org/copyright.html). The libsqlite3-sys / rusqlite
bindings are listed under the Rust crates below.
"""


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="python3 scripts/desktop_notices.py", description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--target", default="aarch64-apple-darwin")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = ap.parse_args(argv)

    rust, n_crates = rust_section(args.target)
    npm, n_npm = npm_section()
    head = [
        "Musaic — third-party notices",
        "",
        "Musaic includes the third-party software listed below. Each component is",
        "used under the licence shown with it.",
        "",
        RULE,
        "Native components",
        RULE,
        "",
        (NOTICES_DIR / "chromaprint.txt").read_text().rstrip(),
        "",
        (NOTICES_DIR / "LGPL-2.1.txt").read_text().rstrip(),
        "",
        SQLITE,
    ]
    text = "\n".join(head + rust + npm + fonts_section()) + "\n"
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(text, encoding="utf-8")
    print(f"{args.out}: {len(text.encode())} bytes; {n_crates} crates, {n_npm} npm packages, "
          f"sha256 {hashlib.sha256(text.encode()).hexdigest()[:16]}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
