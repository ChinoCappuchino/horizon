#!/usr/bin/env python3
"""Dev-only world-generation probe on NeoForge for the fork's matrix (docs/research/worldgen-hooks.md).

    worldgen_probe.py build                                   compile the probe mod only
    worldgen_probe.py run ROW [--seed N] [--order K] [--radius R] --accept-eula
    worldgen_probe.py list                                    the rows of tools/worldgen_matrix.toml

Each row of tools/worldgen_matrix.toml names mod ids and a world preset. The jars come from the
player's own pack folder (HORIZON_PACK_MODS: an installed instance's mods/), with their required
dependencies added from the same folder. The server is the NeoForge version in HORIZON_NEOFORGE
(21.1.251 for the Afterwinter pack). Starting a Minecraft server means accepting Mojang's EULA
(https://aka.ms/MinecraftEULA): `run` refuses to start without --accept-eula, which the person who
owns the game passes. Output: .local/worldgen-probe/<row>/seed-<seed>-order-<k>.json. Nothing here is
shipped or committed; the jars stay the player's own.
"""
import argparse
import io
import json
import os
import random
import shutil
import sys
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tools'))
import neoforge_oracle as oracle  # noqa: E402

PROBE = ROOT / 'mods' / 'horizon' / 'src' / 'neoforge-worldgen-probe'
MATRIX = ROOT / 'tools' / 'worldgen_matrix.toml'
OUT = ROOT / '.local' / 'worldgen-probe'
PROPERTIES = """online-mode=false
server-port=0
spawn-protection=0
view-distance=2
simulation-distance=2
generate-structures=true
spawn-monsters=false
spawn-animals=false
level-name=world
level-seed={seed}
level-type={preset}
"""
NOT_MODS = {'minecraft', 'neoforge', 'forge', 'java'}


def is_required(dep):
    """NeoForge writes `type = "required"`; older files write `required = true` or `mandatory = true`."""
    if 'type' in dep:
        return str(dep['type']).lower() == 'required'
    return bool(dep.get('required', dep.get('mandatory', False)))


def toml_facts(text):
    t = tomllib.loads(text)
    ids = [m['modId'] for m in t.get('mods', []) if 'modId' in m]
    deps = set()
    for mid in ids:
        for d in t.get('dependencies', {}).get(mid, []):
            if is_required(d) and d.get('modId') not in NOT_MODS:
                deps.add(d['modId'])
    return ids, deps


def zip_facts(z):
    """(mod ids, required deps, mod ids bundled inside it as jar-in-jar) of an open jar."""
    names = z.namelist()
    ids, deps, bundled = [], set(), set()
    for name in ('META-INF/neoforge.mods.toml', 'META-INF/mods.toml'):
        if name in names:
            try:
                ids, deps = toml_facts(z.read(name).decode('utf-8', 'replace'))
            except (tomllib.TOMLDecodeError, KeyError):
                pass
            break
    for inner in names:
        if inner.startswith('META-INF/jarjar/') and inner.endswith('.jar'):
            try:
                with zipfile.ZipFile(io.BytesIO(z.read(inner))) as nested:
                    nested_ids, nested_deps, nested_bundled = zip_facts(nested)
                bundled.update(nested_ids, nested_bundled)
                deps |= nested_deps
            except zipfile.BadZipFile:
                pass
    return ids, deps, bundled


def jar_facts(jar):
    """(mod ids it provides, including bundled ones, and its required dependency ids)."""
    try:
        with zipfile.ZipFile(jar) as z:
            ids, deps, bundled = zip_facts(z)
    except zipfile.BadZipFile:
        return [], set()
    provided = [*ids, *sorted(bundled - set(ids))]
    return provided, deps - set(provided)


def pack_index(folder):
    """mod id -> (jar, required deps) for every jar in the pack folder (bundled mods map to their jar)."""
    index = {}
    for jar in sorted(Path(folder).glob('*.jar')):
        ids, deps = jar_facts(jar)
        for mid in ids:
            index.setdefault(mid, (jar, deps))
    return index


def resolve(wanted, index):
    """The jars for the wanted mod ids plus every required dependency; missing ids are an error."""
    jars, seen, todo, missing = [], set(), list(wanted), []
    while todo:
        mid = todo.pop()
        if mid in seen:
            continue
        seen.add(mid)
        if mid not in index:
            missing.append(mid)
            continue
        jar, deps = index[mid]
        if jar not in jars:
            jars.append(jar)
        todo.extend(sorted(deps))
    if missing:
        sys.exit(f'not in the pack folder: {", ".join(sorted(missing))}')
    return sorted(jars)


def rows():
    return tomllib.loads(MATRIX.read_text())['row']


