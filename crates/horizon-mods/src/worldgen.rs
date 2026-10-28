//! World-generation mods' declarations (fork `afterwinter`,
//! docs/research/worldgen-hooks.md): the manifest's `[worldgen]` section,
//! the claims by which one mod replaces, removes or reserves what others
//! add, the pack author's `config/horizon/worldgen-rules.toml`, and the
//! [`Plan`] that settles them, the same way in any load order.
//!
//! Everything here is declared, so the game can show the report and the
//! pack author can overrule a claim before a single chunk is generated.
use serde::Deserialize;
use std::{cmp::Reverse, collections::BTreeMap, fmt::Write};

/// The pack author's rules, from the game's folder.
pub const RULES: &str = "config/horizon/worldgen-rules.toml";
/// Biome numbers `biomes` answers with to keep what is there.
pub const KEEP_BIOME: u32 = u32::MAX;

/// A stage of generation, in the order the game runs them (WIT
/// `worldgen-events.stage`).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Climate,
    Terrain,
    Surface,
    Structures,
    Features,
}
impl Stage {
    pub const ALL: [Stage; 5] = [
        Stage::Climate,
        Stage::Terrain,
        Stage::Surface,
        Stage::Structures,
        Stage::Features,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Stage::Climate => "climate",
            Stage::Terrain => "terrain",
            Stage::Surface => "surface",
            Stage::Structures => "structures",
            Stage::Features => "features",
        }
    }
    /// What the stage's claims target: biomes, structures, features...
    fn things(self) -> &'static str {
        match self {
            Stage::Climate => "biomes",
            Stage::Terrain => "terrain",
            Stage::Surface => "surface rules and carvers",
            Stage::Structures => "structures",
            Stage::Features => "features",
        }
    }
}

/// What a claim does to what it targets.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Keeps the mod's own targets where it applies, against lower claims.
    Add,
    /// Puts the mod's own `with` in place of other mods' targets.
    Replace,
    /// Takes other mods' targets out.
    Remove,
    /// Keeps a region or biome free of other mods' targets, for the mod's
    /// own content.
    Reserve,
}
impl Action {
    fn name(self) -> &'static str {
        match self {
            Action::Add => "add",
            Action::Replace => "replace",
            Action::Remove => "remove",
            Action::Reserve => "reserve",
        }
    }
}

/// Where a claim applies.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String")]
pub enum Scope {
    /// `*`: everywhere.
    Everywhere,
    /// `region:<name>`: the chunks the mod's code puts in one of its
    /// regions (WIT `worldgen-events.regions`).
    Region(String),
    /// `biome:<id>` or `biome:#<tag>`.
    Biome(String),
    BiomeTag(String),
    /// `preset:<id>`: worlds made with that world preset.
    Preset(String),
}
impl TryFrom<String> for Scope {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let bad = || {
            format!("scope {s:?} is not *, region:<name>, biome:<id>, biome:#<tag> or preset:<id>")
        };
        if s == "*" {
            return Ok(Scope::Everywhere);
        }
        let (kind, rest) = s.split_once(':').ok_or_else(bad)?;
        match kind {
            "region" if is_name(rest) => Ok(Scope::Region(rest.into())),
            "biome" => match rest.strip_prefix('#') {
                Some(tag) if is_id(tag) => Ok(Scope::BiomeTag(tag.into())),
                None if is_id(rest) => Ok(Scope::Biome(rest.into())),
                _ => Err(bad()),
            },
            "preset" if is_id(rest) => Ok(Scope::Preset(rest.into())),
            _ => Err(bad()),
        }
    }
}
impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scope::Everywhere => f.write_str("*"),
            Scope::Region(r) => write!(f, "region:{r}"),
            Scope::Biome(b) => write!(f, "biome:{b}"),
            Scope::BiomeTag(t) => write!(f, "biome:#{t}"),
            Scope::Preset(p) => write!(f, "preset:{p}"),
        }
    }
}

