//! The sandbox and the 1.12 stdlib layer on top of stock Lua 5.1.
//!
//! - [`sandbox`]: no filesystem, OS or native reach (`io`, `os`, `package`, `require`, `dofile`,
//!   `loadfile`, `load`, `module`, `newproxy` and `debug` removed), the reference's `debugstack`,
//!   and a text-only `loadstring`.
//! - [`install`]: the 1.12 bare globals (string, table and math aliases, the `debug*` family, the
//!   error-handler pair), `time` and `date`, the clock and zone getters ([`super::clock`],
//!   [`super::zone_text`]), and a `string.format` that accepts positional `%N$` specs as the 1.12
//!   client's does, rewriting them into a sequential format. Mixing positional and sequential
//!   specs raises, as in the reference, whose single argument cursor a positional spec desyncs.

use mlua::{Lua, Value, Variadic};

/// An mlua error's bare Lua message, as `loadstring` returns it: without the `syntax error: ` or
/// `runtime error: ` word mlua's `Display` adds and the reference never does.
pub(super) fn lua_message(e: &mlua::Error) -> String {
    match e {
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::RuntimeError(m) => m.clone(),
        other => other.to_string(),
    }
}

/// Sandbox the VM: no filesystem, OS or native reach, the reference's `debugstack`, and text-only
/// chunk loading.
pub(super) fn sandbox(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // Set to nil explicitly, whatever mlua's safe stdlib already leaves out.
    for name in [
        "io", "os", "package", "require", "dofile", "loadfile", "load", "module", "newproxy",
        "debug",
    ] {
        g.set(name, Value::Nil)?;
    }

    // debugstack([start [, count1 [, count2]]]): the reference's traceback, frames only, in Lua
    // 5.0's wording. There is no `stack traceback:` header and line 1 is the caller's frame:
    // `AceDB-2.0.lua:742` skips one line to read its caller's addon folder, and `AceLibrary.lua:70`
    // reads line 1 as a frame. `AceLibrary.lua:139` needs 5.0's ``in function `name'`` quoting,
    // not later Luas' `'name'`. `start` defaults to 1, the caller; `count1`/`count2` are 5.0's
    // `LEVELS1`/`LEVELS2`, which `FuBarPlugin-2.0.lua:752` relies on with `debugstack(6, 1, 0)`.
    g.set(
        "debugstack",
        lua.create_function(|lua, args: Variadic<Value>| {
            let num = |v: Option<&Value>, default: usize| match v {
                Some(Value::Integer(i)) => usize::try_from(*i).unwrap_or(0),
                Some(Value::Number(n)) if *n >= 0.0 => *n as usize,
                _ => default,
            };
            let start = num(args.first(), 1);
            let count1 = num(args.get(1), 12);
            let count2 = num(args.get(2), 10);
            Ok(traceback_frames(lua, start, count1, count2))
        })?,
    )?;

    // Text-only `loadstring`: a binary chunk is rejected. The BOM strip and `#`-line skip apply,
    // as in the reference they live inside `luaL_loadbuffer`/`luaX_setinput`.
    let loadstring = lua.create_function(
        |lua, (src, chunkname): (mlua::String, Option<mlua::String>)| {
            let raw = src.as_bytes();
            let bytes = crate::source::chunk(&raw).to_vec();
            // The chunk name is the caller's string verbatim, defaulting to the source itself
            // (`luaB_loadstring` `0x703280`, `0x70329a`), which `luaO_chunkid` `0x6f5c40` renders
            // as `[string "…"]`. It comes from the raw bytes: `luaL_loadbuffer` `0x6f5690` strips
            // a BOM only after `0x703296` took the name.
            let name = match &chunkname {
                Some(n) => String::from_utf8_lossy(&n.as_bytes()).into_owned(),
                None => String::from_utf8_lossy(&raw).into_owned(),
            };
            let chunk = lua
                .load(bytes)
                .set_name(name)
                .set_mode(mlua::ChunkMode::Text);
            match chunk.into_function() {
                Ok(f) => Ok((Value::Function(f), Value::Nil)),
                // The failure leg returns nil and Lua's own message, unprefixed (`0x7032c6`,
                // `0x7032d2`).
                Err(e) => Ok((
                    Value::Nil,
                    Value::String(lua.create_string(lua_message(&e))?),
                )),
            }
        },
    )?;
    g.set("loadstring", loadstring)?;

    Ok(())
}

/// Install the stdlib layer: the bare globals, `time`/`date` and the positional `format`.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    install_error_handler(lua)?;
    install_secure_calls(lua)?;
    install_tbc_baseline_apis(lua)?;
    install_time(lua)?;
    // Before the Lua layer, whose `debugprofile*` pair keeps its own `GetTime`.
    super::clock::install(lua)?;
    super::zone_text::install(lua)?;
    lua.load(WOW_STDLIB)
        .set_name("=[benilla wow stdlib]")
        .set_mode(mlua::ChunkMode::Text)
        .exec()?;
    Ok(())
}

/// Live spell bar state pushed by the host from server packets.
#[derive(Clone, Debug)]
pub struct TimedSpellView {
    pub name: String,
    pub rank: String,
    pub icon: Option<String>,
    pub start_ms: f64,
    pub end_ms: f64,
}

/// The state of one elemental totem slot.
#[derive(Clone, Debug)]
pub struct TotemView {
    pub name: String,
    pub icon: Option<String>,
    pub start_secs: f64,
    pub duration_secs: f64,
}

