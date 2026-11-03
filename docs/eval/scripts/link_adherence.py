#!/usr/bin/env python3
"""How saved groupings treat code-intelligence symbol links (docs/eval/symbol-links.md).

usage: link_adherence.py <links.json> <runs-dir> <set> <label> [label ...]

<links.json> maps fixture name -> [{"from", "to", "symbol"}] (the links
`theodore eval --symbol-links` printed). For each label, every link is:
  same   - both hunks in one group
  before - the defining hunk's group comes earlier (reading order kept)
  after  - the defining hunk's group comes later
Prints one row per label: links, same, before, after, groups, wall seconds.
"""

import json
import os
import sys


def main():
    links_path, runs, set_, labels = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4:]
    with open(links_path) as f:
        links = json.load(f)
    print("| run | links | same group | defining group first | defining group later | groups (linked PRs) | wall (s, linked PRs) |")
    print("| --- | ---: | ---: | ---: | ---: | ---: | ---: |")
    for label in labels:
        same = before = after = groups = 0
        wall = 0.0
        for name, ls in links.items():
            path = os.path.join(runs, label, set_, name + ".json")
            if not ls or not os.path.exists(path):
                continue
            with open(path) as f:
                rep = json.load(f)
            gs = rep["grouping"]["groups"]
            groups += len(gs)
            wall += rep["duration_ms"] / 1000
            at = {h: i for i, g in enumerate(gs) for h in g["hunks"]}
            for l in ls:
                a, b = at.get(l["from"]), at.get(l["to"])
                if a is None or b is None:
                    continue
                if a == b:
                    same += 1
                elif a < b:
                    before += 1
                else:
                    after += 1
        n = same + before + after
        print(f"| {label} | {n} | {same} | {before} | {after} | {groups} | {wall:.0f} |")


if __name__ == "__main__":
    main()
