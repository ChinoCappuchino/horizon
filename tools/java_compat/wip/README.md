# Work in progress: Java mod compatibility tools

Status: unfinished tools of the NeuHorizon fork (2026-10-31). They are not
part of a pull request. Writing standard: ASD-STE100.

| File | Use | Status |
|---|---|---|
| `prism-wrapper.sh` | Prism Launcher's wrapper command for a Horizon instance: it installs the build, prepares the resources, hands over the player's session, ports the instance's mods and starts the game with the setup splash. | A prototype of the external-launcher handover (#3545). Paths are of the fork's test machine. |
| `api_surface.py` | The Minecraft and NeoForge classes, members and `@Mixin` targets that a set of mod jars uses. It reads constant pools and annotations only. | Works. Used for the Create and Sable plan. |
| `needed_libs.py` | The library jars of Minecraft 1.21.1 and NeoForge 21.1.251, from Prism's metadata, for `jar_bench --native-check`. | Works. The versions are fixed in the file. |

Do not put mod jars, JDK classes or Mojang files next to these tools.