impl super::UiScript {
    pub fn set_tbc_casts(
        &mut self,
        casts: std::collections::HashMap<u64, TimedSpellView>,
        channels: std::collections::HashMap<u64, TimedSpellView>,
    ) {
        let mut model = self.model_mut();
        model.tbc_casts = casts;
        model.tbc_channels = channels;
    }

    /// Server-authoritative PvP currency, combat ratings, and arena membership.
    /// Install the 2.4.3 CharTitles.dbc index for GetTitleName and GetNumTitles.
    pub fn set_tbc_title_names(&mut self, rows: Vec<(u32, String, String)>) {
        self.model_mut().tbc_title_names = rows.into_iter()
            .map(|(id, male, female)| (id, (male, female))).collect();
    }

    pub fn set_tbc_titles(&mut self, known: u64, current: u32) {
        let mut model = self.model_mut();
        model.tbc_known_titles = known;
        model.tbc_current_title = current;
    }

    pub fn take_tbc_title_requests(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().tbc_title_requests)
    }

    pub fn take_tbc_dungeon_change_requests(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.model_mut().tbc_dungeon_change_requests)
    }

    pub fn set_tbc_dungeon_difficulty(&mut self, wire_difficulty: u8) {
        self.model_mut().tbc_dungeon_difficulty = u32::from(wire_difficulty.min(1)) + 1;
    }

    pub fn set_tbc_pvp_fields(&mut self, currencies: [u32; 2], ratings: [u32; 24], teams: [[u32; 6]; 3]) {
        let mut model = self.model_mut();
        model.tbc_pvp_currencies = currencies;
        model.tbc_combat_ratings = ratings;
        model.tbc_arena_teams = teams;
    }

    pub fn set_tbc_totems(&mut self, totems: [Option<TotemView>; 4]) {
        self.model_mut().tbc_totems = totems;
    }

    pub fn take_tbc_destroy_totem_requests(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.model_mut().totem_destroy_requests)
    }
}

fn spell_info(lua: &Lua, unit: String, channel: bool) -> mlua::Result<mlua::MultiValue> {
    let view = {
        let model = lua.app_data_ref::<super::Model>().expect("model app_data");
        let guid = model.unit_guids.guid_of(&unit)?;
        let map = if channel { &model.tbc_channels } else { &model.tbc_casts };
        guid.and_then(|g| map.get(&g))
            .filter(|s| s.end_ms > model.now * 1000.0)
            .cloned()
    };
    let Some(v) = view else { return Ok(mlua::MultiValue::new()) };
    let mut args = vec![
        Value::String(lua.create_string(&v.name)?),
        Value::String(lua.create_string(&v.rank)?),
        Value::String(lua.create_string(&v.name)?),
    ];
    args.push(match v.icon {
        Some(s) => Value::String(lua.create_string(&s)?),
        None => Value::Nil,
    });
    args.extend([
        Value::Number(v.start_ms),
        Value::Number(v.end_ms),
        Value::Boolean(false), // not a tradeskill
    ]);
    if !channel {
        args.push(Value::Integer(0)); // no client-side cast sequence attached
    }
    args.push(Value::Boolean(false)); // interruptible
    Ok(mlua::MultiValue::from_vec(args))
}

/// Server-owned mirror timer state. The local session clock interpolates progress
/// between START packets exactly as the client's 2.4.3 timer getter does.
#[derive(Clone, Debug)]
pub(crate) struct MirrorTimerView {
    pub name: String,
    pub remaining_ms: u32,
    pub duration_ms: u32,
    pub scale: i32,
    pub paused: bool,
    pub label: String,
    pub since_secs: f64,
}

impl MirrorTimerView {
    fn progress(&self, now: f64) -> f64 {
        let elapsed_ms = if self.paused {
            0.0
        } else {
            ((now - self.since_secs) * 1000.0).max(0.0) * f64::from(self.scale)
        };
        (f64::from(self.remaining_ms) + elapsed_ms)
            .clamp(0.0, f64::from(self.duration_ms))
    }
}

impl super::UiScript {
    /// Feed a TBC mirror timer from SMSG_START/STOP_MIRROR_TIMER before firing the UI event.
    pub fn set_tbc_mirror_timer(
        &mut self,
        kind: usize,
        data: Option<(String, u32, u32, i32, bool, String)>,
    ) {
        let mut model = self.model_mut();
        let since_secs = model.now;
        if let Some(slot) = model.tbc_mirror_timers.get_mut(kind) {
            *slot = data.map(|(name, remaining_ms, duration_ms, scale, paused, label)| {
                MirrorTimerView { name, remaining_ms, duration_ms, scale, paused, label, since_secs }
            });
        }
    }

    pub fn pause_tbc_mirror_timer(&mut self, kind: usize, paused: bool) {
        let mut model = self.model_mut();
        let now = model.now;
        if let Some(Some(timer)) = model.tbc_mirror_timers.get_mut(kind) {
            timer.remaining_ms = timer.progress(now).round() as u32;
            timer.since_secs = now;
            timer.paused = paused;
        }
    }
}

