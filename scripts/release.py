#!/usr/bin/env python3
"""
release.py — bump versions, commit, tag, and push to fire the Release workflow.

Usage:
    scripts/release.py [patch|minor|major]   # default: patch

What it does:
    1. Reads the current version from Cargo.toml and bumps the requested component.
    2. Verifies Cargo.toml and editors/vscode/package.json agree on the current version.
    3. Verifies the working tree is clean and we're on `main`, in sync with origin.
    4. Checks the new tag doesn't already exist locally or on origin.
    5. Rewrites the version in Cargo.toml and editors/vscode/package.json.
    6. Refreshes Cargo.lock so the bump is reflected there too.
    7. Shows the diff and asks for confirmation.
    8. Commits "Release vX.Y.Z", pushes main, then creates and pushes the tag.

Pushing the tag fires .github/workflows/release.yml, which builds binaries
for five targets, packages five .vsix files, and uploads all assets to a
GitHub release at the tag.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Literal, NoReturn


SEMVER_RE: re.Pattern[str] = re.compile(r"^([0-9]+)\.([0-9]+)\.([0-9]+)$")

BumpKind = Literal["patch", "minor", "major"]


def bump_version(current: str, kind: BumpKind) -> str:
    match = SEMVER_RE.match(current)
    if match is None:
        die(f"current version '{current}' is not semver x.y.z")
    major, minor, patch = (int(g) for g in match.groups())
    if kind == "major":
        return f"{major + 1}.0.0"
    if kind == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def die(msg: str) -> NoReturn:
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def run(
    cmd: list[str],
    *,
    capture: bool = False,
    check: bool = True,
    cwd: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        check=check,
        cwd=cwd,
        text=True,
        capture_output=capture,
    )


def git(*args: str, capture: bool = False, check: bool = True) -> str:
    result = run(["git", *args], capture=capture, check=check)
    return (result.stdout or "").strip() if capture else ""


def confirm(prompt: str) -> bool:
    reply = input(f"{prompt} [y/N] ").strip().lower()
    return reply == "y"


def repo_root() -> Path:
    return Path(git("rev-parse", "--show-toplevel", capture=True))


def read_cargo_version(path: Path) -> str:
    """Read the `version = "..."` field from the [package] section of Cargo.toml."""
    text = path.read_text()
    # Find the [package] section (everything until the next [section] or EOF).
    section_match = re.search(r"\[package\][^\[]*", text)
    if section_match is None:
        die(f"could not find [package] section in {path}")
    version_match = re.search(
        r'^version\s*=\s*"([^"]+)"',
        section_match.group(0),
        flags=re.MULTILINE,
    )
    if version_match is None:
        die(f"could not find version in [package] section of {path}")
    return version_match.group(1)


def write_cargo_version(path: Path, new_version: str) -> None:
    """Replace the version field within the [package] section only."""
    text = path.read_text()

    def replace_in_section(section: re.Match[str]) -> str:
        return re.sub(
            r'^(version\s*=\s*")[^"]*(")',
            lambda m: f"{m.group(1)}{new_version}{m.group(2)}",
            section.group(0),
            count=1,
            flags=re.MULTILINE,
        )

    new_text, n = re.subn(r"\[package\][^\[]*", replace_in_section, text, count=1)
    if n != 1:
        die(f"failed to update [package] version in {path}")
    path.write_text(new_text)


def read_vscode_version(path: Path) -> str:
    data = json.loads(path.read_text())
    version = data.get("version")
    if not isinstance(version, str):
        die(f"missing or non-string 'version' in {path}")
    return version


def write_vscode_version(path: Path, new_version: str) -> None:
    """Rewrite package.json preserving 2-space indent and trailing newline."""
    data = json.loads(path.read_text())
    data["version"] = new_version
    path.write_text(json.dumps(data, indent=2) + "\n")


def preflight(root: Path, tag: str) -> None:
    branch = git("rev-parse", "--abbrev-ref", "HEAD", capture=True)
    if branch != "main":
        die(f"must be on main (currently on '{branch}')")

    status = git("status", "--porcelain", capture=True)
    if status:
        die("working tree is not clean — commit or stash changes first")

    print("Fetching origin...")
    git("fetch", "origin", "main", "--tags")

    local_sha = git("rev-parse", "HEAD", capture=True)
    remote_sha = git("rev-parse", "origin/main", capture=True)
    if local_sha != remote_sha:
        die(
            "local main is not in sync with origin/main "
            f"(local={local_sha} remote={remote_sha})"
        )

    if (
        run(
            ["git", "rev-parse", "-q", "--verify", f"refs/tags/{tag}"],
            capture=True,
            check=False,
        ).returncode
        == 0
    ):
        die(f"tag {tag} already exists locally")

    if (
        run(
            ["git", "ls-remote", "--exit-code", "--tags", "origin", f"refs/tags/{tag}"],
            capture=True,
            check=False,
        ).returncode
        == 0
    ):
        die(f"tag {tag} already exists on origin")


def main(argv: list[str]) -> int:
    if len(argv) > 2:
        die(f"usage: {argv[0]} [patch|minor|major]")

    kind_arg: str = argv[1] if len(argv) == 2 else "patch"
    if kind_arg not in ("patch", "minor", "major"):
        die(f"unknown bump kind '{kind_arg}' — expected patch|minor|major")
    kind: BumpKind = kind_arg  # type: ignore[assignment]

    root: Path = repo_root()
    cargo_toml: Path = root / "Cargo.toml"
    vscode_pkg: Path = root / "editors" / "vscode" / "package.json"
    cargo_lock: Path = root / "Cargo.lock"

    for required in (cargo_toml, vscode_pkg):
        if not required.is_file():
            die(f"missing {required.relative_to(root)}")

    current_cargo: str = read_cargo_version(cargo_toml)
    current_vscode: str = read_vscode_version(vscode_pkg)

    if current_cargo != current_vscode:
        die(
            "Cargo.toml and editors/vscode/package.json disagree on current version "
            f"({current_cargo} vs {current_vscode}) — fix manually before releasing"
        )

    version: str = bump_version(current_cargo, kind)
    tag: str = f"v{version}"

    preflight(root, tag)

    print("Current versions:")
    print(f"  Cargo.toml                  : {current_cargo}")
    print(f"  editors/vscode/package.json : {current_vscode}")
    print(f"Bump kind                     : {kind}")
    print(f"New version                   : {version}")
    print(f"Tag                           : {tag}")
    print()

    if not confirm("Bump versions, commit, tag, and push?"):
        die("aborted by user")

    write_cargo_version(cargo_toml, version)
    write_vscode_version(vscode_pkg, version)

    print("Updating Cargo.lock (cargo update -p quicklsp)...")
    run(["cargo", "update", "-p", "quicklsp"], cwd=root)

    print()
    print("Diff to be committed:")
    print("---------------------")
    run(
        [
            "git",
            "--no-pager",
            "diff",
            "--",
            str(cargo_toml.relative_to(root)),
            str(vscode_pkg.relative_to(root)),
            str(cargo_lock.relative_to(root)),
        ],
        cwd=root,
    )
    print("---------------------")
    print()

    if not confirm(f"Commit and push as 'Release {tag}'?"):
        print("aborted — reverting working-tree changes", file=sys.stderr)
        run(
            [
                "git",
                "checkout",
                "--",
                str(cargo_toml.relative_to(root)),
                str(vscode_pkg.relative_to(root)),
                str(cargo_lock.relative_to(root)),
            ],
            cwd=root,
        )
        return 1

    git(
        "add",
        str(cargo_toml.relative_to(root)),
        str(vscode_pkg.relative_to(root)),
        str(cargo_lock.relative_to(root)),
    )
    git("commit", "-m", f"Release {tag}")
    git("push", "origin", "main")

    git("tag", tag)
    git("push", "origin", tag)

    print()
    print(f"Pushed {tag}. The Release workflow should now be running:")
    print("  https://github.com/sinelaw/quicklsp/actions/workflows/release.yml")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
