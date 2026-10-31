//! The options of a world-generation mod's preset (fork `NeuHorizon`,
//! docs/research/worldgen-hooks.md, M3c), over Create New World as the
//! superflat editor is: one row per `[[worldgen.settings]]` entry of the
//! mod's manifest, a cycling button for choices and a slider for numbers.
//! Done keeps them as the preset's settings JSON, which the world saves and
//! hands to the mod; it never changes afterwards.
use super::{Button, Title};
use crate::{
    hud::{HudAtlas, HudBatch},
    lang,
    text::Text,
    ui,
};
use serde_json::{Map, Value};
use std::path::Path;

/// The options' bundle in the resources (`mod_content::PRESET_SETTINGS`).
const BUNDLE: &str = "worldgen/preset_settings.json";
/// Rows: the first's top, the step between them, their width.
const TOP: f32 = 40.0;
const ROW: f32 = 24.0;
const WIDTH: f32 = 310.0;
/// Button ids: Done, Cancel, then one per choice row.
pub(super) const DONE: u8 = 78;
pub(super) const CANCEL: u8 = 79;
pub(super) const FIRST_ROW: u8 = 80;

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub preset: String,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    path: String,
    label: String,
    kind: Kind,
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Choice {
        choices: Vec<String>,
        at: usize,
    },
    Number {
        lo: f64,
        hi: f64,
        step: Option<f64>,
        value: f64,
    },
}

/// The bundle's entries for `preset`, if it has any.
fn entries(resources: &Path, preset: &str) -> Option<Vec<Value>> {
    let bundle: Map<String, Value> =
        serde_json::from_slice(&std::fs::read(resources.join(BUNDLE)).ok()?).ok()?;
    bundle
        .get(preset)?
        .as_array()
        .cloned()
        .filter(|e| !e.is_empty())
}

/// Whether `preset` has options to set.
pub(super) fn has(resources: &Path, preset: &str) -> bool {
    entries(resources, preset).is_some()
}

/// The value at a dotted path of a JSON object.
fn at<'a>(json: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(json, |v, key| v.get(key))
}

impl State {
    /// The preset's options, set as `current` (its settings JSON, "" for the
    /// defaults) has them.
    pub(super) fn load(resources: &Path, preset: &str, current: &str) -> Option<State> {
        let current: Value = serde_json::from_str(current).unwrap_or(Value::Null);
        let entries = entries(resources, preset)?
            .iter()
            .filter_map(|e| {
                let path = e["path"].as_str()?.to_owned();
                let label = e["label"].as_str()?.to_owned();
                let set = at(&current, &path);
                let kind = match e["choices"].as_array().filter(|c| !c.is_empty()) {
                    Some(choices) => {
                        let choices: Vec<String> = choices
                            .iter()
                            .filter_map(|c| c.as_str().map(str::to_owned))
                            .collect();
                        let want = set.or(Some(&e["default"])).and_then(Value::as_str);
                        let at = want
                            .and_then(|w| choices.iter().position(|c| c == w))
                            .unwrap_or(0);
                        Kind::Choice { choices, at }
                    }
                    None => {
                        let range = e["range"].as_array()?;
                        let (lo, hi) = (range.first()?.as_f64()?, range.get(1)?.as_f64()?);
                        let value = set
                            .and_then(Value::as_f64)
                            .or(e["default"].as_f64())
                            .unwrap_or(lo)
                            .clamp(lo, hi);
                        Kind::Number {
                            lo,
                            hi,
                            step: e["step"].as_f64(),
                            value,
                        }
                    }
                };
                Some(Entry { path, label, kind })
            })
            .collect();
        Some(State {
            preset: preset.to_owned(),
            entries,
        })
    }

    /// The settings JSON: every option at its path.
    pub fn json(&self) -> String {
        let mut root = Value::Object(Map::new());
        for e in &self.entries {
            let value = match &e.kind {
                Kind::Choice { choices, at } => Value::from(choices[*at].clone()),
                Kind::Number {
                    step: Some(s),
                    value,
                    ..
                } if s.fract() == 0.0 => Value::from(*value as i64),
                Kind::Number { value, .. } => Value::from(*value),
            };
            let mut keys = e.path.split('.').peekable();
            let mut node = &mut root;
            while let Some(key) = keys.next() {
                let object = node.as_object_mut().expect("objects along the path");
                if keys.peek().is_none() {
                    object.insert(key.to_owned(), value);
                    break;
                }
                node = object
                    .entry(key.to_owned())
                    .or_insert_with(|| Value::Object(Map::new()));
            }
        }
        root.to_string()
    }

    /// A press on a choice row: its next choice.
    pub(super) fn press(&mut self, id: u8) {
        if let Some(Entry {
            kind: Kind::Choice { choices, at },
            ..
        }) = self.entries.get_mut(id.wrapping_sub(FIRST_ROW) as usize)
        {
            *at = (*at + 1) % choices.len();
        }
    }

