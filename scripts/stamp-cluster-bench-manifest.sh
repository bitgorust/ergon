#!/usr/bin/env bash
# Stamp a cluster Criterion tree with run provenance for packaging.
set -euo pipefail
dir="${1:?criterion dir}"
profile="${2:?profile}"
run_id="${3:?run_id}"
commit="${4:?commit}"
rustc_v="${5:?rustc}"
target="${6:?target}"
python3 - "$dir" "$run_id" "$profile" "$commit" "$rustc_v" "$target" <<'PY'
import json, os, sys

directory, run_id, profile, commit, rustc, target = sys.argv[1:7]
stamped = 0
for root, dirs, files in os.walk(directory):
    if os.path.basename(root) == "new" and "estimates.json" in files:
        with open(os.path.join(root, "run-id.txt"), "w", encoding="utf-8") as handle:
            handle.write(run_id)
        stamped += 1
if stamped == 0:
    sys.exit(f"no Criterion estimates were produced under {directory}")
# Each independent invocation the runner took is snapshotted under
# `repeats/<k>/`; stamp those too, and declare how many there are. A repeat
# from another run would widen the gate's envelope with foreign evidence, and
# a manifest claiming more invocations than are on disk is refused by the gate.
repeats_dir = os.path.join(directory, "repeats")
repeats = 0
if os.path.isdir(repeats_dir):
    for name in sorted(
        (n for n in os.listdir(repeats_dir) if os.path.isdir(os.path.join(repeats_dir, n))),
        key=lambda n: (0, int(n)) if n.isdigit() else (1, n),
    ):
        repeat_root = os.path.join(repeats_dir, name)
        found = 0
        for root, dirs, files in os.walk(repeat_root):
            if "estimates.json" in files:
                with open(os.path.join(root, "run-id.txt"), "w", encoding="utf-8") as handle:
                    handle.write(run_id)
                found += 1
        if found:
            repeats += 1
manifest = {
    "run_id": run_id,
    "profile": profile,
    "commit": commit,
    "rustc": rustc,
    "target": target,
    "estimates": stamped,
    "repeats": repeats if repeats > 1 else 1,
}
with open(os.path.join(directory, "run-manifest.json"), "w", encoding="utf-8") as handle:
    json.dump(manifest, handle, indent=2)
print(f"stamped cluster {profile} run_id={run_id} estimates={stamped} repeats={manifest['repeats']}")
PY
