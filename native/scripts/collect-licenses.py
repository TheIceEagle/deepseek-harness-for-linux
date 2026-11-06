#!/usr/bin/env python3
"""Copy registry-provided notices for this binary's production Cargo graph."""

import json
from pathlib import Path
import shutil
import subprocess
import sys


def copy_notice(source, target):
    if not source.is_file() or source.stat().st_size == 0:
        raise SystemExit("An accompanying license notice is missing or empty.")
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, target)


def main():
    native = Path(__file__).resolve().parents[1]
    destination = Path(sys.argv[1])
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--manifest-path", str(native / "Cargo.toml"),
        "--format-version", "1", "--locked", "--filter-platform", "x86_64-unknown-linux-gnu",
    ], text=True))
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    app = next(package["id"] for package in packages.values() if package["name"] == "harness-native-app")
    pending = [app]
    seen = set()
    while pending:
        identity = pending.pop()
        if identity in seen:
            continue
        seen.add(identity)
        for dependency in nodes[identity]["deps"]:
            if any(kind["kind"] != "dev" for kind in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    destination.mkdir(parents=True, exist_ok=True)
    index = []
    for identity in sorted(seen, key=lambda key: (packages[key]["name"], packages[key]["version"])):
        package = packages[identity]
        source = Path(package["manifest_path"]).parent.resolve()
        output = destination / (package["name"] + "-" + package["version"])
        files = sorted(path for path in source.rglob("*") if path.is_file()
                       and path.name.upper().startswith(("LICENSE", "COPYING", "NOTICE"))
                       and path.resolve().is_relative_to(source))
        declared = package.get("license_file")
        if declared:
            path = (source / declared).resolve()
            if path.is_file() and path.is_relative_to(source) and path not in files:
                files.append(path)
        if not files and package["name"] in {"wasmi", "wasmi_core", "wasmi_ir", "wasmi_collections"} and package["version"] == "2.0.0":
            # The published workspace crates omit their shared root notices.
            # Keep the exact release's upstream notices in the repository.
            supplied = native / "licenses" / "wasmi-2.0.0"
            output.mkdir(parents=True, exist_ok=True)
            names = ["LICENSE-MIT", "LICENSE-APACHE"]
            for name in names:
                copy_notice(supplied / name, output / name)
        elif not files and package["name"] == "wasmparser" and package["version"] == "0.228.0":
            supplied = native / "licenses" / "wasmparser-0.228.0"
            output.mkdir(parents=True, exist_ok=True)
            names = ["LICENSE-MIT", "LICENSE-APACHE", "LICENSE-Apache-2.0_WITH_LLVM-exception"]
            for name in names:
                copy_notice(supplied / name, output / name)
        elif not files and source.is_relative_to(native):
            output.mkdir(parents=True, exist_ok=True)
            copy_notice(native.parent / "LICENSE", output / "LICENSE")
            names = ["LICENSE"]
        else:
            if not files:
                raise SystemExit("No accompanying license found for " + package["name"] + " " + package["version"])
            names = []
            for path in files:
                relative = path.relative_to(source)
                target = output / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                copy_notice(path, target)
                names.append(relative.as_posix())
        index.append({"name": package["name"], "version": package["version"],
                      "license": package.get("license"), "files": names})
    (destination / "index.json").write_text(json.dumps(index, indent=2) + "\n")
    print(f"Bundled accompanying license notices for {len(index)} production/build packages.")


if __name__ == "__main__":
    main()
