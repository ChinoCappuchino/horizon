#!/usr/bin/env python3
"""Compares a mod run through the Java compatibility compiler with the same mod on NeoForge
(docs/java-compat/plan.md, phase 0, step 3).

    neoforge_compare.py RECORD.json NEOFORGE.content.json

RECORD.json comes from `cargo run -p horizon-java --example record -- <mod.jar> RECORD.json`: the
compiled jar, run against the recording stand-in for NeoForge. NEOFORGE.content.json comes from
`tools/worldgen_probe.py run <row> --content <namespace> --accept-eula`: the original jar on a
NeoForge server. Both list the mod's POI types, villager professions, trade offers and template
pool elements. Every value must be equal, except two forms the recorder cannot know:

- a sound's id: the recorder knows the field (BLOCK_SKILLET_ADD_FOOD), not the id
  (block.skillet.add_food), so `.` and `_` are the same here, and the field's name may leave out
  the id's start (SoundEvents.BUBBLE_COLUMN_UPWARDS_AMBIENT is block.bubble_column.upwards_ambient);
- a float: the recorder prints a float32 as a double (0.05000000074505806), so floats are
  rounded to 6 places.

Exit status 0 when they agree.
"""
import json
import sys


def normal(value, key=None):
    if isinstance(value, dict):
        return {k: normal(v, k) for k, v in value.items()}
    if isinstance(value, list):
        return [normal(v) for v in value]
    if isinstance(value, float):
        return round(value, 6)
    if key == 'work_sound' and isinstance(value, str):
        return value.replace('.', '_')
    return value


def compare(path, mine, theirs, out):
    if isinstance(mine, dict) and isinstance(theirs, dict):
        for k in sorted(set(mine) | set(theirs)):
            if k not in theirs:
                out.append(f'{path}/{k}: only in the compiled run: {json.dumps(mine[k])[:160]}')
            elif k not in mine:
                out.append(f'{path}/{k}: only on NeoForge: {json.dumps(theirs[k])[:160]}')
            else:
                compare(f'{path}/{k}', mine[k], theirs[k], out)
    elif isinstance(mine, list) and isinstance(theirs, list):
        if len(mine) != len(theirs):
            out.append(f'{path}: {len(mine)} in the compiled run, {len(theirs)} on NeoForge')
        for i, (a, b) in enumerate(zip(mine, theirs)):
            compare(f'{path}[{i}]', a, b, out)
    elif path.endswith('/work_sound') and isinstance(mine, str) and isinstance(theirs, str):
        (ns_a, _, a), (ns_b, _, b) = mine.partition(':'), theirs.partition(':')
        if ns_a != ns_b or not (b == a or b.endswith('_' + a)):
            out.append(f'{path}: {json.dumps(mine)} in the compiled run, {json.dumps(theirs)} on NeoForge')
    elif mine != theirs:
        out.append(f'{path}: {json.dumps(mine)} in the compiled run, {json.dumps(theirs)} on NeoForge')


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    record = json.load(open(sys.argv[1]))
    neoforge = json.load(open(sys.argv[2]))
    differences = []
    counts = {}
    for section in ('poi_types', 'professions', 'trades', 'pools'):
        mine, theirs = normal(record.get(section, {})), normal(neoforge.get(section, {}))
        compare(section, mine, theirs, differences)
        if section == 'trades':
            counts[section] = sum(len(o) for p in theirs.values() for o in p.values())
        else:
            counts[section] = len(theirs)
    print('NeoForge: ' + ', '.join(f'{n} {s.replace("_", " ")}' for s, n in counts.items())
          + ' (trades: offers)')
    if record.get('not_modelled'):
        print(f'calls the recorder does not model ({len(record["not_modelled"])}):')
        for line in record['not_modelled']:
            print(f'  {line}')
    if differences:
        print(f'{len(differences)} differences:')
        for d in differences:
            print(f'  {d}')
        return 1
    print('the compiled run and NeoForge agree')
    return 0


if __name__ == '__main__':
    sys.exit(main())