/// What a claim acts on, among the things of its stage.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String")]
pub enum Target {
    /// `*`: every one.
    Any,
    /// `<namespace>:<path>`.
    Id(String),
    /// `#<namespace>:<path>`.
    Tag(String),
    /// `mod:<id>`: everything that mod adds in the stage.
    Mod(String),
}
impl TryFrom<String> for Target {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        if s == "*" {
            return Ok(Target::Any);
        }
        if let Some(m) = s.strip_prefix("mod:")
            && is_name(m)
        {
            return Ok(Target::Mod(m.into()));
        }
        match s.strip_prefix('#') {
            Some(tag) if is_id(tag) => Ok(Target::Tag(tag.into())),
            None if is_id(&s) => Ok(Target::Id(s)),
            _ => Err(format!(
                "target {s:?} is not *, <namespace>:<path>, #<namespace>:<path> or mod:<id>"
            )),
        }
    }
}
impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Any => f.write_str("*"),
            Target::Id(i) => f.write_str(i),
            Target::Tag(t) => write!(f, "#{t}"),
            Target::Mod(m) => write!(f, "mod:{m}"),
        }
    }
}

/// `[a-z0-9_]{1,64}`: a mod id, a claim's or a region's name.
fn is_name(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `namespace:path` as vanilla spells resource ids.
pub(crate) fn is_id(s: &str) -> bool {
    let Some((ns, path)) = s.split_once(':') else {
        return false;
    };
    let ok = |s: &str, extra: &[u8]| {
        !s.is_empty()
            && s.bytes().all(|b| {
                b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b"_.-".contains(&b)
                    || extra.contains(&b)
            })
    };
    ok(ns, b"") && ok(path, b"/")
}

/// One claim of a mod's manifest (`[[worldgen.claims]]`).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    /// `[a-z0-9_]{1,64}`, once per mod: the pack author's rules name the
    /// claim as `<mod>/<name>`.
    pub name: String,
    pub stage: Stage,
    pub action: Action,
    #[serde(default = "everywhere")]
    pub scope: Scope,
    /// What it acts on; `reserve` defaults to `*`.
    #[serde(default)]
    pub targets: Vec<Target>,
    /// `replace` only: the mod's own thing that takes the targets' place.
    #[serde(default)]
    pub with: Option<String>,
    /// Higher wins; at the same priority, the mod id first in sorting
    /// order, never the load order.
    #[serde(default)]
    pub priority: i32,
}
fn everywhere() -> Scope {
    Scope::Everywhere
}

/// A manifest's `[worldgen]` section.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Section {
    /// World presets whose terrain the mod's code provides (`<mod>:<name>`,
    /// in its own namespaces): one provider per preset.
    #[serde(default)]
    pub presets: Vec<String>,
    /// The stages its code takes part in (capability `worldgen`).
    #[serde(default)]
    pub stages: Vec<Stage>,
    /// Names of the regions its code computes, for claims with scope
    /// `region:<name>`.
    #[serde(default)]
    pub regions: Vec<String>,
    #[serde(default)]
    pub claims: Vec<Claim>,
}
impl Section {
    /// Checks the section of mod `id`, whose content owns `namespaces`;
    /// `code` says whether it has capability `worldgen`.
    pub fn check(&self, id: &str, namespaces: &[&str], code: bool) -> Result<(), String> {
        let owned = |s: &str| {
            s.split_once(':')
                .is_some_and(|(ns, _)| namespaces.contains(&ns))
        };
        if !code
            && (!self.presets.is_empty() || !self.stages.is_empty() || !self.regions.is_empty())
        {
            return Err(
                "worldgen presets, stages and regions are the mod's code: they need capability worldgen"
                    .into(),
            );
        }
        if code && self.stages.is_empty() {
            return Err(
                "capability worldgen needs the stages its code takes part in: worldgen.stages"
                    .into(),
            );
        }
        if let Some(p) = self.presets.iter().find(|p| !is_id(p) || !owned(p)) {
            return Err(format!(
                "worldgen preset {p} is not in the mod's own namespaces"
            ));
        }
        if self.stages.contains(&Stage::Terrain) != !self.presets.is_empty() {
            return Err(
                "stage terrain is for the presets the mod provides, and goes with worldgen.presets"
                    .into(),
            );
        }
        for (i, s) in self.stages.iter().enumerate() {
            if self.stages[..i].contains(s) {
                return Err(format!("worldgen stage {} is listed twice", s.name()));
            }
        }
        for (i, r) in self.regions.iter().enumerate() {
            if !is_name(r) {
                return Err(format!("worldgen region {r:?} is not [a-z0-9_]{{1,64}}"));
            }
            if self.regions[..i].contains(r) {
                return Err(format!("worldgen region {r} is listed twice"));
            }
        }
        for (i, c) in self.claims.iter().enumerate() {
            let at = format!("worldgen claim {:?}", c.name);
            if !is_name(&c.name) {
                return Err(format!("{at}: the name is not [a-z0-9_]{{1,64}}"));
            }
            if self.claims[..i].iter().any(|o| o.name == c.name) {
                return Err(format!("{at} is listed twice"));
            }
            if c.stage == Stage::Terrain {
                return Err(format!(
                    "{at}: terrain has one provider per preset (worldgen.presets), not claims"
                ));
            }
            if let Scope::Region(r) = &c.scope
                && !self.regions.contains(r)
            {
                return Err(format!("{at}: region {r} is not in worldgen.regions"));
            }
            if c.targets.is_empty() && c.action != Action::Reserve {
                return Err(format!("{at}: {} needs targets", c.action.name()));
            }
            match (c.action, &c.with) {
                (Action::Replace, None) => return Err(format!("{at}: replace needs with")),
                (Action::Replace, Some(w)) if !is_id(w) || !owned(w) => {
                    return Err(format!("{at}: with {w} is not in the mod's own namespaces"));
                }
                (Action::Replace, Some(_)) | (_, None) => {}
                (_, Some(_)) => return Err(format!("{at}: with is for replace")),
            }
            if c.action == Action::Reserve && c.scope == Scope::Everywhere {
                return Err(format!(
                    "{at}: reserve keeps a region, a biome or a preset free; everywhere is remove"
                ));
            }
            // A mod adds and protects only its own things.
            if c.action == Action::Add
                && let Some(t) = c.targets.iter().find(|t| match t {
                    Target::Mod(m) => m != id,
                    Target::Id(i) => !owned(i),
                    _ => false,
                })
            {
                return Err(format!("{at}: add keeps the mod's own things; {t} isn't"));
            }
            if c.action != Action::Add && c.targets.contains(&Target::Mod(id.into())) {
                return Err(format!("{at}: a mod's claims never act on its own things"));
            }
        }
        Ok(())
    }
}

