//! The TBC 2.4.3 CharTitles.dbc catalog. The index used by SetCurrentTitle is
//! the final bit_index column, not the DBC row ID. It indexes the 64-bit
//! PLAYER__FIELD_KNOWN_TITLES update field and PLAYER_CHOSEN_TITLE.

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, str_at, u32_at};
use crate::Chain;

/// Localized title templates (male and female where authored), keyed by title bit index.
#[derive(Debug, Clone)]
pub struct CharTitle {
    pub bit_index: u32,
    pub name_male: String,
    pub name_female: String,
}

pub fn load_char_titles(chain: &mut Chain) -> Result<Vec<CharTitle>> {
    const FILE: &str = "DBFilesClient\\CharTitles.dbc";
    let bytes = chain.read_file(FILE).with_context(|| format!("reading {FILE}"))?;
    let mut schema = Schema::new("CharTitles");
    // Stock build 8606: ID, condition, first localized title (16 + flags),
    // second localized title (16 + flags), bit_index = field 36.
    for col in 0..37 {
        schema.add_field(SchemaField::new(
            format!("field{col}"),
            if col == 2 || col == 19 { FieldType::String } else { FieldType::UInt32 },
        ));
    }
    let records = parse(&bytes, schema, "CharTitles.dbc")?;
    let mut out = Vec::with_capacity(records.records().len());
    for row in records.records() {
        let Some(bit_index) = u32_at(row, 36).filter(|id| *id < 64) else { continue };
        let Some(name_male) = str_at(&records, row, 2) else { continue };
        let name_female = str_at(&records, row, 19).unwrap_or_else(|| name_male.clone());
        out.push(CharTitle { bit_index, name_male, name_female });
    }
    out.sort_by_key(|t| t.bit_index);
    out.dedup_by_key(|t| t.bit_index);
    Ok(out)
}
