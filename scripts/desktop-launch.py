#!/usr/bin/env python3
"""Fetch the source-matched, Fedora-tested desktop bundle. Never builds or installs packages."""
import hashlib
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.request

REPOSITORY = "c4us4l1ty/holonomy"
ASSET = "holonomy-linux-x86_64.tar.gz"
BINARIES = {"holonomy-gui", "holonomy-native"}
MAX_DOWNLOAD = 512 * 1024 * 1024


def download(url, destination, limit=MAX_DOWNLOAD):
    request = urllib.request.Request(url, headers={"User-Agent": "Holonomy-desktop-bootstrap"})
    with urllib.request.urlopen(request, timeout=60) as response, destination.open("wb") as output:
        total = 0
        while True:
            chunk = response.read(1024 * 1024)
            if not chunk:
                break
            total += len(chunk)
            if total > limit:
                raise ValueError("desktop download exceeds size limit")
            output.write(chunk)


def unpack_verified(archive, checksum, destination):
    """Validate before extracting only two regular executable files, never links or paths."""
    fields = checksum.read_text().split()
    if not fields:
        raise ValueError("empty desktop checksum")
    expected = fields[0]
    if not re.fullmatch(r"[0-9a-fA-F]{64}", expected):
        raise ValueError("invalid desktop checksum")
    digest = hashlib.sha256()
    with archive.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    if digest.hexdigest() != expected.lower():
        raise ValueError("desktop checksum mismatch; refusing to install")
    with tarfile.open(archive, "r:gz") as bundle:
        members = bundle.getmembers()
        if len(members) != 2 or {member.name for member in members} != BINARIES:
            raise ValueError("unexpected desktop bundle contents")
        if any(not member.isfile() or member.size > MAX_DOWNLOAD for member in members):
            raise ValueError("desktop bundle must contain only bounded regular files")
        for member in members:
            with bundle.extractfile(member) as source, (destination / member.name).open("wb") as output:
                while True:
                    chunk = source.read(1024 * 1024)
                    if not chunk:
                        break
                    output.write(chunk)
            (destination / member.name).chmod(0o755)


def install(root, revision):
    cache = root / "target" / "desktop-prebuilt" / revision
    if not all((cache / name).is_file() for name in BINARIES):
        cache.parent.mkdir(parents=True, exist_ok=True)
        # flock serializes concurrent first launches and is automatically released on failure.
        import fcntl
        with (cache.parent / ".install.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if not all((cache / name).is_file() for name in BINARIES):
                if cache.exists():
                    raise ValueError(f"incomplete desktop cache: remove {cache} and retry")
                base = f"https://github.com/{REPOSITORY}/releases/download/desktop-{revision}"
                print("Downloading the source-matched, Fedora-tested GPUI desktop (no compiler needed)…", flush=True)
                with tempfile.TemporaryDirectory(prefix=".download-", dir=cache.parent) as temp:
                    staging = Path(temp)
                    archive, checksum = staging / ASSET, staging / (ASSET + ".sha256")
                    download(base + "/" + ASSET + ".sha256", checksum, 4096)
                    download(base + "/" + ASSET, archive)
                    unpack_verified(archive, checksum, staging)
                    archive.unlink()
                    checksum.unlink()
                    staging.rename(cache)
                    # TemporaryDirectory will tolerate the renamed, now-missing staging path.
    return cache


def main():
    if platform.system() != "Linux" or platform.machine() not in ("x86_64", "amd64"):
        raise ValueError("prebuilt launcher currently supports Linux x86_64 only; see README for source builds")
    root = Path(__file__).resolve().parent.parent
    # First-parent history identifies the main merge that last changed desktop inputs.
    # Documentation-only pulls can reuse the same tested binary instead of requiring a rebuild.
    revision = subprocess.check_output([
        "git", "-C", str(root), "log", "--first-parent", "-1", "--format=%H", "HEAD", "--",
        "holonomy", "Cargo.toml", "Cargo.lock", "crates/holonomy-native", "crates/holonomy-gui", "crates/holonomy-eye",
        "crates/holonomy-shell", "crates/holonomy-text", "crates/holonomy-line",
        "crates/holonomy-seal", "vendor/xattr",
        ".github/workflows/native-ui.yml", ".github/workflows/release.yml",
        "scripts/desktop-launch.py", "scripts/test-desktop-launch.py",
        "scripts/qualify-native-ui.sh", "scripts/qualify-fedora.sh",
        "scripts/run-desktop.sh", "scripts/test-run-desktop.py",
    ], text=True).strip()
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("cannot identify checkout revision")
    if not os.environ.get("DISPLAY") and not os.environ.get("WAYLAND_DISPLAY"):
        raise ValueError("start Holonomy from a graphical desktop session (no DISPLAY or WAYLAND_DISPLAY)")
    cache = install(root, revision)
    # Preserve source-built files: make the familiar command available only in empty slots.
    release = root / "target" / "release"
    release.mkdir(parents=True, exist_ok=True)
    for name in BINARIES:
        target = release / name
        if not os.path.lexists(target) or target.is_symlink():
            # Only replace links previously created by this bootstrap, not arbitrary user links.
            if target.is_symlink() and target.resolve().parent.parent != cache.parent:
                continue
            temporary = release / ("." + name + "." + str(os.getpid()))
            temporary.symlink_to(cache / name)
            os.replace(temporary, target)
    os.execv(str(cache / "holonomy-gui"), [str(cache / "holonomy-gui"), *sys.argv[1:]])


if __name__ == "__main__":
    try:
        main()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            print("No Fedora-qualified desktop bundle is published for this commit yet. Check the Native UI workflow on GitHub, wait for it to pass, then retry ./holonomy. Nothing was compiled or installed.", file=sys.stderr)
        else:
            print(f"Desktop download failed: HTTP {error.code}. Retry ./holonomy.", file=sys.stderr)
        sys.exit(1)
    except (OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError) as error:
        print(f"Holonomy could not start: {error}", file=sys.stderr)
        print("On Fedora, missing runtime libraries/graphics drivers can be installed with: sudo dnf install libxkbcommon-x11 vulkan-loader mesa-vulkan-drivers fontconfig alsa-lib openssl-libs libX11-xcb libXcursor libXi libXrandr libXinerama libXrender", file=sys.stderr)
        sys.exit(1)