/// The pack author's rules (`config/horizon/worldgen-rules.toml`): the
/// last word over every mod's claims.
///
/// ```toml
/// [claims."afterwinter_worldgen/settlement_sites"]
/// priority = 50        # instead of the manifest's
/// [claims."t_and_t/villages_everywhere"]
/// allow = false        # the claim is ignored
/// [mods.biomesoplenty]
/// off = ["features"]   # none of the mod's features, by data or code
/// ```
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    #[serde(default)]
    pub claims: BTreeMap<String, ClaimRule>,
    #[serde(default)]
    pub mods: BTreeMap<String, ModRule>,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClaimRule {
    pub priority: Option<i32>,
    pub allow: Option<bool>,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModRule {
    #[serde(default)]
    pub off: Vec<Stage>,
}
impl Rules {
    pub fn parse(text: &str) -> Result<Rules, String> {
        toml::from_str(text).map_err(|e| format!("{RULES}: {e}"))
    }
}

/// A mod that takes part in generation: its id, the namespaces its content
/// owns and its `[worldgen]` section (a mod with only data, none).
#[derive(Clone, Debug)]
pub struct Contributor {
    pub id: String,
    pub namespaces: Vec<String>,
    pub section: Section,
}
impl Contributor {
    pub fn of(manifest: &crate::Manifest) -> Contributor {
        Contributor {
            id: manifest.id.clone(),
            namespaces: manifest.owned_namespaces().map(str::to_owned).collect(),
            section: manifest.worldgen.clone().unwrap_or_default(),
        }
    }
}

/// A claim in force: its mod, and its priority after the pack's rules.
#[derive(Clone, Debug, PartialEq)]
pub struct Effective {
    pub owner: String,
    pub claim: Claim,
    /// The pack's rules changed its priority.
    pub ruled: bool,
}
impl Effective {
    fn label(&self) -> String {
        format!("{}/{}", self.owner, self.claim.name)
    }
    /// The order claims decide in: higher priority, then the mod id and the
    /// claim's name in sorting order.
    fn rank(&self) -> (Reverse<i32>, &str, &str) {
        (Reverse(self.claim.priority), &self.owner, &self.claim.name)
    }
}

/// Where a thing would generate: the world's preset, the biome there and
/// its tags, and the regions each mod's code puts the chunk in.
#[derive(Clone, Copy, Debug)]
pub struct Place<'a> {
    pub preset: &'a str,
    pub biome: &'a str,
    pub biome_tags: &'a [&'a str],
    /// (mod id, region name).
    pub regions: &'a [(&'a str, &'a str)],
}

