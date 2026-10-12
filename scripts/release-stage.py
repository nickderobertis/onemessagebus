"""Stage archive and npm inputs from maturin's canonical compiled wheel binary."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import tarfile
import tomllib
import zipfile
from dataclasses import asdict, dataclass, replace
from email.parser import BytesParser
from pathlib import Path
from typing import NewType

try:
    from packaging.version import Version
except ModuleNotFoundError:
    from pip._vendor.packaging.version import Version

SHA256 = NewType("SHA256", str)


@dataclass(frozen=True)
class CarrierHashes:
    wheel: SHA256
    archive: SHA256
    npm: SHA256 | None = None


@dataclass(frozen=True)
class BinaryInventory:
    target: str
    version: str
    sha256: SHA256
    wheel: str
    archive: str
    hashes: CarrierHashes

    @classmethod
    def read(cls, path: Path) -> BinaryInventory:
        value: object = json.loads(path.read_text())
        if not isinstance(value, dict) or not isinstance(value.get("hashes"), dict):
            raise ValueError("binary inventory is not a record with carrier hashes")
        hashes = CarrierHashes(**value.pop("hashes"))
        result = cls(**value, hashes=hashes)
        for text in (result.target, result.version, result.wheel, result.archive):
            if not isinstance(text, str) or not text or "/" in text or "\\" in text:
                raise ValueError("inventory identity must contain safe nonempty strings")
        for digest in (result.sha256, hashes.wheel, hashes.archive):
            if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
                raise ValueError("inventory contains an invalid SHA-256")
        if hashes.npm is not None and (not isinstance(hashes.npm, str) or not re.fullmatch(r"[0-9a-f]{64}", hashes.npm)):
            raise ValueError("inventory contains an invalid npm SHA-256")
        return result

    def write(self, path: Path) -> None:
        value = asdict(self)
        if self.hashes.npm is None:
            del value["hashes"]["npm"]
        path.write_text(json.dumps(value) + "\n")


def refuse(message: str, status: int = 2) -> None:
    print(f"release-stage: {message}\nACTION: rebuild the requested target with scripts/release-build.sh into an empty staging directory", file=sys.stderr)
    raise SystemExit(status)


def stage(args: argparse.Namespace) -> None:
    declared = tomllib.loads(Path("rust-toolchain.toml").read_text())["toolchain"]["targets"]
    if not re.fullmatch(r"(?:x86_64|aarch64)-(?:apple-darwin|unknown-linux-gnu)", args.target) or args.target not in declared or not args.target.endswith(("apple-darwin", "unknown-linux-gnu")):
        refuse(f"unsupported target {args.target!r}; choose a declared Darwin or GNU Linux target")
    inventory_path = args.directory / "binary.json"
    if args.verify_npm:
        inventory = BinaryInventory.read(inventory_path)
        if inventory.target != args.target:
            refuse("inventory target differs from the requested target")
        package, = args.directory.glob("*.tgz")
        with tarfile.open(package) as archive:
            member = archive.extractfile("package/bin/onemessagebus")
            if member is None:
                refuse("npm package has no canonical executable")
            digest = SHA256(hashlib.sha256(member.read()).hexdigest())
        if digest != inventory.sha256:
            refuse("npm payload differs from the compiled wheel binary")
        replace(inventory, hashes=replace(inventory.hashes, npm=digest)).write(inventory_path)
        return
    version = tomllib.loads(Path("Cargo.toml").read_text())["workspace"]["package"]["version"]
    if not isinstance(version, str) or not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?", version):
        refuse("workspace version is not a release version")
    wheel, = args.directory.glob("*.whl")
    parts = wheel.name.split("-")
    if len(parts) != 5 or parts[0] != "onemessagebus_cli" or parts[2:4] != ["py3", "none"]:
        refuse("wheel filename is not an onemessagebus CLI wheel")
    arch = "arm64" if args.target == "aarch64-apple-darwin" else args.target.split("-", 1)[0]
    platform = parts[4].removesuffix(".whl")
    if Version(parts[1]) != Version(version):
        refuse("wheel version differs from the workspace release version")
    if args.target.endswith("unknown-linux-gnu") and any(not re.match(r"(?:manylinux[0-9]*|linux)_", tag) for tag in platform.split(".")):
        refuse("wheel platform family differs from the requested GNU Linux target")
    if not platform.endswith(f"_{arch}") or (args.target.endswith("apple-darwin") != platform.startswith("macosx_")):
        refuse("wheel platform/architecture differs from the requested target")
    with zipfile.ZipFile(wheel) as archive:
        binary_name = f"onemessagebus_cli-{parts[1]}.data/scripts/onemessagebus"
        if archive.namelist().count(binary_name) != 1:
            refuse("wheel has no unique exact onemessagebus script member")
        info = f"onemessagebus_cli-{parts[1]}.dist-info"
        metadata = BytesParser().parsebytes(archive.read(f"{info}/METADATA"))
        wheel_tags = BytesParser().parsebytes(archive.read(f"{info}/WHEEL")).get_all("Tag", [])
        if metadata["Name"] != "onemessagebus-cli" or metadata["Version"] != parts[1] or any(f"py3-none-{tag}" not in wheel_tags for tag in platform.split(".")):
            refuse("wheel metadata identity or tag differs from its filename")
        binary = archive.read(binary_name)
    output = args.directory / "onemessagebus"
    output.write_bytes(binary)
    output.chmod(0o755)
    name = f"onemessagebus-v{version}-{args.target}.tar.gz"
    with tarfile.open(args.directory / name, "w:gz") as archive:
        archive.add(output, arcname="onemessagebus")
        for extra in ("README.md", "LICENSE", "CHANGELOG.md"):
            archive.add(extra, arcname=extra)
    archive_digest = hashlib.sha256((args.directory / name).read_bytes()).hexdigest()
    (args.directory / f"{name}.sha256").write_text(f"{archive_digest}  {name}\n")
    wheel_hash = SHA256(hashlib.sha256(binary).hexdigest())
    with tarfile.open(args.directory / name) as archive:
        carried = archive.extractfile("onemessagebus")
        if carried is None:
            refuse("archive has no canonical executable")
        archive_hash = SHA256(hashlib.sha256(carried.read()).hexdigest())
    BinaryInventory(args.target, version, wheel_hash, wheel.name, name,
                    CarrierHashes(wheel_hash, archive_hash)).write(inventory_path)


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--target", required=True)
parser.add_argument("--directory", type=Path, required=True)
parser.add_argument("--verify-npm", action="store_true")
try:
    stage(parser.parse_args())
except (ValueError, TypeError, KeyError, json.JSONDecodeError, tarfile.TarError, zipfile.BadZipFile) as error:
    refuse(f"invalid staged inputs: {error}")
except OSError as error:
    refuse(f"cannot read or write staged inputs: {error}", 1)
