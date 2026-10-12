"""Stage archive and npm inputs from maturin's canonical compiled wheel binary."""
import argparse
import hashlib
import json
import tarfile
import tomllib
import zipfile
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--target", required=True)
parser.add_argument("--directory", type=Path, required=True)
parser.add_argument("--verify-npm", action="store_true")
args = parser.parse_args()
if args.verify_npm:
    inventory = json.loads((args.directory / "binary.json").read_text())
    package, = args.directory.glob("*.tgz")
    with tarfile.open(package) as archive:
        carried = archive.extractfile("package/bin/onemessagebus").read()
    digest = hashlib.sha256(carried).hexdigest()
    if digest != inventory["sha256"]:
        parser.error("npm payload differs from the compiled wheel binary; rebuild the target")
    inventory["hashes"]["npm"] = digest
    (args.directory / "binary.json").write_text(json.dumps(inventory) + "\n")
    raise SystemExit(0)
version = tomllib.loads(Path("Cargo.toml").read_text())["workspace"]["package"]["version"]
wheel, = args.directory.glob("*.whl")
with zipfile.ZipFile(wheel) as archive:
    binaries = [name for name in archive.namelist() if ".data/scripts/onemessagebus" in name]
    binary_name, = binaries
    binary = archive.read(binary_name)
output = args.directory / "onemessagebus"
output.write_bytes(binary)
output.chmod(0o755)
name = f"onemessagebus-v{version}-{args.target}.tar.gz"
with tarfile.open(args.directory / name, "w:gz") as archive:
    archive.add(output, arcname="onemessagebus")
    for extra in ("README.md", "LICENSE", "CHANGELOG.md"):
        archive.add(extra, arcname=extra)
digest = hashlib.sha256((args.directory / name).read_bytes()).hexdigest()
(args.directory / f"{name}.sha256").write_text(f"{digest}  {name}\n")
(args.directory / "binary.json").write_text(json.dumps({"target": args.target, "version": version,
    "sha256": hashlib.sha256(binary).hexdigest(), "wheel": wheel.name, "archive": name,
    "hashes": {"wheel": hashlib.sha256(binary).hexdigest(), "archive": hashlib.sha256(tarfile.open(args.directory / name).extractfile("onemessagebus").read()).hexdigest()}}) + "\n")
