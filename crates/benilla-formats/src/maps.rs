//! `Map.dbc`: a server `mapId` to its MPQ directory (`0` "Azeroth", `1` "Kalimdor", `36`
//! "DeadminesInstance") for `MapTiles::load`, plus the columns the client reads per map. Records
//! are 125 four-byte fields; `MapName` and the description blocks are 2.4.3 localized blocks.

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{f32_at, parse, str_at, u32_at};

const MAP: &str = "DBFilesClient\\Map.dbc";

/// Field index of `LoadingScreenID`, a `LoadingScreens.dbc` id; 0 on the dev and test maps.
const LOADING_SCREEN_FIELD: usize = 62;

/// `Map.dbc`'s per-map data, keyed by map id.
pub struct MapCatalog {
    dirs: HashMap<u32, String>,
    /// `MapName_Lang[enUS]` (field 4, `+0x10`, read by `0x4a65a0`). The continent dropdown shows
    /// this ("Eastern Kingdoms"), not the art folder ("Azeroth") (`GetMapContinents` `0x4a7ce0`).
    names: HashMap<u32, String>,
    /// `LoadingScreenID`, for maps with a non-zero one.
    loading_screens: HashMap<u32, u32>,
    /// `InstanceType` (field 2): 0 common, 1 party instance, 2 raid, 3 battleground, 4 arena.
    instance_types: HashMap<u32, u32>,
    /// The battleground columns of every row, since the client resolves map 0's too.
    battleground: HashMap<u32, MapBattlegroundColumns>,
}

/// The battleground-facing slice retained for existing callers. In 2.4.3 the queue limits and
/// bracket data moved out of `Map.dbc` into `BattlemasterList.dbc`; those fields stay zero here
/// until that gameplay catalog is ported. The descriptions and minimap scale still come from Map.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MapBattlegroundColumns {
    pub min_level: u32,
    pub max_level: u32,
    pub max_players: u32,
    pub field_16: i32,
    pub field_17: f32,
    pub field_18: f32,
    /// `MapDescription0_lang[enUS]` and `MapDescription1_lang[enUS]`.
    pub descriptions: [String; 2],
    pub bracket_span: u32,
    pub group_queue: u32,
    /// `MinimapIconScale`, field 65 in the 2.4.3 row.
    pub minimap_icon_scale: f32,
}

impl MapBattlegroundColumns {
    /// A wire bracket's `(min, max)` levels (`0x4aa6c0`/`0x4aa850`): with a span,
    /// `min = bracket · span + min_level` and `max = min(span + min − 1, 60)`, else both zero.
    pub fn bracket_levels(&self, bracket: u8) -> (u32, u32) {
        if self.bracket_span == 0 {
            return (0, 0);
        }
        let min = u32::from(bracket) * self.bracket_span + self.min_level;
        (min, (self.bracket_span + min).saturating_sub(1).min(70))
    }
}

impl MapCatalog {
    /// The MPQ directory name (`Directory`) for `map_id`, as `MapTiles::load(chain, dir)` takes it.
    pub fn directory(&self, map_id: u32) -> Option<&str> {
        self.dirs.get(&map_id).map(String::as_str)
    }

    /// The enUS display name (`MapName_Lang`) for `map_id`, "Eastern Kingdoms".
    pub fn name(&self, map_id: u32) -> Option<&str> {
        self.names.get(&map_id).map(String::as_str)
    }

    /// `LoadingScreenID` for `map_id`, to resolve against [`crate::LoadingScreenCatalog`].
    pub fn loading_screen_id(&self, map_id: u32) -> Option<u32> {
        self.loading_screens.get(&map_id).copied()
    }

    /// `InstanceType` for `map_id`. The client treats a missing row as type 0 wherever it asks.
    pub fn instance_type(&self, map_id: u32) -> Option<u32> {
        self.instance_types.get(&map_id).copied()
    }

    /// Whether `map_id` is a party dungeon, the reference's lockout test rather than "is an
    /// instance": `cmp [rec+8],1` gates `SMSG_UPDATE_LAST_INSTANCE` and `CanShowResetInstances`.
    pub fn is_party_dungeon(&self, map_id: u32) -> bool {
        self.instance_type(map_id) == Some(1)
    }

    /// The battleground columns for `map_id`; the client reads map 0's when nothing was listed.
    pub fn battleground(&self, map_id: u32) -> Option<&MapBattlegroundColumns> {
        self.battleground.get(&map_id)
    }

