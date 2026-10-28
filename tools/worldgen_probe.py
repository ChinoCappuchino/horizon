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
import subprocess
import sys
import time
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


def crash_report(run_dir, since):
    """The newest crash report the server wrote since `since`, as (path, exception line): a crash while the
    spawn area generates happens before the probe starts, so the probe has no chance to record it."""
    reports = sorted((p for p in (run_dir / 'crash-reports').glob('*.txt') if p.stat().st_mtime >= since),
                     key=lambda p: p.stat().st_mtime)
    if not reports:
        return None
    lines = reports[-1].read_text(errors='replace').splitlines()
    cause = next((l for l in lines if l and not l[0].isspace() and ('Exception' in l or 'Error' in l)
                  and not l.startswith(('Description:', '//'))), None)
    return reports[-1], cause or next((l for l in lines if l.startswith('Description:')), '?')


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
    error = Path(f'{out}.error')
    for stale in (out, error):
        stale.unlink(missing_ok=True)
    started = time.time()
    try:
        seconds, log = oracle.start(home, run_dir, {
            'horizon.probe.out': out, 'horizon.probe.radius': args.radius,
            'horizon.probe.cx': args.cx, 'horizon.probe.cz': args.cz}, timeout=args.timeout)
    except subprocess.TimeoutExpired:
        sys.exit(f'timed out after {args.timeout} s (server log: {run_dir / "server.log"})')
    if error.exists():
        sys.exit(f'generation crashed: {error.read_text().splitlines()[0]} (details: {error})')
    crash = crash_report(run_dir, started)
    if not out.exists() and crash:
        sys.exit(f'generation crashed: {crash[1]} (details: {crash[0]})')
    if not out.exists():
        tail = '\n'.join(log.splitlines()[-30:])
        sys.exit(f'no probe output after {seconds:.0f} s; the server log ends with:\n{tail}')
    data = json.loads(out.read_text())
    print(f'{row["name"]}: seed {args.seed}, order {args.order}: {len(data["chunks"])} chunks in {seconds:.0f} s '
          f'({data["generator"]}) -> {out}')
    return out


TERRAIN = ('base', 'biomes', 'starts')     # the generator's own height, biomes there, structure starts: identical
DECORATION = ('logs', 'leaves', 'ores')    # vanilla itself varies here (feature order): compared as totals


def summarise(path):
    """Land share, ground range, structure starts and tree/ore totals of one probe file."""
    d = json.loads(Path(path).read_text())
    biomes = [b for c in d['chunks'] for b in c['biomes']]
    heights = [h for c in d['chunks'] for h in c['ground']]
    land = sum(1 for b in biomes if 'ocean' not in b and 'river' not in b) / max(1, len(biomes))
    starts = sorted({s['id'] for c in d['chunks'] for s in c['starts']})
    return {'generator': d['generator'].rsplit('.', 1)[-1], 'land': round(100 * land),
            'ground': [min(heights), max(heights)], 'starts': starts,
            'logs': sum(c['logs'] for c in d['chunks']),
            'leaves': sum(c['leaves'] for c in d['chunks']),
            'ores': sum(sum(c['ores'].values()) for c in d['chunks'])}


def terrain(path):
    d = json.loads(Path(path).read_text())
    return [[c[k] for k in TERRAIN] for c in d['chunks']]


def decoration_spread(paths):
    """Largest relative difference between the runs, in percent: the decoration totals, and the share of
    ground columns (terrain plus what features put on it) that differ."""
    worst = 0.0
    grounds = [[g for c in json.loads(Path(p).read_text())['chunks'] for g in c['ground']] for p in paths]
    for g in grounds[1:]:
        worst = max(worst, 100 * sum(1 for a, b in zip(grounds[0], g) if a != b) / max(1, len(g)))
    sums = [summarise(p) for p in paths]
    for k in DECORATION:
        vals = [s[k] for s in sums]
        if max(vals):
            worst = max(worst, 100 * (max(vals) - min(vals)) / max(vals))
    return round(worst, 2)


def matrix(args):
    """Every row x seed x load order; then the determinism check: load order must change nothing."""
    m = tomllib.loads(MATRIX.read_text())
    wanted = [r for r in m['row'] if not args.rows or r['name'] in args.rows]
    results, lines = {}, []
    for row in wanted:
        for seed in m.get('seeds', [1]):
            files = []
            for order in m.get('orders', [0]):
                done = OUT / row['name'] / f'seed-{seed}-order-{order}.json'
                if args.resume and done.exists() and 'base' in json.loads(done.read_text())['chunks'][0]:
                    files.append(done)                      # already probed with this probe version
                    continue
                ns = argparse.Namespace(row=row['name'], seed=seed, order=order, radius=args.radius, cx=0, cz=0,
                                        timeout=args.timeout, accept_eula=args.accept_eula)
                try:
                    files.append(run(ns))
                except SystemExit as e:
                    print(f'{row["name"]} seed {seed} order {order}: FAILED: {e}')
                    files.append(None)
            ok = [f for f in files if f]
            if not ok:
                results[f'{row["name"]}/{seed}'] = {'terrain_identical': False, 'failed': True, 'runs': 0}
                lines.append(f'| {row["name"]} | {seed} | FAILED | - | - | - | - | - | - | - |')
                continue
            # Terrain is compared between the runs that finished; runs that crashed are counted apart, so a
            # crash is not mistaken for load order changing the world.
            same = all(terrain(f) == terrain(ok[0]) for f in ok)
            partial = len(ok) < len(files)
            verdict = ('yes' if same else 'NO') + (f' (partial: {len(ok)} of {len(files)} runs, rest crashed)'
                                                   if partial else '')
            spread = decoration_spread(ok)
            s = summarise(ok[0])
            results[f'{row["name"]}/{seed}'] = {'terrain_identical': same, 'decoration_spread_percent': spread,
                                                'runs': len(ok), 'crashed': len(files) - len(ok), **s}
            lines.append(f'| {row["name"]} | {seed} | {verdict} | {spread} % | '
                         f'{s.get("generator", "-")} | {s.get("land", "-")} % | {s.get("ground", "-")} | '
                         f'{len(s.get("starts", []))} | {s.get("logs", "-")} | {s.get("ores", "-")} |')
    OUT.mkdir(parents=True, exist_ok=True)
    tag = args.tag or 'summary'
    (OUT / f'{tag}.json').write_text(json.dumps(results, indent=1))
    table = ['| row | seed | terrain identical in every load order | decoration spread | generator | land | ground '
             '| structure types | logs | ores |',
             '|---|---:|---|---:|---|---:|---|---:|---:|---:|', *lines]
    (OUT / f'{tag}.md').write_text('\n'.join(table) + '\n')
    print('\n'.join(table))
    if not all(r['terrain_identical'] for r in results.values() if not r.get('failed')):
        sys.exit('load order changed the terrain in at least one row')
    if any(r.get('failed') or r.get('crashed') for r in results.values()):
        sys.exit('generation crashed in at least one run (see FAILED and partial rows)')


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
    mx.add_argument('--tag', default='', help='name of the summary files (default: summary)')
    mx.add_argument('--resume', action='store_true', help='keep runs already probed with this probe version')
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