    /// A click at `(x, y)`: sets the number row's slider under it.
    pub(super) fn click(&mut self, (x, y): (f32, f32), w: f32) -> bool {
        let left = (w / 2.0).floor() - WIDTH / 2.0;
        for (i, e) in self.entries.iter_mut().enumerate() {
            let top = TOP + i as f32 * ROW;
            let inside = (left..left + WIDTH).contains(&x) && (top..top + 20.0).contains(&y);
            if let (
                true,
                Kind::Number {
                    lo,
                    hi,
                    step,
                    value,
                },
            ) = (inside, &mut e.kind)
            {
                let t = f64::from(ui::slider_value(left, WIDTH, x));
                let mut v = *lo + (*hi - *lo) * t;
                if let Some(s) = step.filter(|s| *s > 0.0) {
                    v = *lo + ((v - *lo) / s).round() * s;
                }
                *value = v.clamp(*lo, *hi);
                return true;
            }
        }
        false
    }
}

/// A number as the slider shows it: whole for whole steps, else two places.
fn number(value: f64, step: Option<f64>) -> String {
    match step {
        Some(s) if s.fract() == 0.0 => format!("{}", value as i64),
        _ => format!("{value:.2}"),
    }
}

/// The choice rows and Done and Cancel; number rows are sliders ([`draw`]).
pub(super) fn buttons(w: f32, h: f32, state: &State) -> Vec<Button> {
    let left = (w / 2.0).floor() - WIDTH / 2.0;
    let mut out: Vec<Button> = state
        .entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.kind {
            Kind::Choice { choices, at } => Some(Button {
                x: left,
                y: TOP + i as f32 * ROW,
                w: WIDTH,
                label: format!("{}: {}", e.label, choices[*at]),
                enabled: true,
                id: FIRST_ROW + i as u8,
                icon: None,
            }),
            Kind::Number { .. } => None,
        })
        .collect();
    let footer = h - 28.0;
    for (x, key, fallback, id) in [
        ((w / 2.0).floor() - 155.0, "gui.done", "Done", DONE),
        ((w / 2.0).floor() + 5.0, "gui.cancel", "Cancel", CANCEL),
    ] {
        out.push(Button {
            x,
            y: footer,
            w: 150.0,
            label: lang::get_or(key, fallback),
            enabled: true,
            id,
            icon: None,
        });
    }
    out
}

pub(super) fn draw(
    _title: &Title,
    atlas: &HudAtlas,
    batch: &mut HudBatch,
    cursor: (f32, f32),
    state: &State,
) {
    let (w, h) = (batch.width.ceil(), batch.height.ceil());
    ui::menu_background(batch, atlas, 0.0, 0.0, w, h, false);
    let heading = format!(
        "{}: {}",
        lang::get_or("selectWorld.customizeType", "Customize"),
        super::create_world::preset_label(&state.preset)
    );
    super::centered(atlas, batch, &heading, 15.0, u32::MAX);
    let left = (w / 2.0).floor() - WIDTH / 2.0;
    for (i, e) in state.entries.iter().enumerate() {
        if let Kind::Number {
            lo,
            hi,
            step,
            value,
        } = &e.kind
        {
            let top = TOP + i as f32 * ROW;
            let hover =
                (left..left + WIDTH).contains(&cursor.0) && (top..top + 20.0).contains(&cursor.1);
            let label = Text::literal(format!("{}: {}", e.label, number(*value, *step)));
            let t = ((value - lo) / (hi - lo).max(f64::MIN_POSITIVE)) as f32;
            ui::slider(batch, atlas, left, top, WIDTH, &label, t, hover, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_load_set_and_write_back_as_nested_json() {
        let dir = std::env::temp_dir().join(format!("horizon-preset-page-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("worldgen")).unwrap();
        let bundle = serde_json::json!({"terra:earth": [
            {"path": "climate.mode", "label": "Climate", "choices": ["latitude", "planet"], "range": null, "step": null, "default": "latitude"},
            {"path": "climate.band_width", "label": "Band", "choices": [], "range": [4096.0, 1048576.0], "step": 1024.0, "default": 65536},
            {"path": "strengths.world_scale", "label": "World scale", "choices": [], "range": [0.5, 2.0], "step": null, "default": 1.0}
        ]});
        std::fs::write(dir.join(BUNDLE), bundle.to_string()).unwrap();
        assert!(has(&dir, "terra:earth") && !has(&dir, "minecraft:normal"));
        let mut s = State::load(&dir, "terra:earth", r#"{"climate":{"mode":"planet"}}"#).unwrap();
        let json: Value = serde_json::from_str(&s.json()).unwrap();
        assert_eq!(json["climate"]["mode"], "planet");
        assert_eq!(json["climate"]["band_width"], 65536);
        assert_eq!(json["strengths"]["world_scale"], 1.0);
        s.press(FIRST_ROW);
        // The band slider's far right, rounded to its step.
        let w = 800.0;
        let left = (w / 2.0_f32).floor() - WIDTH / 2.0;
        assert!(s.click((left + WIDTH - 1.0, TOP + ROW + 5.0), w));
        let json: Value = serde_json::from_str(&s.json()).unwrap();
        assert_eq!(json["climate"]["mode"], "latitude");
        let band = json["climate"]["band_width"].as_i64().unwrap();
        assert!(band % 1024 == 0 && band > 1_000_000, "{band}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