/// TBC engine APIs: stateful entries read the host's spell/totem feeds.
fn install_tbc_baseline_apis(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    // These identifiers are referenced by stock TBC XML but absent from the
    // extracted enUS GlobalStrings.lua. Seed localized-text fallbacks only.
    g.set("HONOR_THISWEEK", "This Week")?;
    g.set("ARTICLE_COUNT", "Articles")?;
    g.set("ERROR", "Error")?;
    g.set("GetAccountExpansionLevel", lua.create_function(|_, ()| Ok(1u32))?)?;
    g.set("GetNumVoiceSessions", lua.create_function(|_, ()| Ok(0u32))?)?;
    g.set("GetCurrentTitle", lua.create_function(|lua, ()| {
        let bit = lua.app_data_ref::<super::Model>().expect("model").tbc_current_title;
        Ok(if bit == 0 { -1i64 } else { i64::from(bit) })
    })?)?;
    g.set("GetNumTitles", lua.create_function(|lua, ()| {
        Ok(lua.app_data_ref::<super::Model>().expect("model").tbc_title_names
            .keys().copied().max().unwrap_or(0))
    })?)?;
    g.set("GetTitleName", lua.create_function(|lua, bit: u32| {
        let model = lua.app_data_ref::<super::Model>().expect("model");
        let female = model.unit("player").is_some_and(|u| u.sex == 3);
        let name = model.tbc_title_names.get(&bit)
            .map(|(male, female_name)| if female { female_name } else { male });
        match name {
            Some(value) => Ok(mlua::MultiValue::from_vec(vec![
                Value::String(lua.create_string(value)?), Value::Boolean(true),
            ])),
            None => Ok(mlua::MultiValue::new()),
        }
    })?)?;
    g.set("IsTitleKnown", lua.create_function(|lua, bit: u32| {
        let model = lua.app_data_ref::<super::Model>().expect("model");
        Ok(bit > 0 && bit < 64 && model.tbc_title_names.contains_key(&bit)
            && model.tbc_known_titles & (1u64 << bit) != 0)
    })?)?;
    g.set("SetCurrentTitle", lua.create_function(|lua, bit: i32| {
        let mut model = lua.app_data_mut::<super::Model>().expect("model");
        // Invalid and unearned titles clear the displayed title (same as the stock API).
        let title = if bit > 0 && bit < 64 && model.tbc_known_titles & (1u64 << bit) != 0 {
            bit as u32
        } else { 0 };
        model.tbc_title_requests.push(title);
        Ok(())
    })?)?;
    g.set("GetSummonFriendCooldown",
        lua.create_function(|_, ()| Ok((0u32, 0u32)))?)?;
    g.set("UnitChannelInfo",
        lua.create_function(|lua, unit: String| spell_info(lua, unit, true))?)?;
    g.set("UnitCastingInfo",
        lua.create_function(|lua, unit: String| spell_info(lua, unit, false))?)?;
    g.set("GetTotemInfo",
        lua.create_function(|lua, slot: u32| {
            let model = lua.app_data_ref::<super::Model>().expect("model app_data");
            let view = usize::try_from(slot).ok()
                .and_then(|s| s.checked_sub(1))
                .and_then(|s| model.tbc_totems.get(s))
                .and_then(Option::as_ref)
                .filter(|t| t.duration_secs <= 0.0
                    || t.start_secs + t.duration_secs > model.now);
            Ok(match view {
                Some(t) => (true, Some(t.name.clone()), t.start_secs,
                            t.duration_secs, t.icon.clone()),
                None => (false, None, 0.0, 0.0, None),
            })
        })?)?;
    g.set("DestroyTotem",
        lua.create_function(|lua, slot: u8| {
            if (1..=4).contains(&slot) {
                let mut model = lua.app_data_mut::<super::Model>().expect("model app_data");
                model.totem_destroy_requests.push(slot - 1);
            }
            Ok(())
        })?)?;
    g.set("GetMirrorTimerInfo", lua.create_function(|lua, index: u32| {
        let model = lua.app_data_ref::<super::Model>().expect("model app_data");
        let timer = usize::try_from(index).ok().and_then(|i| i.checked_sub(1))
            .and_then(|i| model.tbc_mirror_timers.get(i)).and_then(Option::as_ref);
        let Some(timer) = timer else {
            return Ok(mlua::MultiValue::from_vec(vec![
                Value::String(lua.create_string("UNKNOWN")?),
                Value::Integer(0), Value::Integer(0), Value::Integer(0),
                Value::Integer(0), Value::String(lua.create_string("")?),
            ]));
        };
        Ok(mlua::MultiValue::from_vec(vec![
            Value::String(lua.create_string(&timer.name)?),
            Value::Number(timer.progress(model.now)),
            Value::Integer(i64::from(timer.duration_ms)),
            Value::Integer(i64::from(timer.scale)),
            Value::Integer(i64::from(timer.paused)),
            Value::String(lua.create_string(&timer.label)?),
        ]))
    })?)?;
    g.set("UnitInBattleground",
        lua.create_function(|_, _unit: Option<String>| Ok(None::<u32>))?)?;
    g.set("IsPossessBarVisible", lua.create_function(|_, ()| Ok(false))?)?;
    // Neutral values for TBC-only stock FrameXML entry points whose server-backed
    // features are not implemented yet. These do not simulate successful actions.
    for name in ["CanSummonFriend", "IsVoiceChatEnabled", "UnitIsTalking",
        "UnitIsPossessed"] {
        g.set(name, lua.create_function(|_, _: Variadic<Value>| Ok(false))?)?;
    }
    for name in ["GetArenaTeam", "GetVoiceSessionInfo",
        "GetPossessInfo"] {
        g.set(name, lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?)?;
    }
    for name in ["GetVoiceCurrentSessionID",
        "GetSelectedDisplayChannel", "Sound_ChatSystem_GetNumInputDrivers",
        "VoiceEnumerateOutputDevices"] {
        g.set(name, lua.create_function(|_, _: Variadic<Value>| Ok(0u32))?)?;
    }
    for name in ["ClearLFMAutofill", "ClearLFGAutojoin",
        "CombatLogResetFilter"] {
        g.set(name, lua.create_function(|_, _: Variadic<Value>| Ok(()))?)?;
    }
    // TBC PaperDollFrame stat tooltips: AP gained per supplied strength/agility.
    // Source: Player::UpdateAttackPowerAndDamage (cMaNGOS TBC StatSystem.cpp).
    g.set("GetAttackPowerForStat", lua.create_function(|lua, (stat, value): (u32, f64)| {
        let model = lua.app_data_ref::<super::Model>().expect("model");
        let class = model.unit("player").and_then(|u| u.class_file.as_deref()).unwrap_or("");
        let per_point = match (stat, class) {
            (1, "WARRIOR" | "PALADIN" | "SHAMAN" | "DRUID") => 2.0,
            (1, "ROGUE" | "HUNTER" | "MAGE" | "PRIEST" | "WARLOCK") => 1.0,
            (2, "ROGUE" | "HUNTER") => 1.0,
            _ => 0.0,
        };
        Ok(value * per_point)
    })?)?;
    g.set("GetHonorCurrency", lua.create_function(|lua, ()| {
        Ok(lua.app_data_ref::<super::Model>().expect("model").tbc_pvp_currencies[0])
    })?)?;
    g.set("GetArenaCurrency", lua.create_function(|lua, ()| {
        Ok(lua.app_data_ref::<super::Model>().expect("model").tbc_pvp_currencies[1])
    })?)?;
    g.set("GetCombatRating", lua.create_function(|lua, kind: u32| {
        let model = lua.app_data_ref::<super::Model>().expect("model");
        Ok(usize::try_from(kind).ok().and_then(|i| i.checked_sub(1))
            .and_then(|i| model.tbc_combat_ratings.get(i)).copied().unwrap_or(0))
    })?)?;
    g.set("GetArenaTeamId", lua.create_function(|lua, slot: u32| {
        let model = lua.app_data_ref::<super::Model>().expect("model");
        Ok(usize::try_from(slot).ok().and_then(|i| i.checked_sub(1))
            .and_then(|i| model.tbc_arena_teams.get(i)).map_or(0, |team| team[0]))
    })?)?;
    g.set("GetArenaPersonalRating", lua.create_function(|lua, slot: u32| {
        let model = lua.app_data_ref::<super::Model>().expect("model");
        Ok(usize::try_from(slot).ok().and_then(|i| i.checked_sub(1))
            .and_then(|i| model.tbc_arena_teams.get(i)).map_or(0, |team| team[5]))
    })?)?;
    g.set("CombatLog_Object_IsA", lua.create_function(|_, (flags, mask): (u32, u32)| {
        Ok(flags & mask != 0)
    })?)?;
    // An unimplemented LFG catalogue must not claim a bogus numeric category.
    g.set("GetLFGTypes", lua.create_function(|_, ()| Ok(mlua::MultiValue::new()))?)?;
    g.set("GetMirrorTimerProgress", lua.create_function(|lua, name: String| {
        let model = lua.app_data_ref::<super::Model>().expect("model app_data");
        Ok(model.tbc_mirror_timers.iter().flatten()
            .find(|t| t.name == name)
            .map_or(0.0, |t| t.progress(model.now)))
    })?)?;
    g.set("GetCurrentDungeonDifficulty", lua.create_function(|lua, ()| {
        Ok(lua.app_data_ref::<super::Model>().expect("model").tbc_dungeon_difficulty)
    })?)?;
    // TBC Lua difficulty indexes 1=normal, 2=heroic; the opcode writes 0 or 1.
    g.set("SetDungeonDifficulty", lua.create_function(|lua, mode: u32| {
        if (1..=2).contains(&mode) {
            lua.app_data_mut::<super::Model>().expect("model")
                .tbc_dungeon_change_requests.push((mode - 1) as u8);
        }
        Ok(())
    })?)?;
    g.set("Sound_GameSystem_GetNumOutputDrivers", lua.create_function(|_, ()| Ok(1u32))?)?;
    g.set("Sound_GameSystem_GetOutputDriverName", lua.create_function(|_, _: Variadic<Value>| Ok("Default Audio Device"))?)?;
    g.set("GetExistingLocales", lua.create_function(|_, ()| Ok("enUS"))?)?;
    g.set("SetAutoLootDefault", lua.create_function(|lua, on: Value| {
        let setter: mlua::Function = lua.globals().get("SetCVar")?;
        let enabled = !matches!(on, Value::Nil | Value::Boolean(false) | Value::Integer(0));
        setter.call::<()>( ("autoLootDefault", if enabled {"1"} else {"0"}) )
    })?)?;
    Ok(())
}

