"""The Minecraft and NeoForge API surface a set of mod jars uses, from class
file structure only (constant pools and class annotations; no code read).

usage: api_surface.py OUT.json MOD.jar... -- PLATFORM.jar...
Platform jars (Minecraft, NeoForge) tell which referenced classes exist.
"""
import io, json, struct, sys, zipfile
from collections import defaultdict


def classes_in(path, nested=True):
    """(name, bytes) of each class in a jar, nested jars (jarjar) too."""
    def walk(z, label):
        for n in z.namelist():
            if n.endswith(".class") and not n.startswith("META-INF/versions/"):
                yield label, n[:-6], z.read(n)
            elif nested and n.endswith(".jar"):
                with zipfile.ZipFile(io.BytesIO(z.read(n))) as inner:
                    yield from walk(inner, n.rsplit("/", 1)[-1])
    with zipfile.ZipFile(path) as z:
        yield from walk(z, path.rsplit("/", 1)[-1])


def parse(data):
    """Member refs (kind, owner, name, desc), class refs, and @Mixin targets."""
    u2 = lambda o: struct.unpack_from(">H", data, o)[0]
    u4 = lambda o: struct.unpack_from(">I", data, o)[0]
    count = u2(8)
    cp = [None] * count
    o, i = 10, 1
    while i < count:
        tag = data[o]
        if tag == 1:
            n = u2(o + 1)
            cp[i] = ("utf8", data[o + 3:o + 3 + n].decode("utf-8", "replace"))
            o += 3 + n
        elif tag in (3, 4):
            o += 5
        elif tag in (5, 6):
            o += 9
            i += 1
        elif tag in (7, 8, 16, 19, 20):
            cp[i] = (tag, u2(o + 1))
            o += 3
        elif tag in (9, 10, 11, 12, 17, 18):
            cp[i] = (tag, u2(o + 1), u2(o + 3))
            o += 5
        elif tag == 15:
            o += 4
        else:
            raise ValueError(f"constant tag {tag}")
        i += 1
    utf = lambda k: cp[k][1]
    cls = lambda k: utf(cp[k][1])
    refs, crefs = [], set()
    for e in cp:
        if not e:
            continue
        if e[0] in (9, 10, 11):
            nat = cp[e[2]]
            kind = {9: "field", 10: "method", 11: "method"}[e[0]]
            refs.append((kind, cls(e[1]), utf(nat[1]), utf(nat[2])))
        elif e[0] == 7:
            crefs.add(utf(e[1]))
    # Class attributes: skip header, interfaces, fields, methods.
    o += 6
    o += 2 + 2 * u2(o)
    for _ in range(2):
        n = u2(o)
        o += 2
        for _ in range(n):
            o += 6
            a = u2(o)
            o += 2
            for _ in range(a):
                o += 6 + u4(o + 2)
    mixin = []
    n = u2(o)
    o += 2
    for _ in range(n):
        name, length = utf(u2(o)), u4(o + 2)
        body = o + 6
        if name in ("RuntimeInvisibleAnnotations", "RuntimeVisibleAnnotations"):
            mixin += annotations(data, body, utf, u2)
        o = body + length
    return refs, crefs, mixin


def annotations(data, o, utf, u2):
    """@Mixin targets (value classes, targets strings) in an annotations attribute."""
    out = []

    def value(o, collect):
        tag = chr(data[o])
        o += 1
        if tag in "BCDFIJSZs":
            if collect and tag == "s":
                out.append(utf(u2(o)).replace(".", "/"))
            return o + 2
        if tag == "e":
            return o + 4
        if tag == "c":
            if collect:
                d = utf(u2(o))
                out.append(d[1:-1] if d.startswith("L") else d)
            return o + 2
        if tag == "@":
            return annotation(o, False)
        if tag == "[":
            k = u2(o)
            o += 2
            for _ in range(k):
                o = value(o, collect)
            return o
        raise ValueError(tag)

    def annotation(o, top):
        t = utf(u2(o))
        pairs = u2(o + 2)
        o += 4
        is_mixin = top and t == "Lorg/spongepowered/asm/mixin/Mixin;"
        for _ in range(pairs):
            key = utf(u2(o))
            o = value(o + 2, is_mixin and key in ("value", "targets"))
        return o

    k = u2(o)
    o += 2
    for _ in range(k):
        o = annotation(o, True)
    return out


def main():
    out_path, rest = sys.argv[1], sys.argv[2:]
    split = rest.index("--")
    mod_jars, platform_jars = rest[:split], rest[split + 1:]
    platform = set()
    for j in platform_jars:
        for _, name, _ in classes_in(j, nested=False):
            platform.add(name)
    own = {}
    parsed = []
    for j in mod_jars:
        for label, name, data in classes_in(j):
            own.setdefault(name, label)
            parsed.append((label, name, data))
    members = defaultdict(lambda: defaultdict(set))  # owner -> member -> mods
    mixins = defaultdict(set)  # target -> mods
    errors = 0
    for label, name, data in parsed:
        try:
            refs, _, mixin = parse(data)
        except Exception:
            errors += 1
            continue
        for kind, owner, mname, mdesc in refs:
            if owner in own or owner.startswith("["):
                continue
            members[owner][f"{kind} {mname}{mdesc}"].add(label)
        for t in mixin:
            mixins[t].add(label)

    def area(owner):
        if owner.startswith("net/minecraft/") or owner.startswith("com/mojang/blaze3d/"):
            return "minecraft"
        if owner.startswith("net/neoforged/"):
            return "neoforge"
        if owner.startswith("java/") or owner.startswith("javax/") or owner.startswith("jdk/"):
            return "jdk"
        return "library"

    report = {
        "mod_classes": len(own),
        "parse_errors": errors,
        "owners": {
            o: {
                "area": area(o),
                "in_platform": o in platform,
                "members": {m: sorted(v) for m, v in ms.items()},
            }
            for o, ms in members.items()
        },
        "mixin_targets": {t: sorted(v) for t, v in mixins.items()},
    }
    with open(out_path, "w") as f:
        json.dump(report, f)


if __name__ == "__main__":
    main()