/// A thing a mod adds in a stage: a biome, a structure, a feature...
#[derive(Clone, Copy, Debug)]
pub struct Candidate<'a> {
    /// The mod that adds it; `minecraft` for vanilla's own.
    pub owner: &'a str,
    pub id: &'a str,
    pub tags: &'a [&'a str],
}

/// What happens to a [`Candidate`].
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// It generates; `by` names the claim that kept it, if one did.
    Keep { by: Option<String> },
    /// A claim took it out.
    Drop { by: String },
    /// A claim put the claimant's `with` in its place.
    Replace { with: String, by: String },
    /// The pack's rules turned its mod off in this stage.
    Off,
}

/// The claims of every mod that takes part, settled with the pack's rules.
#[derive(Clone, Debug)]
pub struct Plan {
    contributors: Vec<Contributor>,
    /// In the order they decide.
    claims: Vec<Effective>,
    denied: Vec<String>,
    off: BTreeMap<String, Vec<Stage>>,
    /// What the report lists under problems: rules naming nothing, claims
    /// that remove what no mod adds.
    problems: Vec<String>,
}

impl Plan {
    /// Settles the claims; the order of `contributors` changes nothing.
    pub fn new(contributors: &[Contributor], rules: &Rules) -> Plan {
        let mut contributors = contributors.to_vec();
        contributors.sort_by(|a, b| a.id.cmp(&b.id));
        let mut problems = Vec::new();
        let mut denied = Vec::new();
        let mut claims = Vec::new();
        for m in &contributors {
            for claim in &m.section.claims {
                let label = format!("{}/{}", m.id, claim.name);
                let rule = rules.claims.get(&label);
                if rule.and_then(|r| r.allow) == Some(false) {
                    denied.push(label);
                    continue;
                }
                let mut claim = claim.clone();
                if claim.targets.is_empty() {
                    claim.targets.push(Target::Any);
                }
                let ruled = match rule.and_then(|r| r.priority) {
                    Some(p) => {
                        claim.priority = p;
                        true
                    }
                    None => false,
                };
                claims.push(Effective {
                    owner: m.id.clone(),
                    claim,
                    ruled,
                });
            }
        }
        claims.sort_by(|a, b| a.rank().cmp(&b.rank()));
        let installed = |id: &str| contributors.iter().any(|m| m.id == id);
        let namespace_known = |id: &str| {
            let ns = id.split_once(':').map_or(id, |(ns, _)| ns);
            ns == "minecraft"
                || ns == "c"
                || contributors
                    .iter()
                    .any(|m| m.namespaces.iter().any(|n| n == ns))
        };
        for c in &claims {
            if c.claim.action == Action::Add {
                continue;
            }
            for t in &c.claim.targets {
                let unknown = match t {
                    Target::Mod(m) => !installed(m),
                    Target::Id(i) => !namespace_known(i),
                    Target::Any | Target::Tag(_) => false,
                };
                if unknown {
                    problems.push(format!(
                        "{} would {} {t}, which no mod here adds",
                        c.label(),
                        c.claim.action.name()
                    ));
                }
            }
        }
        for label in rules.claims.keys() {
            let known = contributors.iter().any(|m| {
                m.section
                    .claims
                    .iter()
                    .any(|c| *label == format!("{}/{}", m.id, c.name))
            });
            if !known {
                problems.push(format!(
                    "{RULES} names claim {label}, which no mod here has"
                ));
            }
        }
        let mut off = BTreeMap::new();
        for (id, rule) in &rules.mods {
            if !installed(id) {
                problems.push(format!("{RULES} names mod {id}, which takes no part here"));
            } else if !rule.off.is_empty() {
                off.insert(id.clone(), rule.off.clone());
            }
        }
        Plan {
            contributors,
            claims,
            denied,
            off,
            problems,
        }
    }

    /// The claims in force, in the order they decide.
    pub fn claims(&self) -> &[Effective] {
        &self.claims
    }
    pub fn problems(&self) -> &[String] {
        &self.problems
    }
    /// Whether the pack's rules turned `id` off in `stage`.
    pub fn is_off(&self, id: &str, stage: Stage) -> bool {
        self.off.get(id).is_some_and(|s| s.contains(&stage))
    }