/// TBC secure-call entry points. In a VM without taint tracking, first-party
/// code is initially secure. These calls still use protected Lua calls so an
/// error in the callee does not abort the caller's FrameXML initialization.
fn install_secure_calls(lua: &Lua) -> mlua::Result<()> {
    lua.globals().set(
        "issecure",
        lua.create_function(|_, ()| Ok(true))?,
    )?;
    lua.globals().set(
        "securecall",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(target) = args.first() else {
                return Ok(mlua::MultiValue::new());
            };
            let func = match target {
                Value::Function(f) => f.clone(),
                Value::String(name) => match lua.globals().get::<Value>(name.to_str()?.to_string())? {
                    Value::Function(f) => f,
                    _ => return Ok(mlua::MultiValue::new()),
                },
                _ => return Ok(mlua::MultiValue::new()),
            };
            let params = mlua::MultiValue::from_vec(args.iter().skip(1).cloned().collect());
            match func.call::<mlua::MultiValue>(params) {
                Ok(values) => Ok(values),
                Err(e) => {
                    lua.app_data_mut::<super::Model>()
                        .expect("model")
                        .record_script_error(format!("securecall: {e}"));
                    Ok(mlua::MultiValue::new())
                }
            }
        })?,
    )?;
    Ok(())
}

/// The handler `seterrorhandler` holds, `None` until one is given.
pub(super) fn error_handler(lua: &Lua) -> Option<mlua::Function> {
    lua.named_registry_value::<Option<mlua::Function>>(super::REG_ERRORHANDLER)
        .ok()
        .flatten()
}

