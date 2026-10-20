//! Words the tools go by to tell what things are and do, from the offsets file's "words" section (`OffsetsFile`), which
//! defaults to these. They're the same for every game; a new game's words can be added to the section of the offsets
//! file next to the exe (`tomb-helper export-offsets`) rather than to the code.

use crate::ui_ipc_data::{TriggerCategory, TRIGGER_ACTIONS};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Words {
    /// What triggers are for, by words in their name or the name of what they're part of, checked in this order:
    /// (category as in the config, words). Most of the game's own names are like "triggerplane_DelayDeath-imill8" or
    /// "bendy_bv_young_pine.triggersphere".
    pub trigger_categories: Vec<(String, Vec<String>)>,
    /// What a trigger's script does, by part of the class names of the nodes it leads to, most important first: (part
    /// of the class name, one of `TRIGGER_ACTIONS`)
    pub script_actions: Vec<(String, String)>,
    /// Instances with an interaction component that's for NPCs and animals rather than Lara: the places they play
    /// animations at ("animhost_npc_human_eatcandybar", ...)
    pub not_interactable: Vec<String>,
    /// What an instance is by its component data's names (lower case), where there's no component table (TR2013):
    /// something Lara can interact with, unless it's her own gear, or a character or animal
    pub interactable_components: Vec<String>,
    pub inventory_components: Vec<String>,
    pub npc_components: Vec<String>,
    /// Left off the names of the instances scripts act on, when they're shown
    /// (`tr11_animhost_friendly_npc_roaming_musician_guitar`)
    pub name_prefixes: Vec<String>,
}

impl Default for Words {
    fn default() -> Self {
        let strings = |words: Vec<&str>| words.into_iter().map(String::from).collect();
        let categories: Vec<(&str, Vec<&str>)> = vec![
            ("objects", vec![
                "bendy", "concealment", "foliage", "bush", "tree", "sapling", "grass", "plant", "collectible",
                "throwable", "campsite", "survivalcache", "looter", "breakable", "balcony", "corpse", "lantern",
            ]),
            ("checkpoints", vec![
                "checkpoint", "ckpt",
            ]),
            ("death", vec![
                "death", "kill", "drown",
            ]),
            ("streaming", vec![
                "streaming", "airlock",
            ]),
            ("combat", vec![
                "combat", "spawn", "npc", "enemy", "ai_", "wolf", "bear", "deer", "animal", "heli", "stealth",
                "alert",
            ]),
            ("camera", vec![
                "camera", "cam_", "customcam", "lookat", "look_at", "headtrack", "interior", "indoor",
            ]),
            ("effects", vec![
                "sound", "music", "audio", "glow", "dust", "wind", "contrast", "fx", "storm", "weather", "light",
                "vo_", "bark", "tp_",
            ]),
            ("hints", vec![
                "tutorial", "notification", "hint", "prompt", "radio", "objective", "missing_gear", "discovery",
                "journal",
            ]),
        ];
        let actions: Vec<(&str, &str)> = vec![
            ("Checkpoint", "checkpoint"),
            ("GameObjective", "objective"),
            ("GlobalProgress", "progress"),
            ("Timeline", "cutscene"),
            ("MovesetProfile", "moveset"),
            ("Camera", "camera"),
            ("Streaming", "streaming"),
            ("StreamStates", "streaming"),
            ("UnitStream", "streaming"),
            ("UnloadUnit", "streaming"),
            ("Teleport", "teleport"),
            ("BattleConductor", "combat"),
            ("Spawn", "spawn"),
            ("GiveBirth", "spawn"),
            ("InstanceDestroy", "despawn"),
            ("FadeOut", "despawn"),
            ("HealthAndDamage", "damage"),
            ("Dialogue", "dialogue"),
            ("Subtitle", "dialogue"),
            ("SoundVocal", "dialogue"),
            ("Tutorial", "tutorial"),
            ("HeadTrack", "look at"),
            ("NPC", "NPC"),
            ("HuntManager", "NPC"),
            ("Inventory", "inventory"),
            ("Ammo", "inventory"),
            ("Campsite", "campsite"),
            ("Lighting", "lighting"),
            ("GraphicsOverride", "lighting"),
            ("Fog", "lighting"),
            ("Weather", "weather"),
            ("Wetness", "weather"),
            ("Wwise", "audio"),
            ("SoundPlex", "audio"),
            ("SoundReverb", "audio"),
            ("SFX", "audio"),
            ("Notification", "UI"),
            ("UI_", "UI"),
            ("PadShock", "rumble"),
            ("PersistentData", "saves state"),
        ];
        Words {
            trigger_categories: categories.into_iter().map(|(c, w)| (c.to_string(), strings(w))).collect(),
            script_actions: actions.into_iter().map(|(p, w)| (p.to_string(), w.to_string())).collect(),
            not_interactable: strings(vec!["animhost"]),
            interactable_components: strings(vec!["interactcomponent", "lootcomponent"]),
            inventory_components: strings(vec!["inventorymembercomponent"]),
            npc_components: strings(vec!["ragdollcomponent"]),
            name_prefixes: strings(vec!["tr11_", "animhost_", "friendly_"]),
        }
    }
}

impl Words {
    pub fn is_default(&self) -> bool {
        *self == Words::default()
    }

    /// Whether every category and action named is a real one
    pub fn validate(&self) -> Result<(), String> {
        for (category, _) in &self.trigger_categories {
            if category_named(category).is_none() {
                let known: Vec<&str> = TriggerCategory::ALL.iter().map(|c| c.name()).collect();
                return Err(format!("words: no trigger category {} (they're {})", category, known.join(", ")));
            }
        }
        for (_, action) in &self.script_actions {
            if !TRIGGER_ACTIONS.contains(&action.as_str()) {
                return Err(format!("words: no trigger action {} (they're {})", action, TRIGGER_ACTIONS.join(", ")));
            }
        }
        Ok(())
    }

    /// The category, by its words, checked in order
    pub fn categories(&self) -> impl Iterator<Item = (TriggerCategory, &Vec<String>)> {
        self.trigger_categories.iter().filter_map(|(name, words)| Some((category_named(name)?, words)))
    }
}

fn category_named(name: &str) -> Option<TriggerCategory> {
    TriggerCategory::ALL.iter().copied().find(|c| c.name() == name)
}

static CURRENT: RwLock<Option<Arc<Words>>> = RwLock::new(None);

/// The words from the offsets file in use (the defaults until one's loaded)
pub fn current() -> Arc<Words> {
    if let Some(words) = CURRENT.read().unwrap().as_ref() {
        return words.clone();
    }
    Arc::new(Words::default())
}

pub fn set(words: Words) {
    *CURRENT.write().unwrap() = Some(Arc::new(words));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_valid_and_only_named_words_are_accepted() {
        assert!(Words::default().validate().is_ok());
        let mut words = Words::default();
        words.script_actions.push(("Nothing".to_string(), "not an action".to_string()));
        assert!(words.validate().is_err());
        let words: Words = serde_json::from_str(r#"{"not_interactable": ["animhost", "spectator"]}"#).unwrap();
        assert_eq!(words.not_interactable.len(), 2);
        assert_eq!(words.script_actions, Words::default().script_actions);
    }
}
