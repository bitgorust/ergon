#!/usr/bin/env python3
"""Copy the "Work behind the percentiles" tables out of a run's generated tables.md.

`tables.md` is gitignored (it is generated and large), but the completeness
counts cited in BENCHMARK.md have to be checkable from the repository. This
writes a small `work-counts.md` beside the run's `fair.md`, which is tracked.

    python3 scripts/extract_work_counts.py results/<stamp>

The run's `records.bin` is long gone by then, so this copies the counts the
analyser already reconciled against it before dropping any warm-up.
"""

import sys
from pathlib import Path


def extract(run_dir: Path) -> int:
    src_path = run_dir / "tables.md"
    if not src_path.is_file():
        print(f"{src_path}: no tables.md", file=sys.stderr)
        return 1
    src = src_path.read_text().splitlines()
    out = [
        f"# Work behind the percentiles — run {run_dir.name}",
        "",
        "Extracted from the run's generated `tables.md` (gitignored by policy) so the",
        "completeness counts cited in `BENCHMARK.md` are checkable from the repository.",
        "Counts are the run's own account, reconciled against `records.bin` before any",
        "warm-up is dropped. Every percentile in `BENCHMARK.md` is over `completed`",
        "alone, after the warm-up cut, so `n` is smaller still.",
        "",
        "`scheduled` `issued` `completed` `late` `rejected` `censored`",
        "",
    ]
    cur = "unknown"
    i = 0
    n = 0
    while i < len(src):
        line = src[i]
        if line.startswith("## "):
            cur = line[3:].strip()
        if line.startswith("### Work behind the percentiles"):
            out.append(f"## {cur}")
            out.append("")
            j = i + 1
            while j < len(src) and not src[j].startswith("| "):
                j += 1
            while j < len(src) and src[j].startswith("|"):
                out.append(src[j])
                j += 1
            out.append("")
            n += 1
            i = j
            continue
        i += 1
    (run_dir / "work-counts.md").write_text("\n".join(out) + "\n")
    print(f"{run_dir / 'work-counts.md'}: {n} tables")
    return 0


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    return extract(Path(sys.argv[1]))


if __name__ == "__main__":
    sys.exit(main())
