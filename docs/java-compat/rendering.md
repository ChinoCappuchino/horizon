# Java mod compatibility: rendering

Status: accepted (research council and owner, 2026-10-31).

## 1. Problem

Mods built for 1.21.1 draw through four kinds of interface:

1. Vanilla's 1.21.1 render API: `PoseStack`, `VertexConsumer`,
   `MultiBufferSource`, `RenderType`, `BlockEntityRenderer`, `ModelPart`,
   `BakedModel`.
2. Flywheel, an instancing library (Create and its addons).
3. Veil, a library for framebuffers, post-processing, shaders and lights
   (Sable, Aeronautics).
4. Raw OpenGL through LWJGL. The Create/Sable set has about 787 call sites.

Horizon renders vanilla natively with wgpu, on 26.3's model. It does not run
vanilla's Java render code. Mod geometry must still go through Horizon's
culling, its vertex arena and the active shader pack, or it breaks under
shader packs the way Flywheel does under Iris.

## 2. Options studied

| Option | Verdict | Effort (person-weeks) |
|---|---|---|
| A. Emulate OpenGL on wgpu | General, but the wrong layer for most mod code; wgpu is a subset of GL (no geometry shaders, no transform feedback, no persistent mapping); pipeline-cache stutter risk; no GL-on-wgpu prior art | 45–65 |
| B. Reimplement each API (vanilla, Flywheel, Veil) in Rust | Native speed, but N separate ports, each needing its own culling, batching and shader-pack wiring | 25–40 |
| C. Shader pipeline | Needed by every option: naga's GLSL frontend cannot take mod GLSL as it is | 12–17 |
| D. One render-intent IR with thin adapters | Recommended | see section 5 |

## 3. Recommended architecture

The narrow waist is render intent: geometry, a material, a transform and a
lifetime. Each mod-facing API is an adapter that lowers into one Horizon IR.
Only Horizon's backend touches wgpu.

```
 mod code (compiled natively)
 ├─ vanilla 1.21.1 render API ──┐
 ├─ Flywheel API ───────────────┤ adapters, one per API and version,
 ├─ Veil (JSON + GLSL) ─────────┤ inside the version bridge (compiler.md 4.5)
 └─ sub-levels / contraptions ──┘
                ▼
 ┌──────────── render-intent IR ────────────┐
 │ StaticMesh(id, material)  cached         │
 │ Instance(mesh, transform, params)        │
 │ SectionGroup(sections, model matrix)     │
 │ Immediate(quads, material)  last resort  │
 │ GraphNode(post / framebuffer pass)       │
 │ Material: RenderType → shader-pack stage │
 └──────────────────────────────────────────┘
                ▼
 Horizon backend: arena, indirect draws, culling, shader packs → wgpu
                ▲
 mod GLSL → preprocessor → glslang (Vulkan, relaxed) → SPIR-V
          → sampler split → naga → WGSL/backend
```

Reasons:

- Vanilla moved to the same model in 1.21.9: extract a render state, then
  submit nodes for deferred, batched drawing. Translating 1.21.1's immediate
  calls into 26.3 is therefore a recording problem by nature.
- One IR gives every adapter the same culling, batching, translucency order
  and shader-pack path. Generality comes from the IR, not from the number of
  APIs ported.
- Horizon already has the sink: entity and block-entity vertex runs with a
  material key in `Frame::entities`, and terrain in one arena drawn with
  indirect draws.

### 3.1 Automatic de-animation

The compiler owns `PoseStack` and `VertexConsumer`, so the recorder sees each
vertex in local space with its current pose matrix. It cuts the stream at
every pose change and hashes each run's local vertices. A run whose hash
stays the same across frames becomes a `StaticMesh` uploaded once, and after
that only its matrix is sent. Rigid moving parts (cogwheels, shafts, fans,
chests) thus become GPU instancing with no work per mod. Runs that really
deform (fluids, text, procedural geometry) stay `Immediate`. A run whose
cache hit rate stays low falls back to `Immediate`.

### 3.2 Sub-levels and contraptions

Sable keeps a sub-level's blocks in a "plot" region of ordinary chunk
storage, with a logical pose, a last pose and a render pose (public
sable-companion API). Create contraptions are block assemblies too. Both
become `SectionGroup`s: Horizon's own mesher builds their sections, and each
group is drawn with one model matrix. Terrain shaders, shader packs and LOD
then apply with no mod render code involved. Open issues: oriented bounds for
culling, translucency sorting per group, and plot-local light.

### 3.3 Shaders