/// `seterrorhandler(f)` / `geterrorhandler()`, engine globals in 1.12 (`0x702900`, `0x702950`)
/// over one registry reference, `[0x8722cc]`. It starts at -1 (`0x7039e0`), so `geterrorhandler()`
/// answers nil until something is set; in the running client FrameXML sets `_ERRORMESSAGE` first
/// (`BasicControls.xml:16`), so every addon finds that. `seterrorhandler` takes a function alone
/// and raises its `Usage:` on anything else, nil included, so the slot can never be cleared.
fn install_error_handler(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    g.set(
        "seterrorhandler",
        lua.create_function(|lua, f: Value| match f {
            Value::Function(f) => lua.set_named_registry_value(super::REG_ERRORHANDLER, f),
            _ => Err(mlua::Error::RuntimeError(
                "Usage: seterrorhandler(errfunc)".into(),
            )),
        })?,
    )?;
    g.set(
        "geterrorhandler",
        lua.create_function(|lua, ()| Ok(error_handler(lua)))?,
    )?;
    Ok(())
}

/// The stdlib layer in Lua, run once at construction.
const WOW_STDLIB: &str = r##"
-- ── the bare string family ─────────────────────────────────────────────────────────────────────
-- Every name here is present in the REAL 1.12 client's global table — the in-world `_G` captured
-- from the running reference client (19,572 entries), which is the
-- authority this file answers to.
--
-- `strmatch`, `strrev`, `gmatch`, `strlenutf8` and `strcmputf8i` come from Blizzard's *Era*
-- enumeration, which a Classic Era addon may reach for. **None of them exist in
-- 1.12** — `string.match`/`gmatch` are Lua 5.1 additions the 5.0 client never had — and Era is
-- not our target. They stay out; adding a global the client does not have is how an addon that
-- feature-detects gets sent down a path we cannot honour.
strlen  = string.len
strsub  = string.sub
strupper = string.upper
strlower = string.lower
strrep  = string.rep
strfind = string.find
strbyte = string.byte
strchar = string.char
gsub    = string.gsub
tinsert = table.insert
tremove = table.remove
-- TBC uses Lua 5.1 and exposes its string and table helpers.
strmatch = string.match
strrev = string.reverse
gmatch = string.gmatch
getn = table.getn
sort = table.sort
foreach = table.foreach
foreachi = table.foreachi

function strtrim(text)
    return (string.gsub(text, "^%s*(.-)%s*$", "%1"))
end

function strsplit(delimiter, text)
    if not delimiter or delimiter == "" then return text end
    local parts = {}
    local start = 1
    local i = 1
    while true do
        local first, last = string.find(text, delimiter, start, true)
        if not first then
            parts[i] = string.sub(text, start)
            break
        end
        parts[i] = string.sub(text, start, first - 1)
        i = i + 1
        start = last + 1
    end
    return unpack(parts, 1, i)
end

function strjoin(delimiter, ...)
    local parts = {...}
    for i = 1, select("#", ...) do
        parts[i] = tostring(parts[i])
    end
    return table.concat(parts, delimiter)
end

function strconcat(...)
    return strjoin("", ...)
end

