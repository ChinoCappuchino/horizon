"""The library jars Minecraft 1.21.1 and NeoForge 21.1.251 need, from Prism's metadata."""
import json, os, sys

prism = os.path.expanduser("~/.local/share/PrismLauncher")
metas = [f"{prism}/meta/net.minecraft/1.21.1.json", f"{prism}/meta/net.neoforged/21.1.251.json"]
seen, out = set(), []
for meta in metas:
    data = json.load(open(meta))
    for lib in data.get("libraries", []) + data.get("mavenFiles", []):
        parts = lib["name"].split(":")
        if len(parts) < 3:
            continue
        group, artifact, version = parts[:3]
        classifier = parts[3] if len(parts) > 3 else ""
        if classifier.startswith("natives") or "@" in version:
            continue
        name = f"{artifact}-{version}" + (f"-{classifier}" if classifier else "") + ".jar"
        path = f"{prism}/libraries/{group.replace('.', '/')}/{artifact}/{version}/{name}"
        if path not in seen and os.path.isfile(path):
            seen.add(path)
            out.append(path)
print("\n".join(out))
print(f"{len(out)} libraries", file=sys.stderr)