    /// What happens to `thing` at `place` in `stage`: the first claim, in
    /// [`Plan::claims`]' order, that applies to it decides.
    pub fn decide(&self, stage: Stage, place: &Place, thing: &Candidate) -> Decision {
        if self.is_off(thing.owner, stage) {
            return Decision::Off;
        }
        let first = self
            .claims
            .iter()
            .find(|c| c.claim.stage == stage && applies(c, place, thing));
        let Some(c) = first else {
            return Decision::Keep { by: None };
        };
        let by = c.label();
        match c.claim.action {
            Action::Add => Decision::Keep { by: Some(by) },
            Action::Remove | Action::Reserve => Decision::Drop { by },
            Action::Replace => Decision::Replace {
                with: c.claim.with.clone().unwrap_or_default(),
                by,
            },
        }
    }

    /// Pairs of claims of different mods that can act on the same thing in
    /// the same place, with the one that wins: (winner, loser).
    pub fn conflicts(&self) -> Vec<(&Effective, &Effective)> {
        let mut out = Vec::new();
        for (i, a) in self.claims.iter().enumerate() {
            for b in &self.claims[i + 1..] {
                let same = a.claim.stage == b.claim.stage && a.owner != b.owner;
                if same && may_overlap(&a.claim.scope, &b.claim.scope) && (hits(a, b) || hits(b, a))
                {
                    out.push((a, b));
                }
            }
        }
        out
    }

    /// The world-generation report's declared half (the counts of what each
    /// claim took out come with generation): who takes part in which stage,
    /// the claims in the order they decide, the pack's rules, the conflicts
    /// and the problems.
    pub fn report(&self) -> String {
        let mut out = String::from("# World-generation report\n\n## Mods\n\n");
        let w = &mut out;
        let list = |v: &[String]| {
            if v.is_empty() {
                "-".to_string()
            } else {
                v.join(", ")
            }
        };
        writeln!(
            w,
            "| Mod | Terrain for presets | Stages (code) | Regions | Claims |"
        )
        .unwrap();
        writeln!(w, "| --- | --- | --- | --- | ---: |").unwrap();
        for m in &self.contributors {
            let stages: Vec<String> = m
                .section
                .stages
                .iter()
                .map(|s| s.name().to_string())
                .collect();
            writeln!(
                w,
                "| {} | {} | {} | {} | {} |",
                m.id,
                list(&m.section.presets),
                list(&stages),
                list(&m.section.regions),
                m.section.claims.len()
            )
            .unwrap();
        }
        writeln!(w, "\n## Claims, in the order they decide\n").unwrap();
        if self.claims.is_empty() {
            writeln!(w, "None: every mod adds, nothing is taken out.").unwrap();
        } else {
            writeln!(w, "| Claim | Stage | Action | Where | Targets | Priority |").unwrap();
            writeln!(w, "| --- | --- | --- | --- | --- | ---: |").unwrap();
            for c in &self.claims {
                let targets: Vec<String> = c.claim.targets.iter().map(|t| t.to_string()).collect();
                let action = match &c.claim.with {
                    Some(with) => format!("replace with {with}"),
                    None => c.claim.action.name().to_string(),
                };
                let ruled = if c.ruled { " (pack rules)" } else { "" };
                writeln!(
                    w,
                    "| {} | {} | {action} | {} | {} | {}{ruled} |",
                    c.label(),
                    c.claim.stage.name(),
                    c.claim.scope,
                    targets.join(", "),
                    c.claim.priority
                )
                .unwrap();
            }
        }
        let mut rules: Vec<String> = self
            .denied
            .iter()
            .map(|d| format!("claim {d} is denied"))
            .collect();
        for (id, stages) in &self.off {
            for s in stages {
                rules.push(format!(
                    "{id} adds no {} ({} stage off)",
                    s.things(),
                    s.name()
                ));
            }
        }
        if !rules.is_empty() {
            writeln!(w, "\n## Pack rules ({RULES})\n").unwrap();
            for r in rules {
                writeln!(w, "- {r}").unwrap();
            }
        }
        let conflicts = self.conflicts();
        if !conflicts.is_empty() {
            writeln!(w, "\n## Conflicts\n").unwrap();
            for (win, lose) in conflicts {
                let why = if win.claim.priority > lose.claim.priority {
                    format!(
                        "priority {} over {}",
                        win.claim.priority, lose.claim.priority
                    )
                } else {
                    format!(
                        "same priority {}; {} sorts first",
                        win.claim.priority, win.owner
                    )
                };
                writeln!(
                    w,
                    "- {} wins over {} where both apply ({why})",
                    win.label(),
                    lose.label()
                )
                .unwrap();
            }
        }
        if !self.problems.is_empty() {
            writeln!(w, "\n## Problems\n").unwrap();
            for p in &self.problems {
                writeln!(w, "- {p}").unwrap();
            }
        }
        out
    }
}