-- ── the bare math aliases (the same 1.12 global family; the reference FrameXML uses them
-- unqualified — WorldMapFrame's overlay pool calls ceil/mod, CooldownFrame floor, …). `mod` is
-- fmod under a 5.0-era name (stock 5.1 dropped math.mod).
ceil  = math.ceil
floor = math.floor
abs   = math.abs
max   = math.max
min   = math.min
sqrt  = math.sqrt
mod   = math.fmod
random = math.random
-- `randomseed` is an ENGINE global in 1.12 exactly like `random` beside it (the captured `_G` types
-- both `function engine`), and it is the half that was missing: `IgniteStatus` calls it at file
-- scope in its OnLoad and dies on `attempt to call global 'randomseed'`. Seeding a PRNG is the one
-- thing an addon does that has no FrameXML counterpart to copy, so the bare name is all it has.
randomseed = math.randomseed
-- `PI` is an ENGINE global in 1.12, not a FrameXML one: the reference reads it bare (UIParent.lua's
-- `elapsedTime * 2 * PI * ROTATIONS_PER_SECOND`, TabardFrame.lua l.61-68) and no shipped FrameXML
-- file ever assigns it (grepped over the whole 1.12 extraction) — so the client supplies it, here,
-- beside the rest of the bare math family.
PI = math.pi
-- The 1.12 bare trig globals are DEGREE-based (the reference CombatText's fountain scroll calls
-- cos/sin with degree arguments); rad/deg ride along as the same family.
function sin(d) return math.sin(math.rad(d)) end
function cos(d) return math.cos(math.rad(d)) end
rad = math.rad
deg = math.deg
-- The rest of the bare math family, each verified present in the real 1.12 client's global
-- table (the captured `_G`).
-- `tan` and the inverses follow sin/cos into DEGREES — same family, same convention; that is
-- consistent-with the verified sin/cos finding rather than separately byte-verified, and it is
-- the convention every addon rotation helper assumes (`atan2` returning degrees is why
-- `atan2(dy, dx)` feeds a texture rotation directly).
function tan(d) return math.tan(math.rad(d)) end
function asin(x) return math.deg(math.asin(x)) end
function acos(x) return math.deg(math.acos(x)) end
function atan(x) return math.deg(math.atan(x)) end
function atan2(y, x) return math.deg(math.atan2(y, x)) end
exp = math.exp
log = math.log
log10 = math.log10
frexp = math.frexp
ldexp = math.ldexp

-- ── the debug* family: six verified STUBS and two real ones ────────────────────────────────────
--
-- All eight, read at the bytes (`0x7027e0`..`0x702860`):
-- `debuginfo`, `debugload`, `debugprint`, `debugdump`, `debugbreak` and `debugtimestamp` are
-- **byte-identical `xor eax,eax; ret` stubs** — three bytes, no `call`, no memory write. They
-- cannot print, log, write or set a global, and they return ZERO Lua values. So these are not
-- no-ops we invented under a real name (an "absent capability"); they are the
-- reference's own no-ops, transcribed.
--
-- That is what lets `BasicControls.xml` stop guarding its `debuginfo()` call and transcribe
-- `_ERRORMESSAGE` verbatim, which it now does.
--
-- `debugprofilestart`/`debugprofilestop` are the two that are REAL (RDTSC via `0x4293d0`): start
-- latches, stop answers the elapsed milliseconds since it. Modelled on `GetTime`'s own clock —
-- the same monotonic session seconds the tick advances — because benilla has no cycle counter and
-- an addon uses these to time its own work, which milliseconds answer honestly. The clock is
-- captured, so an addon that replaces the `GetTime` global cannot bend it.
do
    local GetTime = GetTime
    local function noop() end
    debuginfo = noop
    debugload = noop
    debugprint = noop
    debugdump = noop
    debugbreak = noop
    debugtimestamp = noop

    local profileStart = 0
    function debugprofilestart() profileStart = GetTime() end
    function debugprofilestop() return (GetTime() - profileStart) * 1000 end
end

-- ── _G accessors (getglobal/setglobal: _G[name] get/set) ───────────────────────────────
function getglobal(name) return _G[name] end
function setglobal(name, value) _G[name] = value end

-- ── GetLocale: benilla ships/reads enUS data only (the 5875 MPQs + Spell.dbc enUS column) ──────
function GetLocale() return "enUS" end

-- ── NOT here: wipe / tostringall / strsplit / strjoin / strconcat / strtrim ────────────────────
-- Six 2.0+ names benilla used to define. 1.12 has none of them (absent from
-- `reference/1.12-globals.tsv`, from the stock chain, and from every registrar table), and the
-- reason they were kept — that the vanilla addon ecosystem assumes them — did not survive being
-- checked: the corpus callers are multi-client addons whose vanilla paths raise on a real 1.12
-- client too, or that define the name themselves under their own namespace.
-- An addon reaching for one of these is reaching for a later client's API, and it should find
-- what it would find there: nothing.

-- ── Blizzard's positional string.format (%N$) ──────────────────────────────────────────────────
do
    -- `_find`/`_sub` are captured for the same reason `_format` is: this wrapper runs on every
    -- `format` call in the UI and must not follow an addon's later replacement of `string.*`.
    -- They are CALLS and not method syntax because 1.12 installs no string metatable, so
    -- Keep format processing independent of later replacements of string methods.
    local _format, _find, _sub, _len = string.format, string.find, string.sub, string.len
    local CONV = "diouxXeEfgGqscp"  -- Lua 5.1 conversion letters

    local function reformat(fmt, ...)
        if type(fmt) ~= "string" then
            return _format(fmt, unpack(arg, 1, arg.n))
        end
        -- fast path: no "%<digit>" at all ⇒ definitely no positional spec.
        if not _find(fmt, "%%%d") then
            return _format(fmt, unpack(arg, 1, arg.n))
        end

        local args = arg
        local pieces = {}      -- rebuilt (sequential) format fragments
        local order = {}       -- for each real conversion: the source arg index, or false=sequential
        local seen_pos, seen_seq = false, false
        local npieces, norder = 0, 0
        local i, len = 1, _len(fmt)

        while i <= len do
            local c = _sub(fmt, i, i)
            if c ~= "%" then
                npieces = npieces + 1; pieces[npieces] = c
                i = i + 1
            elseif _sub(fmt, i + 1, i + 1) == "%" then
                npieces = npieces + 1; pieces[npieces] = "%%"
                i = i + 2
            else
                local j = i + 1
                local ds, de = _find(fmt, "^%d+%$", j)  -- optional N$
                if ds then
                    seen_pos = true
                    norder = norder + 1; order[norder] = tonumber(_sub(fmt, ds, de - 1))
                    j = de + 1
                else
                    seen_seq = true
                    norder = norder + 1; order[norder] = false
                end
                -- copy flags/width/precision + the conversion letter
                local ce = _find(fmt, "[" .. CONV .. "]", j)
                if not ce then
                    error("invalid conversion in format string", 2)
                end
                npieces = npieces + 1; pieces[npieces] = "%" .. _sub(fmt, j, ce)
                i = ce + 1
            end
        end

        if seen_pos and seen_seq then
            error("cannot mix positional and sequential arguments in format string", 2)
        end

        local out, seq = {}, 0
        for k = 1, norder do
            local idx = order[k]
            if idx == false then
                seq = seq + 1
                out[k] = args[seq]
            else
                out[k] = args[idx]
            end
        end
        return _format(table.concat(pieces, "", 1, npieces), unpack(out, 1, norder))
    end

    string.format = reformat
    format = reformat
end
"##;

/// `time()` and `date([format [, when]])`: 1.12 engine globals (`0x7035a0`, `0x7033a0`), Lua 5.0's
/// `os.time`/`os.date` hoisted. `time` is wall-clock epoch seconds, as addons persist it across
/// sessions.
///
/// Deviation: `date` formats UTC where the reference uses local time, because the tree has no
/// timezone database.
fn install_time(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    g.set(
        "time",
        lua.create_function(|_, ()| Ok(crate::civil::unix_seconds()))?,
    )?;
    g.set(
        "date",
        lua.create_function(|lua, (fmt, when): (Option<String>, Option<i64>)| {
            // Bare `date()` is `%c`, as in Lua.
            let fmt = fmt.unwrap_or_else(|| "%c".to_string());
            let secs = when.unwrap_or_else(crate::civil::unix_seconds);
            // A leading `!` selects UTC, which every time here already is, so it is only consumed.
            let body = fmt.strip_prefix('!').unwrap_or(&fmt);
            // `*t` answers a table, as in Lua: an addon that loops until its `wday` matches
            // (`Accountant_WeekStart`) spins forever on anything else.
            if body == "*t" {
                let c = crate::civil::from_unix(secs);
                let t = lua.create_table()?;
                t.set("year", c.year)?;
                t.set("month", c.month)?;
                t.set("day", c.day)?;
                t.set("hour", c.hour)?;
                t.set("min", c.min)?;
                t.set("sec", c.sec)?;
                // Lua counts weekdays 1..7 from Sunday; ours is the 0-based index into `DAYS`.
                t.set("wday", c.weekday + 1)?;
                // Already 1-based, as in Lua.
                t.set("yday", c.yearday)?;
                // UTC has no daylight saving.
                t.set("isdst", false)?;
                return Ok(mlua::Value::Table(t));
            }
            Ok(mlua::Value::String(
                lua.create_string(format_epoch(secs, body))?,
            ))
        })?,
    )?;
    Ok(())
}

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// The `strftime` subset addons use; an unknown specifier is emitted verbatim.
fn format_epoch(secs: i64, fmt: &str) -> String {
    let crate::civil::Civil {
        year,
        month,
        day,
        hour,
        min,
        sec,
        weekday: wday,
        yearday: yday,
    } = crate::civil::from_unix(secs);
    let mut out = String::with_capacity(fmt.len() + 16);
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(spec) = chars.next() else {
            out.push('%');
            break;
        };
        let hour12 = match hour % 12 {
            0 => 12,
            h => h,
        };
        match spec {
            'a' => out.push_str(&DAYS[wday as usize][..3]),
            'A' => out.push_str(DAYS[wday as usize]),
            'b' | 'h' => out.push_str(&MONTHS[(month - 1) as usize][..3]),
            'B' => out.push_str(MONTHS[(month - 1) as usize]),
            'c' => out.push_str(&format!(
                "{} {} {:2} {:02}:{:02}:{:02} {}",
                &DAYS[wday as usize][..3],
                &MONTHS[(month - 1) as usize][..3],
                day,
                hour,
                min,
                sec,
                year
            )),
            'd' => out.push_str(&format!("{day:02}")),
            'H' => out.push_str(&format!("{hour:02}")),
            'I' => out.push_str(&format!("{hour12:02}")),
            'j' => out.push_str(&format!("{yday:03}")),
            'm' => out.push_str(&format!("{month:02}")),
            'M' => out.push_str(&format!("{min:02}")),
            'p' => out.push_str(if hour < 12 { "AM" } else { "PM" }),
            'S' => out.push_str(&format!("{sec:02}")),
            'w' => out.push_str(&format!("{wday}")),
            'x' => out.push_str(&format!("{month:02}/{day:02}/{:02}", year.rem_euclid(100))),
            'X' => out.push_str(&format!("{hour:02}:{min:02}:{sec:02}")),
            'y' => out.push_str(&format!("{:02}", year.rem_euclid(100))),
            'Y' => out.push_str(&format!("{year}")),
            '%' => out.push('%'),
            other => {
                out.push('%');
                out.push(other);
            }
        }
    }
    out
}
/// `luaO_chunkid` (`0x6f5c40`), for `debugstack`'s `short_src`. An `@` name longer than
/// [`CHUNKID_KEEP`] keeps only its tail behind `"..."`, so a long addon path loses the `\AddOns\`
/// that corpus libraries search for.
fn chunk_id(source: &str) -> String {
    if let Some(rest) = source.strip_prefix('=') {
        // A `=name` chunk is taken verbatim, clipped to the buffer from the front.
        return rest.chars().take(LUA_IDSIZE - 1).collect();
    }
    if let Some(rest) = source.strip_prefix('@') {
        let n = rest.chars().count();
        if n <= CHUNKID_KEEP {
            return rest.to_string();
        }
        let tail: String = rest.chars().skip(n - CHUNKID_KEEP).collect();
        return format!("...{tail}");
    }
    // A string chunk: `[string "<first line, clipped>"]`.
    let first = source.split('\n').next().unwrap_or_default();
    let budget = LUA_IDSIZE - STRING_CHUNK_OVERHEAD;
    if first.chars().count() > budget || first.len() < source.len() {
        let clipped: String = first.chars().take(budget).collect();
        format!("[string \"{clipped}...\"]")
    } else {
        format!("[string \"{first}\"]")
    }
}