    pub fn len(&self) -> usize {
        self.dirs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }
}

const MAP_NAME_FIELD: usize = 4;

const INSTANCE_TYPE_FIELD: usize = 2;

const DESCRIPTION_0_FIELD: usize = 28;
const DESCRIPTION_1_FIELD: usize = 45;
const MINIMAP_ICON_SCALE_FIELD: usize = 65;

/// The 2.4.3 table is 125 four-byte fields. Localized text occupies 16 string offsets plus a
/// language-mask field; the renderer only consumes the enUS slot, but every field still has to be
/// typed so the generic WDBC parser sees the correct record shape.
fn map_schema() -> Schema {
    let mut s = Schema::new("Map");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    s.add_field(SchemaField::new("Directory", FieldType::String));
    for i in 2..125 {
        let localized = (4..=19).contains(&i)
            || (28..=43).contains(&i)
            || (45..=60).contains(&i)
            || (66..=81).contains(&i)
            || (83..=98).contains(&i)
            || (100..=115).contains(&i);
        let ty = if localized {
            FieldType::String
        } else if matches!(i, 65 | 118 | 119) {
            FieldType::Float32
        } else {
            FieldType::UInt32
        };
        let name = match i {
            INSTANCE_TYPE_FIELD => "InstanceType".to_string(),
            MAP_NAME_FIELD => "MapName".to_string(),
            DESCRIPTION_0_FIELD => "MapDescription0".to_string(),
            DESCRIPTION_1_FIELD => "MapDescription1".to_string(),
            LOADING_SCREEN_FIELD => "LoadingScreenID".to_string(),
            MINIMAP_ICON_SCALE_FIELD => "MinimapIconScale".to_string(),
            _ => format!("_field{i}"),
        };
        s.add_field(SchemaField::new(name, ty));
    }
    s
}

/// Read Map.dbc off the patch chain into a [`MapCatalog`].
pub fn load_map_catalog(chain: &mut Chain) -> Result<MapCatalog> {
    let bytes = chain
        .read_file(MAP)
        .with_context(|| format!("reading {MAP}"))?;
    let rs = parse(&bytes, map_schema(), "Map")?;
    let mut dirs = HashMap::with_capacity(rs.records().len());
    let mut names = HashMap::with_capacity(rs.records().len());
    let mut loading_screens = HashMap::new();
    let mut instance_types = HashMap::with_capacity(rs.records().len());
    let mut battleground = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        battleground.insert(
            id,
            MapBattlegroundColumns {
                field_16: -1,
                descriptions: [
                    str_at(&rs, r, DESCRIPTION_0_FIELD).unwrap_or_default(),
                    str_at(&rs, r, DESCRIPTION_1_FIELD).unwrap_or_default(),
                ],
                minimap_icon_scale: f32_at(r, MINIMAP_ICON_SCALE_FIELD).unwrap_or(1.0),
                ..Default::default()
            },
        );
        if let Some(dir) = str_at(&rs, r, 1) {
            dirs.insert(id, dir);
        }
        if let Some(name) = str_at(&rs, r, MAP_NAME_FIELD).filter(|n| !n.is_empty()) {
            names.insert(id, name);
        }
        // Type 0 is recorded too, so `None` means only that no such map exists.
        if let Some(ty) = u32_at(r, INSTANCE_TYPE_FIELD) {
            instance_types.insert(id, ty);
        }
        if let Some(ls) = u32_at(r, LOADING_SCREEN_FIELD).filter(|&v| v != 0) {
            loading_screens.insert(id, ls);
        }
    }

    Ok(MapCatalog {
        dirs,
        names,
        loading_screens,
        instance_types,
        battleground,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tbc_map_layout_reads_outland_and_map_owned_battleground_fields() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let catalog = load_map_catalog(&mut chain).expect("Map.dbc");

        assert_eq!(catalog.directory(530), Some("Expansion01"));
        assert_eq!(catalog.name(530), Some("Outland"));

        let wsg = catalog.battleground(489).expect("Warsong Gulch");
        assert!(wsg.descriptions[0].starts_with("A valley bordering Ashenvale"));
        assert_eq!(wsg.descriptions[0], wsg.descriptions[1]);
        assert!((wsg.minimap_icon_scale - 1.0).abs() < 1e-6);

        let ab = catalog.battleground(529).expect("Arathi Basin");
        assert!((ab.minimap_icon_scale - 1.25).abs() < 1e-6);
    }
}
