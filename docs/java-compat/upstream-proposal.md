# Draft: request to the Horizon project

STATUS: DRAFT. NOT SENT. Send only after the owner of this fork approves it.

Writing standard: ASD-STE100 rules (see [README.md](README.md#writing-rules-for-these-documents)).

## Subject

A compatibility layer that runs NeoForge mods from their original jars.

## Request

We request that the Horizon project consider a Java mod compatibility system.
The system compiles original NeoForge mod jars into Horizon WASM mods. It uses
one compatibility layer for all mods, a cache keyed to each jar, and a hook
table for Mixins. [README.md](README.md) gives the design.

## Why

- Hand ports do not scale. A large pack has hundreds of code mods.
- Players keep the original mods. The system does not distribute mod code.
- Work on the compatibility layer helps all mods at the same time.
- The hook table lets the community add support over time.

## What we can give

- The design documents in this folder.
- The census tool and the data from the Afterwinter pack.
- The proof of concept (Phase 0), when it is complete.

## If the project does not accept it

We will make the system as a Horizon WASM mod. It will use only the public mod
API. It will not need changes to the Horizon core, if the mod API has the
necessary hooks.
