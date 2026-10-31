#!/usr/bin/env python3
# Fork `NeuHorizon`: docs/java-compat/README.md.
"""What the pack's Mixins patch, from constant pools: each Mixin class's likely target (the game
class descriptors it names), grouped by the area of the game and by the jar's role."""
import json, re, sys, zipfile, collections
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
from census import pool
MODS = Path(sys.argv[1]); cov = {m['key']: m for m in json.load(open(sys.argv[2]))}
area = lambda c: ('client/render+gui' if c.startswith(('net/minecraft/client', 'com/mojang/blaze3d', 'net/neoforged/neoforge/client'))
                  else 'world/gameplay' if c.startswith(('net/minecraft/world', 'net/minecraft/server', 'net/minecraft/core', 'net/minecraft/tags'))
                  else 'network/data' if c.startswith(('net/minecraft/network', 'net/minecraft/nbt', 'net/minecraft/resources', 'net/minecraft/util', 'net/minecraft/data'))
                  else 'other-mods' if not c.startswith(('net/minecraft', 'com/mojang', 'net/neoforged')) else 'other-game')
by_area = collections.Counter(); by_role = collections.Counter(); per_jar = {}
for jar in sorted(MODS.glob('*.jar')):
    try: z = zipfile.ZipFile(jar)
    except Exception: continue
    stem = jar.name
    key = next((k for k in cov if k and k in stem.lower().replace('-', '').replace('_', '')), None)
    role = (cov[key]['class'], cov[key]['status']) if key else ('?', '?')
    areas = collections.Counter()
    for name in z.namelist():
        if not name.endswith('.class'): continue
        cp = pool(z.read(name))
        if not cp: continue
        u = [x[1] for x in cp if x and x[0] == 'utf8']
        if 'Lorg/spongepowered/asm/mixin/Mixin;' not in u: continue
        # The target: class descriptors (not method signatures) and dotted target strings.
        targets = [s[1:-1] for s in u if s.startswith('L') and s.endswith(';') and '(' not in s and not s.startswith('Lorg/spongepowered') and not s.startswith('Ljava/')]
        targets += [s.replace('.', '/') for s in u if re.match(r'^(net\.minecraft|com\.mojang|net\.neoforged)\.[\w.$]+$', s)]
        own = name[:-6].rsplit('/', 1)[0]
        targets = [t for t in targets if not t.startswith(own.split('/')[0] + '/' + own.split('/')[1])] or ['?']
        a = area(collections.Counter(targets).most_common(1)[0][0])
        areas[a] += 1; by_area[a] += 1; by_role[(role, a)] += 1
    if areas: per_jar[jar.name] = (role, dict(areas))
print('Mixin classes by area:', dict(by_area))
roles = collections.Counter()
for (role, a), n in by_role.items(): roles[role] += n
print('by the jar role (class, port status):', roles.most_common())
print()
for j, (role, a) in sorted(per_jar.items(), key=lambda x: -sum(x[1][1].values()))[:25]:
    print(f'{sum(a.values()):4}  {j[:45]:45} {role}  {a}')