/// `LUA_IDSIZE`: the `short_src` buffer, 60 bytes with the terminator.
const LUA_IDSIZE: usize = 60;

/// Characters an `@` name keeps, tail-first, behind `"..."` (`0x6f5c40`).
const CHUNKID_KEEP: usize = 52;

/// What `[string "…"]` costs the budget in the third `luaO_chunkid` arm.
const STRING_CHUNK_OVERHEAD: usize = 17;

/// One `debugstack` frame, `short_src ":" [currentline ":"] DESC`, the caller pushing its `\n`.
/// DESC (`0x7038fa`) is `` in function `%s' `` for a named frame (`0x872c98`), else by `what`:
/// `" in main chunk"` (`0x872c88`), `" ?"` for C or a tail call (`0x872c6c`), or
/// `" in function <%s:%d>"` (`0x872c70`).
fn traceback_frame(d: &mlua::Debug) -> String {
    let src = d.source();
    let names = d.names();
    let short = src
        .source
        .as_deref()
        .map(chunk_id)
        .or_else(|| src.short_src.as_deref().map(str::to_owned))
        .unwrap_or_else(|| "?".to_string());
    let mut line = String::with_capacity(short.len() + 32);
    line.push_str(&short);
    line.push(':');
    if let Some(n) = d.current_line() {
        line.push_str(&n.to_string());
        line.push(':');
    }
    // A tail call has nothing to name it from and takes the `" ?"` arm, known by its `what` or as
    // the `(tail call)` pseudo-frame.
    let tail_call = src.what == "t" || short == "(tail call)";
    match names.name.as_deref() {
        Some(name) if !name.is_empty() && !tail_call => {
            line.push_str(" in function `");
            line.push_str(name);
            line.push('\'');
        }
        _ if src.what == "main" => line.push_str(" in main chunk"),
        _ if src.what == "C" || tail_call => line.push_str(" ?"),
        _ => {
            line.push_str(" in function <");
            line.push_str(&short);
            line.push(':');
            line.push_str(&src.line_defined.unwrap_or(0).to_string());
            line.push('>');
        }
    }
    line
}

