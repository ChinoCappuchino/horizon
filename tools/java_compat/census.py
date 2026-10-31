#!/usr/bin/env python3
# Fork `NeuHorizon`: docs/java-compat/README.md.
"""API census of mod jars, from class files' constant pools only (no code read): which Minecraft and
NeoForge classes, methods and fields each mod refers to, and its Mixins.  census.py <mods dir> <out.json>"""
import json, struct, sys, zipfile, collections
from pathlib import Path

def pool(data):
    if data[:4] != b'\xca\xfe\xba\xbe': return None
    n = struct.unpack('>H', data[8:10])[0]; i = 10; cp = [None] * n; k = 1
    while k < n:
        tag = data[i]
        if tag == 1:
            ln = struct.unpack('>H', data[i+1:i+3])[0]; cp[k] = ('utf8', data[i+3:i+3+ln].decode('utf-8', 'replace')); i += 3 + ln
        elif tag in (3, 4): i += 5
        elif tag in (5, 6): i += 9; k += 1
        elif tag == 7: cp[k] = ('class', struct.unpack('>H', data[i+1:i+3])[0]); i += 3
        elif tag == 8: i += 3
        elif tag in (9, 10, 11): cp[k] = (('field','method','imethod')[tag-9],) + struct.unpack('>HH', data[i+1:i+5]); i += 5
        elif tag == 12: cp[k] = ('nat',) + struct.unpack('>HH', data[i+1:i+5]); i += 5
        elif tag in (15,): i += 4
        elif tag in (16, 19, 20): i += 3
        elif tag in (17, 18): i += 5
        else: return None
        k += 1
    return cp

def refs(cp):
    s = lambda j: cp[j][1] if cp[j] and cp[j][0] == 'utf8' else ''
    cls = lambda j: s(cp[j][1]) if cp[j] and cp[j][0] == 'class' else ''
    out = set(); classes = set()
    for e in cp:
        if not e: continue
        if e[0] == 'class': classes.add(s(e[1]))
        if e[0] in ('field', 'method', 'imethod'):
            c = cls(e[1]); nat = cp[e[2]]
            out.add((e[0], c, s(nat[1]), s(nat[2])))
    strings = {x[1] for x in cp if x and x[0] == 'utf8'}
    return out, classes, strings

GAME = ('net/minecraft/', 'com/mojang/', 'net/neoforged/')

if __name__ == '__main__':
    mods = {}
    for jar in sorted(Path(sys.argv[1]).glob('*.jar')):
        try: z = zipfile.ZipFile(jar)
        except Exception: continue
        members = {}; mixins = 0; classes_n = 0; game_classes = set()
        nested = [n for n in z.namelist() if n.startswith('META-INF/jarjar/') and n.endswith('.jar')]
        for name in z.namelist():
            if not name.endswith('.class'): continue
            if name.startswith(GAME): continue   # bundled copies
            cp = pool(z.read(name))
            if cp is None: continue
            classes_n += 1
            r, cls, strings = refs(cp)
            for m in r:
                if m[1].startswith(GAME): members[m] = members.get(m, 0) + 1
            game_classes |= {c for c in cls if c.startswith(GAME)}
            if 'Lorg/spongepowered/asm/mixin/Mixin;' in strings: mixins += 1
        mods[jar.name] = {'classes': classes_n, 'mixins': mixins, 'game_classes': sorted(game_classes),
                          'members': sorted('|'.join(m) for m in members)}
    json.dump(mods, open(sys.argv[2], 'w'))
    allm = collections.Counter(); allc = collections.Counter()
    for m in mods.values():
        allm.update(m['members']); allc.update(m['game_classes'])
    code = {k: v for k, v in mods.items() if v['classes'] > 0}
    print(f"{len(mods)} jars, {len(code)} with code; {sum(v['classes'] for v in code.values())} classes, "
          f"{sum(v['mixins'] for v in code.values())} Mixin classes in {sum(1 for v in code.values() if v['mixins'])} jars")
    print(f"distinct game members referred to: {len(allm)}; distinct game classes: {len(allc)}")
    for lim in (1, 2, 5, 10):
        print(f"  members used by >= {lim} jars: {sum(1 for c in allm.values() if c >= lim)}")
    ns = collections.Counter(m.split('|')[1].split('/')[1] for m in allm)
    print('by namespace:', dict(ns))
    pk = collections.Counter('/'.join(m.split('|')[1].split('/')[:4]) for m in allm)
    print('top packages:', pk.most_common(15))