/// Whether claim `c` acts on `thing` at `place`.
fn applies(c: &Effective, place: &Place, thing: &Candidate) -> bool {
    let own = thing.owner == c.owner;
    // Add keeps the mod's own things; the others act on everyone else's.
    if own != (c.claim.action == Action::Add) {
        return false;
    }
    let here = match &c.claim.scope {
        Scope::Everywhere => true,
        Scope::Region(r) => place
            .regions
            .iter()
            .any(|&(m, name)| m == c.owner && name == r),
        Scope::Biome(b) => place.biome == b,
        Scope::BiomeTag(t) => place.biome_tags.contains(&t.as_str()),
        Scope::Preset(p) => place.preset == p,
    };
    here && c.claim.targets.iter().any(|t| match t {
        Target::Any => true,
        Target::Id(i) => thing.id == i,
        Target::Tag(tag) => thing.tags.contains(&tag.as_str()),
        Target::Mod(m) => thing.owner == m,
    })
}

/// Whether `a` may act on something `b`'s mod adds and `b` covers: an
/// exclusive claim aimed at `b`'s mod against `b`'s own add, or two
/// exclusive claims aimed at each other's things.
fn hits(a: &Effective, b: &Effective) -> bool {
    if a.claim.action == Action::Add {
        return false;
    }
    a.claim.targets.iter().any(|t| match t {
        Target::Any | Target::Tag(_) => true,
        Target::Mod(m) => *m == b.owner,
        Target::Id(i) => b.claim.targets.iter().any(|bt| match bt {
            Target::Id(j) => i == j,
            _ => true,
        }),
    }) && (b.claim.action == Action::Add
        || b.claim.targets.iter().any(|t| match t {
            Target::Mod(m) => *m == a.owner,
            _ => true,
        }))
}

