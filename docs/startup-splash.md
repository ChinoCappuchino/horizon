# The startup splash (fork `NeuHorizon`)

Before the game window opens, Horizon shows its loading as liquid: each mod that loads is a drop that flies in from a screen edge and merges into an ember
pool, vanilla's snow fox (the game's own model and the player's own texture) laps it, and when the
game is ready the fox pounces and the pool springs into the game window's rectangle, where the
game window then opens. Design: chino, 2026-10-28.

## Settings

- `startupSplash` in Horizon's extras file (`horizon-extras.txt`): `full` (the default), `still`
  (no motion: the pool at its size, the stage line and the bar) or `off`.
- Without mods there is no splash: the game starts too quickly to need one.
- `--no-splash` turns it off for one launch. Benchmarks, harness runs, `--control` and dedicated
  servers never show it (they start synchronously).

## Where it draws

| System | Splash |
|---|---|
| Linux, Wayland (KWin, Hyprland, Mutter...) | transparent and click-through over the whole screen (full screen); the compositor places the game window |
| Linux, X11 with a compositor | the same |
| Windows 10 and 11 | transparent over the screen through DirectX 12 and DirectComposition (`DxgiFromVisual`); Vulkan and plain DirectX swapchains can't be transparent there |
| macOS | transparent over the screen through Metal (a layer that isn't opaque), in the monitor's own rectangle rather than a full-screen Space |
| Anything without a transparent surface (OpenGL only, X11 without a compositor, a driver that refuses) | the same animation in a borderless window the size of the game window, in its place, over the game window's dark face; the pool becomes that very window |

When nothing can open a splash, the game window opens as without one. The splash never holds the
game back: it hands over as soon as the title's artwork and the loaded resources are both there,
after a finale of about two seconds (never before 3 s of splash); drops still to come then hurry.

Checked: Linux (KDE Plasma 6 on Wayland, NVIDIA): both the transparent splash and the fallback
(`HORIZON_SPLASH_OPAQUE=1` forces it). Windows and macOS compile from the same code (the platform
choices are made at run time) but have not been run yet.

## For development

- `HORIZON_SPLASH_TRACE=FILE` writes every frame's inputs; `splash_frames <prepared resources> OUT
  --replay FILE BACKGROUND.png` renders that run again offscreen at 60 frames a second (then
  `ffmpeg -framerate 60 -i OUT/frame-%05d.png ...`).
- `examples/splash_frames.rs` alone renders chosen moments of a simulated loading; `--fox-test`
  draws the fox at four headings.