/// `debugstack`'s walk (`0x703760`): Lua 5.0's `db_errorfb` without its header, each frame
/// followed by `"\n"` (`0x703971`) rather than preceded by `"\n\t"`, so line 1 is a frame.
/// Frames print while the level is `<= start + count1` (`0x703857`, `jbe`); then a failed
/// `getstack(level + count2)` probe prints that level as an ordinary frame, else `"...\n"` and
/// the last `count2` follow. So `count1 = 1` gives two frames on a two-deep stack, not one.
fn traceback_frames(lua: &Lua, start: usize, count1: usize, count2: usize) -> String {
    let mut out = String::new();
    let mut level = start;
    let mut first_part = true;
    while lua.inspect_stack(level, |_| ()).is_some() {
        level += 1;
        if level > start + count1 && first_part {
            first_part = false;
            if lua.inspect_stack(level + count2, |_| ()).is_none() {
                level -= 1; // nothing to elide: print this level after all
            } else {
                out.push_str("...\n");
                while lua.inspect_stack(level + count2, |_| ()).is_some() {
                    level += 1;
                }
            }
            continue;
        }
        if let Some(f) = lua.inspect_stack(level - 1, traceback_frame) {
            out.push_str(&f);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod date_table_tests {
    use crate::script::UiScript;

    /// `Accountant.lua:364-375` steps back a day until `date("*t", ct).wday` matches, which a
    /// string return never does.
    #[test]
    fn date_star_t_answers_a_table_that_walks_with_its_argument() {
        let s = UiScript::new().unwrap();
        // A Wednesday at 00:00 UTC: wday 4, counting from Sunday = 1.
        let t: i64 = 1_786_492_800;
        assert_eq!(
            s.eval::<(i64, i64, i64, i64, bool)>(&format!(
                "local d = date(\"*t\", {t}) return d.year, d.month, d.day, d.wday, d.isdst"
            ))
            .unwrap(),
            (2026, 8, 12, 4, false)
        );
        assert_eq!(
            s.eval::<(i64, i64, i64, i64)>(&format!(
                "local d = date(\"*t\", {t} + 3661) return d.hour, d.min, d.sec, d.yday"
            ))
            .unwrap(),
            (1, 1, 1, 224)
        );

        // Accountant's loop: one step back from Wednesday reaches Tuesday.
        assert_eq!(
            s.eval::<i64>(&format!(
                "local ct = {t} local n = 0 \
                 while date(\"*t\", ct).wday ~= 3 and n < 100 do ct = ct - 86400 n = n + 1 end \
                 return n"
            ))
            .unwrap(),
            1,
            "Wednesday(4) back to Tuesday(3) is exactly one step; a constant wday would spin"
        );

        // `!` is consumed, in the table form and the string form.
        assert_eq!(
            s.eval::<i64>(&format!("return date(\"!*t\", {t}).wday"))
                .unwrap(),
            4
        );
        assert!(
            !s.eval::<String>(&format!("return date(\"!%A\", {t})"))
                .unwrap()
                .contains('!'),
            "the UTC flag is consumed, not printed"
        );
        assert_eq!(
            s.eval::<String>(&format!("return date(\"%A\", {t})"))
                .unwrap(),
            "Wednesday"
        );
    }
}