/// Whether two scopes can cover the same chunk. Regions are the mods' code,
/// and biomes and tags only the world knows: those may.
fn may_overlap(a: &Scope, b: &Scope) -> bool {
    match (a, b) {
        (Scope::Biome(x), Scope::Biome(y)) | (Scope::Preset(x), Scope::Preset(y)) => x == y,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(text: &str) -> Section {
        toml::from_str(text).unwrap()
    }
    fn farview() -> Contributor {
        Contributor {
            id: "afterwinter_worldgen".into(),
            namespaces: vec!["afterwinter_worldgen".into()],
            section: section(
                r##"
presets = ["afterwinter_worldgen:earth"]
stages = ["climate", "terrain", "structures", "features"]
regions = ["settlements", "rails"]
[[claims]]
name = "settlement_sites"
stage = "structures"
action = "reserve"
scope = "region:settlements"
priority = 10
[[claims]]
name = "own_forests"
stage = "features"
action = "replace"
scope = "biome:#minecraft:is_forest"
targets = ["minecraft:trees_birch_and_oak"]
with = "afterwinter_worldgen:winter_trees"
"##,
            ),
        }
    }
    fn towns() -> Contributor {
        Contributor {
            id: "t_and_t".into(),
            namespaces: vec!["t_and_t".into(), "towns_and_towers".into()],
            section: section(
                r##"
[[claims]]
name = "villages"
stage = "structures"
action = "add"
targets = ["#towns_and_towers:villages"]
priority = 5
"##,
            ),
        }
    }
    fn place<'a>(regions: &'a [(&'a str, &'a str)], tags: &'a [&'a str]) -> Place<'a> {
        Place {
            preset: "afterwinter_worldgen:earth",
            biome: "minecraft:forest",
            biome_tags: tags,
            regions,
        }
    }

    #[test]
    fn sections_are_checked() {
        let ns = ["m"];
        let check = |text: &str, code: bool| section(text).check("m", &ns, code);
        check("stages = [\"features\"]", true).unwrap();
        assert!(
            check("stages = [\"features\"]", false)
                .unwrap_err()
                .contains("capability worldgen")
        );
        assert!(check("", true).unwrap_err().contains("worldgen.stages"));
        assert!(
            check("stages = [\"terrain\"]", true)
                .unwrap_err()
                .contains("presets")
        );
        assert!(
            check("presets = [\"other:p\"]\nstages = [\"terrain\"]", true)
                .unwrap_err()
                .contains("own namespaces")
        );
        let claim = |body: &str| {
            check(
                &format!(
                    "stages = [\"features\"]\nregions = [\"r\"]\n[[claims]]\nname = \"c\"\n{body}"
                ),
                true,
            )
        };
        claim("stage = \"features\"\naction = \"remove\"\ntargets = [\"mod:other\"]").unwrap();
        claim("stage = \"features\"\naction = \"reserve\"\nscope = \"region:r\"").unwrap();
        for (body, why) in [
            (
                "stage = \"terrain\"\naction = \"remove\"\ntargets = [\"*\"]",
                "one provider per preset",
            ),
            ("stage = \"features\"\naction = \"remove\"", "needs targets"),
            (
                "stage = \"features\"\naction = \"reserve\"",
                "everywhere is remove",
            ),
            (
                "stage = \"features\"\naction = \"reserve\"\nscope = \"region:x\"",
                "not in worldgen.regions",
            ),
            (
                "stage = \"features\"\naction = \"replace\"\ntargets = [\"*\"]",
                "replace needs with",
            ),
            (
                "stage = \"features\"\naction = \"replace\"\ntargets = [\"*\"]\nwith = \"other:x\"",
                "own namespaces",
            ),
            (
                "stage = \"features\"\naction = \"remove\"\ntargets = [\"*\"]\nwith = \"m:x\"",
                "with is for replace",
            ),
            (
                "stage = \"features\"\naction = \"add\"\ntargets = [\"mod:other\"]",
                "add keeps the mod's own",
            ),
            (
                "stage = \"features\"\naction = \"remove\"\ntargets = [\"mod:m\"]",
                "its own things",
            ),
        ] {
            let e = claim(body).unwrap_err();
            assert!(e.contains(why), "{body}: {e}");
        }
        for bad in [
            "scope = \"near:spawn\"",
            "targets = [\"Oak\"]",
            "targets = [\"#bad\"]",
        ] {
            let text = format!(
                "[[claims]]\nname = \"c\"\nstage = \"features\"\naction = \"remove\"\n{bad}"
            );
            assert!(toml::from_str::<Section>(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn claims_decide_by_priority_then_mod_id_never_load_order() {
        let a = Plan::new(&[farview(), towns()], &Rules::default());
        let b = Plan::new(&[towns(), farview()], &Rules::default());
        assert_eq!(a.claims(), b.claims());
        assert_eq!(a.report(), b.report());

        let village = Candidate {
            owner: "t_and_t",
            id: "towns_and_towers:village_rustic",
            tags: &["towns_and_towers:villages"],
        };
        let tower = Candidate {
            owner: "t_and_t",
            id: "towns_and_towers:tower",
            tags: &[],
        };
        let in_site = [("afterwinter_worldgen", "settlements")];
        // Farview's reserve (10) beats Towns and Towers' add (5) in its sites.
        let by = Some("afterwinter_worldgen/settlement_sites".to_string());
        for thing in [&village, &tower] {
            assert_eq!(
                a.decide(Stage::Structures, &place(&in_site, &[]), thing),
                Decision::Drop {
                    by: by.clone().unwrap()
                }
            );
        }
        // Outside them the village is kept by its own claim, the tower by default.
        assert_eq!(
            a.decide(Stage::Structures, &place(&[], &[]), &village),
            Decision::Keep {
                by: Some("t_and_t/villages".into())
            }
        );
        assert_eq!(
            a.decide(Stage::Structures, &place(&[], &[]), &tower),
            Decision::Keep { by: None }
        );
        // A region of another mod with the same name is not Farview's.
        assert_eq!(
            a.decide(
                Stage::Structures,
                &place(&[("t_and_t", "settlements")], &[]),
                &tower
            ),
            Decision::Keep { by: None }
        );
        // Farview's own structures are never taken out by its reserve.
        let own = Candidate {
            owner: "afterwinter_worldgen",
            id: "afterwinter_worldgen:hamlet",
            tags: &[],
        };
        assert_eq!(
            a.decide(Stage::Structures, &place(&in_site, &[]), &own),
            Decision::Keep { by: None }
        );
        // Replace, in a forest only.
        let oak = Candidate {
            owner: "minecraft",
            id: "minecraft:trees_birch_and_oak",
            tags: &[],
        };
        assert_eq!(
            a.decide(Stage::Features, &place(&[], &["minecraft:is_forest"]), &oak),
            Decision::Replace {
                with: "afterwinter_worldgen:winter_trees".into(),
                by: "afterwinter_worldgen/own_forests".into()
            }
        );
        assert_eq!(
            a.decide(Stage::Features, &place(&[], &[]), &oak),
            Decision::Keep { by: None }
        );
        // Same priority: the mod id first in sorting order wins, whoever loads first.
        let mut low = farview();
        low.section.claims[0].priority = 5;
        for order in [[low.clone(), towns()], [towns(), low.clone()]] {
            let p = Plan::new(&order, &Rules::default());
            assert_eq!(
                p.decide(Stage::Structures, &place(&in_site, &[]), &village),
                Decision::Drop {
                    by: by.clone().unwrap()
                },
                "afterwinter_worldgen sorts before t_and_t"
            );
        }
    }

    #[test]
    fn the_pack_author_has_the_last_word() {
        let rules = Rules::parse(
            r##"
[claims."t_and_t/villages"]
priority = 20
[claims."afterwinter_worldgen/own_forests"]
allow = false
[mods.t_and_t]
off = ["features"]
[mods.ghost]
off = ["structures"]
[claims."ghost/x"]
priority = 1
"##,
        )
        .unwrap();
        let plan = Plan::new(&[farview(), towns()], &rules);
        let village = Candidate {
            owner: "t_and_t",
            id: "towns_and_towers:village_rustic",
            tags: &["towns_and_towers:villages"],
        };
        let in_site = [("afterwinter_worldgen", "settlements")];
        assert_eq!(
            plan.decide(Stage::Structures, &place(&in_site, &[]), &village),
            Decision::Keep {
                by: Some("t_and_t/villages".into())
            }
        );
        let oak = Candidate {
            owner: "minecraft",
            id: "minecraft:trees_birch_and_oak",
            tags: &[],
        };
        assert_eq!(
            plan.decide(Stage::Features, &place(&[], &["minecraft:is_forest"]), &oak),
            Decision::Keep { by: None }
        );
        let well = Candidate {
            owner: "t_and_t",
            id: "towns_and_towers:well",
            tags: &[],
        };
        assert_eq!(
            plan.decide(Stage::Features, &place(&[], &[]), &well),
            Decision::Off
        );
        assert_eq!(plan.problems().len(), 2, "{:?}", plan.problems());
        let report = plan.report();
        for line in [
            "| t_and_t/villages | structures | add | * | #towns_and_towers:villages | 20 (pack rules) |",
            "- claim afterwinter_worldgen/own_forests is denied",
            "- t_and_t adds no features (features stage off)",
            "- t_and_t/villages wins over afterwinter_worldgen/settlement_sites where both apply (priority 20 over 10)",
            "names mod ghost, which takes no part here",
            "names claim ghost/x, which no mod here has",
        ] {
            assert!(report.contains(line), "missing {line:?} in\n{report}");
        }
        assert!(Rules::parse("[mods.x]\noff = [\"weather\"]").is_err());
        assert!(Rules::parse("[claims.\"a/b\"]\nprio = 1").is_err());
    }

    #[test]
    fn removing_what_no_mod_adds_is_a_problem() {
        let mut lone = farview();
        lone.section.claims.push(Claim {
            name: "no_villages".into(),
            stage: Stage::Structures,
            action: Action::Remove,
            scope: Scope::Everywhere,
            targets: vec![
                Target::Mod("t_and_t".into()),
                Target::Id("yungs:dungeon".into()),
            ],
            with: None,
            priority: 0,
        });
        let plan = Plan::new(&[lone], &Rules::default());
        assert_eq!(
            plan.problems(),
            [
                "afterwinter_worldgen/no_villages would remove mod:t_and_t, which no mod here adds",
                "afterwinter_worldgen/no_villages would remove yungs:dungeon, which no mod here adds",
            ]
        );
    }
}
