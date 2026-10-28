#!/usr/bin/env python3
"""Large Ore Deposits' deposits, from a pack's own config files, for Horizon's port of the mod
(mods/ports/adlods, fork `afterwinter`, M3g).

    adlods_port.py <pack config/adlods> <prepared resources> <out deposits.json>

The configs (`config/adlods/Deposits/*.cfg`, Forge's config format) say what each deposit is made
of, where and how rare; the port's code places them as observed on NeoForge 1.21.1
(docs/research/large-ore-deposits.md). Tags are expanded from the prepared resources' tag files;
a tag nothing defines (the `c:` tags of mods the pack doesn't have) is empty, and a deposit left
without ores is listed and dropped. The output is the player's own (from their pack's configs) and
is never committed: build the mod with ADLODS_DEPOSITS=<out>.
"""
import json
import re
import sys
from pathlib import Path


def parse(text):
    """Forge's config format: `S:key=value`, `S:key < ... >` lists, `Name { ... }` sections. In a
    list every line is a value: one starting with `#` is a tag, not a comment."""
    root, stack, lines, i = {}, [], text.splitlines(), 0
    stack.append(root)
    while i < len(lines):
        raw = lines[i].strip()
        i += 1
        if not raw or raw.startswith('#'):
            continue
        if raw.endswith('{'):
            section = {}
            stack[-1][raw[:-1].strip()] = section
            stack.append(section)
            continue
        if raw == '}':
            stack.pop()
            continue
        m = re.match(r'([SBIDL]):"?([^=<"]+)"?\s*(=|<)\s*(.*)$', raw)
        if not m:
            continue
        kind, key, op, value = m.groups()
        if op == '<':
            items = []
            while i < len(lines) and lines[i].strip() != '>':
                if lines[i].strip():
                    items.append(lines[i].strip())
                i += 1
            i += 1
            stack[-1][key.strip()] = items
        else:
            value = value.strip()
            stack[-1][key.strip()] = value == 'true' if kind == 'B' else int(value) if kind == 'I' else value
    return root


def block_id(text):
    """`coal_ore`, `mod:block`, `mod:block:[props]` or `mod:block:*` as a block state string."""
    text = text.strip()
    props = ''
    m = re.match(r'^(.*?):?(\[.*\])$', text)
    if m:
        text, props = m.group(1), m.group(2).replace(' ', '')
    if text.endswith(':*'):
        text = text[:-2]
    parts = text.split(':')
    name = text if len(parts) >= 2 else f'minecraft:{text}'
    return name + props


class Tags:
    def __init__(self, resources):
        self.root = Path(resources) / 'data'

    def expand(self, tag, seen=None):
        seen = seen or set()
        if tag in seen:
            return []
        seen.add(tag)
        ns, path = tag.split(':', 1) if ':' in tag else ('minecraft', tag)
        f = self.root / ns / 'tags' / 'block' / f'{path}.json'
        if not f.exists():
            return []
        out = []
        for v in json.loads(f.read_text())['values']:
            v = v if isinstance(v, str) else v['id']
            out += self.expand(v[1:], seen) if v.startswith('#') else [v]
        return out

    def blocks(self, entry):
        entry = entry.strip()
        return self.expand(entry[1:]) if entry.startswith('#') else [block_id(entry)]


def weighted(entry):
    name, _, weight = entry.partition(',')
    return name.strip(), int(weight) if weight.strip() else 1


def main():
    config, resources, out = sys.argv[1:4]
    tags = Tags(resources)
    deposits, dropped = [], []
    for f in sorted(Path(config, 'Deposits').glob('*.cfg')):
        cfg = parse(f.read_text())
        d = cfg.get('Deposit', {})
        if not cfg.get('Config', {}).get('enabled', True) or not d:
            continue
        ores = []
        for entry in d.get('ores', []):
            name, weight = weighted(entry)
            ores += [(b, weight) for b in tags.blocks(name)]
        if not ores:
            dropped.append(f.stem)
            continue
        custom = []
        for rule in d.get('customReplacements', []):
            target, _, replacement = rule.partition('->')
            replacement, weight = weighted(replacement)
            custom.append({'from': tags.blocks(target), 'to': block_id(replacement), 'weight': weight})
        misc, alt, size = d.get('Miscellaneous', {}), d.get('Altitude', {}), d.get('Size', {})
        deposits.append({
            'name': f.stem,
            'ores': [{'block': b, 'weight': w} for b, w in ores],
            'replaceable': [b for e in d.get('replaceableBlocks', []) for b in tags.blocks(e)],
            'custom': custom,
            'rarity': d.get('rarity', 1020),
            'size': [size.get('min', 700), size.get('max', 1400)],
            'altitude': [int(alt.get('min', -64)), int(alt.get('max', 64))],
            'dimensions': d.get('Dimensions', {}).get('whiteList', []),
            'exposed': misc.get('exposed', True),
        })
    Path(out).write_text(json.dumps(deposits, indent=1))
    print(f'{len(deposits)} deposits -> {out}: {", ".join(x["name"] for x in deposits)}')
    if dropped:
        print(f'no ores in this pack (left out): {", ".join(dropped)}')


if __name__ == '__main__':
    main()