def run(args):
    if not args.accept_eula:
        sys.exit('Starting a Minecraft server means accepting the Minecraft EULA (https://aka.ms/MinecraftEULA).\n'
                 'Only the person who owns the game decides that: rerun with --accept-eula.')
    pack = os.environ.get('HORIZON_PACK_MODS')
    if not pack:
        sys.exit('set HORIZON_PACK_MODS to the mods/ folder of your installed pack')
    row = next((r for r in rows() if r['name'] == args.row), None)
    if row is None:
        sys.exit(f'no row {args.row!r} in {MATRIX}')
    jars = resolve(row.get('mods', []), pack_index(pack))
    home = oracle.install()
    probe = oracle.build(home, source=PROBE, name='horizon-worldgen-probe.jar')
    name = f'worldgen-{row["name"]}-{args.seed}-{args.order}'
    _, run_dir = oracle.instance(name, [], extra=[])
    # The load order varies by name: the same jars, linked under shuffled prefixes.
    shuffled = list(jars)
    if args.order:
        random.Random(args.order).shuffle(shuffled)
    for i, jar in enumerate([*shuffled, probe]):
        (run_dir / 'mods' / f'{i:03d}-{Path(jar).name}').symlink_to(Path(jar).resolve())
    preset = row.get('preset', 'minecraft:normal').replace(':', '\\:')
    (run_dir / 'server.properties').write_text(PROPERTIES.format(seed=args.seed, preset=preset))
    out = OUT / row['name'] / f'seed-{args.seed}-order-{args.order}.json'
    out.parent.mkdir(parents=True, exist_ok=True)
    seconds, log = oracle.start(home, run_dir, {
        'horizon.probe.out': out, 'horizon.probe.radius': args.radius,
        'horizon.probe.cx': args.cx, 'horizon.probe.cz': args.cz}, timeout=args.timeout)
    if not out.exists():
        tail = '\n'.join(log.splitlines()[-30:])
        sys.exit(f'no probe output after {seconds:.0f} s; the server log ends with:\n{tail}')
    data = json.loads(out.read_text())
    print(f'{row["name"]}: seed {args.seed}, order {args.order}: {len(data["chunks"])} chunks in {seconds:.0f} s '
          f'({data["generator"]}) -> {out}')
    return out


def summarise(path):
    """Land share, height range, structure starts and tree/ore totals of one probe file."""
    d = json.loads(Path(path).read_text())
    biomes = [b for c in d['chunks'] for b in c['biomes']]
    heights = [h for c in d['chunks'] for h in c['surface']]
    land = sum(1 for b in biomes if 'ocean' not in b and 'river' not in b) / max(1, len(biomes))
    starts = sorted({s['id'] for c in d['chunks'] for s in c['starts']})
    return {'generator': d['generator'].rsplit('.', 1)[-1], 'land': round(100 * land),
            'height': [min(heights), max(heights)], 'starts': starts,
            'logs': sum(c['logs'] for c in d['chunks']),
            'ores': sum(sum(c['ores'].values()) for c in d['chunks'])}


def matrix(args):
    """Every row x seed x load order; then the determinism check: load order must change nothing."""
    m = tomllib.loads(MATRIX.read_text())
    wanted = [r for r in m['row'] if not args.rows or r['name'] in args.rows]
    results, lines = {}, []
    for row in wanted:
        for seed in m.get('seeds', [1]):
            files = []
            for order in m.get('orders', [0]):
                ns = argparse.Namespace(row=row['name'], seed=seed, order=order, radius=args.radius, cx=0, cz=0,
                                        timeout=args.timeout, accept_eula=args.accept_eula)
                try:
                    files.append(run(ns))
                except SystemExit as e:
                    print(f'{row["name"]} seed {seed} order {order}: FAILED: {e}')
                    files.append(None)
            ok = [f for f in files if f]
            same = len(ok) == len(files) and len({Path(f).read_bytes() for f in ok}) == 1
            s = summarise(ok[0]) if ok else {}
            results[f'{row["name"]}/{seed}'] = {'deterministic': same, 'runs': len(ok), **s}
            lines.append(f'| {row["name"]} | {seed} | {"yes" if same else "NO"} | {s.get("generator", "-")} | '
                         f'{s.get("land", "-")} % | {s.get("height", "-")} | {len(s.get("starts", []))} | '
                         f'{s.get("logs", "-")} | {s.get("ores", "-")} |')
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / 'summary.json').write_text(json.dumps(results, indent=1))
    table = ['| row | seed | same in every load order | generator | land | surface | structure types | logs | ores |',
             '|---|---:|---|---|---:|---|---:|---:|---:|', *lines]
    (OUT / 'summary.md').write_text('\n'.join(table) + '\n')
    print('\n'.join(table))
    if not all(r['deterministic'] for r in results.values()):
        sys.exit('load order changed the generated world in at least one row')


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    sub = ap.add_subparsers(dest='cmd', required=True)
    sub.add_parser('build')
    sub.add_parser('list')
    r = sub.add_parser('run')
    r.add_argument('row')
    r.add_argument('--seed', type=int, default=1)
    r.add_argument('--order', type=int, default=0, help='0 keeps file-name order; other values shuffle it')
    r.add_argument('--radius', type=int, default=4)
    r.add_argument('--cx', type=int, default=0)
    r.add_argument('--cz', type=int, default=0)
    r.add_argument('--timeout', type=int, default=1800)
    r.add_argument('--accept-eula', action='store_true')
    mx = sub.add_parser('matrix', help='every row x seed x load order, then the determinism check')
    mx.add_argument('rows', nargs='*', help='only these rows (default: all)')
    mx.add_argument('--radius', type=int, default=4)
    mx.add_argument('--timeout', type=int, default=1800)
    mx.add_argument('--accept-eula', action='store_true')
    a = ap.parse_args()
    if a.cmd == 'build':
        print(oracle.build(oracle.install(), source=PROBE, name='horizon-worldgen-probe.jar'))
    elif a.cmd == 'list':
        for row in rows():
            print(f'{row["name"]:<24} {row.get("preset", "minecraft:normal"):<36} {", ".join(row.get("mods", [])) or "-"}')
    elif a.cmd == 'matrix':
        if not a.accept_eula:
            sys.exit('Starting a Minecraft server means accepting the Minecraft EULA (https://aka.ms/MinecraftEULA).\n'
                     'Only the person who owns the game decides that: rerun with --accept-eula.')
        matrix(a)
    else:
        run(a)


if __name__ == '__main__':
    main()