naga's GLSL frontend accepts only `#version 440/450/460`. It requires
`layout(binding=)` on every resource, has no combined `sampler2D` and no
atomics. `horizon-shaders` works around this for Iris packs by rewriting
source text into GLSL 450, but that rewrite is tied to the Iris program
model. For mod shaders:

1. Take the final source after the mod's own composition (Flywheel's
   stitching, Veil's processors).
2. Normalize it with `horizon-shaders` pieces: compatibility built-ins, and
   the `gl_InstanceID` base-instance difference.
3. Compile with glslang to SPIR-V: Vulkan target, relaxed rules
   (`GL_EXT_vulkan_glsl_relaxed`), automatic binding and location mapping.
4. Split combined samplers, then naga `spv-in`, then wgpu.
5. Keep a reflection table per program (uniform offsets, sampler and block
   bindings, attribute locations). It emulates GL's runtime binding calls.
6. Cache at install, keyed by a hash of the source and its defines.
7. A failing shader disables only its own effect, with a diagnostic and a
   folder for override shaders.

The hardest part is not compiling. It is GL's runtime binding model, which
Veil and Flywheel use dynamically.

### 3.4 Raw OpenGL

(Owner decision 1 in section 6: GL calls are an adapter of the IR.)
GL emulation is not the main path. A scoped layer can follow later as an
escape hatch for raw-GL code: GL 3.3 core plus selected 4.x, LWJGL classes
as runtime natives, a Gallium-style state tracker and the shader pipeline
of 3.3. A NeoForge 1.21.1 GLES port reports Create, Aeronautics, Veil and
Flywheel working with about 365 real GL functions, which bounds the
work. Until it exists, `RenderSystem` and GL calls are recorded as
`Immediate`/`GraphNode` where they can be, and skipped with a report
where they can't.

## 4. Flywheel and Veil

- Flywheel's public concepts: Visual, Visualizer, Instancer, InstanceType,
  Material, Model, render origin, Plans, VisualEmbedding. These map onto
  `Instance`, `StaticMesh` and `SectionGroup`. Flywheel reportedly has an
  "off" backend, under which Create falls back to block-entity renderers.
  If running it confirms that, the recorder plus de-animation renders Create
  with no Flywheel work. A native Flywheel adapter comes only if a benchmark
  says it is needed. Its shader prelude is written from the prose API
  documentation only.
- Veil: JSON framebuffers and post pipelines become render-graph nodes, its
  dynamic buffers become extra outputs of the world pass, and its GLSL goes
  through 3.3. Deferred lights come later.

## 5. Milestones

| # | Milestone | Done when |
|---|---|---|
| 1 | Recorder: 1.21.1 `PoseStack`/`VertexConsumer`/`MultiBufferSource`, RenderType → material table, `Immediate` into `Frame::entities` | a simple mod block-entity renderer draws in game |
| 2 | Create with Flywheel off, through the recorder | a cogwheel turns |
| 3 | De-animation cache | 1,000 cogwheels, measured against Horizon's native block-entity renderers |
| 4 | Contraptions and Sable sub-levels as `SectionGroup` | a moving contraption, then a Sable ship |
| 5 | Shader pipeline (glslang → SPIR-V → naga), cache, diagnostics | the mods' shaders compile at install |
| 6 | Veil subset (framebuffers, post, dynamic buffers) | Sable's and Aeronautics' effects |
| 7 | Native Flywheel adapter | only if milestone 3 is too slow |
| 8 | Shader-pack parity for every IR material | mod geometry correct under an Iris pack |
| 9 | Scoped GL layer | raw-GL mods (minimaps, HUDs) |

## 6. Owner decisions (2026-10-31)

1. Raw OpenGL is one more adapter of the IR, not a separate path. The GL
   calls of all mods lower into the same IR. The IR is then the single
   source of truth for what is drawn, and Horizon's backend batches and
   orders the work of all mods together. GL state that the IR cannot
   express goes through `GraphNode`. This replaces the separate GL layer
   of 3.4 and milestone 9.
2. Flywheel: native is the goal. The order of the work depends on what the
   milestones show. The fallback path with de-animation (milestones 2 and
   3) is a step on the way.
3. Mod materials target both Iris `gbuffers` stages and Horizon's own pack
   stages. A material is data passed to the active pack. Resource packs can
   define material types (PBR, surface roughness and others), and the
   shaders handle them.
4. The IR is the rendering target of every version step of the bridge,
   where this is not too complex.
5. glslang is included. An Android build can leave it out or use a
   replacement.

## 7. Clean room

This document comes from public documentation, wikis, release notes and
Horizon's own code. The RenderType → material table must come from
documentation and from observed output, not from vanilla source. Shader
preludes come from prose API documentation, not from shader files in the
mods' repositories or jars.
