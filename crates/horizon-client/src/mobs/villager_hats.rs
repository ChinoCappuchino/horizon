//! VillagerProfessionLayer's cached texture metadata and type-layer head visibility.
use super::{anim::Poses, render::VILLAGER_TYPES};
use std::{collections::HashMap, fs, path::Path};

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Hat {
    #[default]
    None,
    Partial,
    Full,
}

#[derive(Default)]
pub struct VillagerHats(HashMap<String, Hat>);

impl VillagerHats {
    pub fn load(root: &Path) -> Self {
        let mut hats = HashMap::new();
        for folder in ["villager", "zombie_villager"] {
            // The type layers, then every profession's (a mod's in its own
            // namespace: `ns:entity/villager/profession/name`).
            let mut textures: Vec<String> = VILLAGER_TYPES
                .iter()
                .map(|t| format!("entity/{folder}/type/{t}"))
                .collect();
            textures.extend(
                (0..crate::professions::count())
                    .map(|p| crate::professions::texture(&format!("entity/{folder}"), p as u8)),
            );
            for texture in textures {
                let (ns, path) = texture.split_once(':').unwrap_or(("minecraft", &texture));
                let path = root.join(format!("assets/{ns}/textures/{path}.png.mcmeta"));
                let metadata = fs::read(path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
                let hat = match metadata
                    .as_ref()
                    .and_then(|v| v["villager"]["hat"].as_str())
                {
                    Some("partial") => Hat::Partial,
                    Some("full") => Hat::Full,
                    _ => Hat::None,
                };
                hats.insert(texture, hat);
            }
        }
        Self(hats)
    }

    /// Hides the head under a profession's hat: `profession` is the
    /// profession layer's texture ([`crate::professions::texture`]).
    pub fn apply(&self, folder: &str, variant: &str, profession: &str, poses: &mut Poses) {
        let hat = |texture: &str| self.0.get(texture).copied().unwrap_or_default();
        let visible = match hat(profession) {
            Hat::None => true,
            Hat::Partial => hat(&format!("{folder}/type/{variant}")) != Hat::Full,
            Hat::Full => false,
        };
        // VillagerModel.createNoHatModel clears the head and every child.
        poses.visible("head", visible);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires prepared 26.3 models and textures in HORIZON_TEST_RESOURCES"]
    fn metadata_head_visibility_matches_exported_no_hat_models() {
        use crate::mobs::{load_model, render::draw};
        let root = std::env::var_os("HORIZON_TEST_RESOURCES").expect("HORIZON_TEST_RESOURCES");
        let mut hats = VillagerHats::load(Path::new(&root));
        assert_eq!(hats.0["entity/villager/profession/farmer"], Hat::Full);
        assert_eq!(hats.0["entity/villager/type/plains"], Hat::None);
        for (name, folder) in [
            ("villager", "entity/villager"),
            ("villager_baby", "entity/villager"),
            ("zombie_villager", "entity/zombie_villager"),
            ("zombie_villager_baby", "entity/zombie_villager"),
        ] {
            let model = load_model(Path::new(&root), &format!("minecraft:{name}#main")).unwrap();
            let no_hat =
                load_model(Path::new(&root), &format!("minecraft:{name}_no_hat#main")).unwrap();
            for (type_hat, profession_hat, hidden) in [
                (Hat::None, Hat::Full, true),
                (Hat::Full, Hat::Partial, true),
                (Hat::Partial, Hat::Partial, false),
                (Hat::Full, Hat::None, false),
            ] {
                hats.0.insert(format!("{folder}/type/plains"), type_hat);
                hats.0
                    .insert(format!("{folder}/profession/farmer"), profession_hat);
                let mut poses = Poses::new(&model);
                hats.apply(folder, "plains", "farmer", &mut poses);
                let mut actual = Vec::new();
                draw(
                    &model,
                    glam::Mat4::IDENTITY,
                    &poses,
                    [0.0, 0.0, 1.0, 1.0],
                    u32::MAX,
                    0,
                    0,
                    &mut actual,
                );
                let expected_model = if hidden { &no_hat } else { &model };
                let mut expected = Vec::new();
                draw(
                    expected_model,
                    glam::Mat4::IDENTITY,
                    &Poses::new(expected_model),
                    [0.0, 0.0, 1.0, 1.0],
                    u32::MAX,
                    0,
                    0,
                    &mut expected,
                );
                assert_eq!(
                    bytemuck::cast_slice::<_, u8>(&actual),
                    bytemuck::cast_slice::<_, u8>(&expected),
                    "{name}: {type_hat:?}/{profession_hat:?}"
                );
            }
        }
    }
}
