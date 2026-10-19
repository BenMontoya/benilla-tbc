//! TBC spell and totem state, fed by authoritative world-session packets.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use benilla_protocol::{SessionEvent, SessionEventKind};
use benilla_ui::script::{ScriptValue, TimedSpellView, TotemView, UiScript};
use bevy::prelude::*;

use crate::net::{NetHandlerApp, SelfGuid};
use crate::ui_action::Spells;

#[derive(Clone)]
struct TimedSpell {
    spell_id: u32,
    started: Instant,
    ends: Instant,
}

#[derive(Clone)]
struct Totem {
    guid: u64,
    spell_id: u32,
    started: Instant,
    duration_ms: u32,
    ends: Option<Instant>,
}

#[derive(Resource, Default)]
pub(super) struct TbcCastState {
    owner: Option<u64>,
    casts: HashMap<u64, TimedSpell>,
    channels: HashMap<u64, TimedSpell>,
    totems: [Option<Totem>; 4],
    changed_slots: Vec<u8>,
}

impl TbcCastState {
    fn remove_totem(&mut self, guid: u64) {
        for (i, slot) in self.totems.iter_mut().enumerate() {
            if slot.as_ref().is_some_and(|t| t.guid == guid) {
                *slot = None;
                self.changed_slots.push((i + 1) as u8);
            }
        }
    }
}

fn cast_start(In(ev): In<SessionEvent>, mut state: ResMut<TbcCastState>) {
    if let SessionEvent::SpellStart { caster, spell_id, cast_time_ms, .. } = ev {
        if cast_time_ms > 0 {
            let started = Instant::now();
            state.casts.insert(caster, TimedSpell {
                spell_id, started, ends: started + Duration::from_millis(u64::from(cast_time_ms)),
            });
        }
    }
}

fn cast_finish(In(ev): In<SessionEvent>, mut state: ResMut<TbcCastState>) {
    match ev {
        SessionEvent::SpellGo { caster, .. }
        | SessionEvent::SpellFailedOther { caster, .. } => { state.casts.remove(&caster); }
        _ => {}
    }
}

fn cast_failed(In(ev): In<SessionEvent>, mut state: ResMut<TbcCastState>, self_guid: Res<SelfGuid>) {
    if let SessionEvent::CastResult { success: false, .. } = ev {
        if let Some(owner) = self_guid.0 { state.casts.remove(&owner); }
    }
}

fn cast_delayed(In(ev): In<SessionEvent>, mut state: ResMut<TbcCastState>) {
    if let SessionEvent::SpellDelayed { caster, delay_ms } = ev {
        if let Some(cast) = state.casts.get_mut(&caster) {
            cast.ends += Duration::from_millis(u64::from(delay_ms));
        }
    }
}

fn channel(In(ev): In<SessionEvent>, mut state: ResMut<TbcCastState>) {
    match ev {
        SessionEvent::ChannelStart { caster, spell_id, duration_ms } => {
            if duration_ms > 0 {
                let started = Instant::now();
                state.channels.insert(caster, TimedSpell {
                    spell_id, started, ends: started + Duration::from_millis(u64::from(duration_ms)),
                });
            }
        }
        SessionEvent::ChannelUpdate { caster, remaining_ms } => {
            if remaining_ms == 0 {
                state.channels.remove(&caster);
            } else if let Some(channel) = state.channels.get_mut(&caster) {
                channel.ends = Instant::now() + Duration::from_millis(u64::from(remaining_ms));
            }
        }
        _ => {}
    }
}

fn totem_created(In(ev): In<SessionEvent>, mut state: ResMut<TbcCastState>) {
    if let SessionEvent::TotemCreated { slot, guid, duration_ms, spell_id } = ev {
        let Some(index) = state.totems.get_mut(usize::from(slot)) else { return };
        let started = Instant::now();
        *index = Some(Totem {
            guid, spell_id, started, duration_ms,
            ends: (duration_ms > 0).then(|| started + Duration::from_millis(u64::from(duration_ms))),
        });
        state.changed_slots.push(slot + 1);
    }
}

fn totem_destroyed(In(ev): In<SessionEvent>, mut state: ResMut<TbcCastState>) {
    match ev {
        SessionEvent::ObjectDestroyed(guid) => state.remove_totem(guid),
        SessionEvent::ObjectsRemoved(guids) => {
            for guid in guids { state.remove_totem(guid); }
        }
        _ => {}
    }
}

pub(super) fn register(app: &mut App) {
    use SessionEventKind as K;
    app.init_resource::<TbcCastState>()
        .net_handler(K::SpellStart, cast_start)
        .net_handler(K::SpellGo, cast_finish)
        .net_handler(K::SpellFailedOther, cast_finish)
        .net_handler(K::CastResult, cast_failed)
        .net_handler(K::SpellDelayed, cast_delayed)
        .net_handler(K::ChannelStart, channel)
        .net_handler(K::ChannelUpdate, channel)
        .net_handler(K::TotemCreated, totem_created)
        .net_handler(K::ObjectDestroyed, totem_destroyed)
        .net_handler(K::ObjectsRemoved, totem_destroyed);
}

pub(super) fn feed(
    script: &mut UiScript,
    state: &mut TbcCastState,
    self_guid: &SelfGuid,
    spells: Option<&Spells>,
) {
    let now = Instant::now();
    if state.owner != self_guid.0 {
        // Do not discard packets received before the first player-GUID feed.
        if state.owner.is_some() {
            state.casts.clear();
            state.channels.clear();
            state.totems = std::array::from_fn(|_| None);
            state.changed_slots.clear();
        }
        state.owner = self_guid.0;
    }
    state.casts.retain(|_, s| s.ends > now);
    state.channels.retain(|_, s| s.ends > now);
    for i in 0..4 {
        if state.totems[i].as_ref().is_some_and(|t| t.ends.is_some_and(|end| end <= now)) {
            state.totems[i] = None;
            state.changed_slots.push((i + 1) as u8);
        }
    }
    let clock_ms = script.now() * 1000.0;
    let convert = |s: &TimedSpell| {
        let row = spells.and_then(|catalog| catalog.catalog.get(s.spell_id));
        TimedSpellView {
            name: row.map(|r| r.name.clone()).unwrap_or_default(),
            rank: row.and_then(|r| r.rank.clone()).unwrap_or_default(),
            icon: row.and_then(|r| r.icon.clone()),
            start_ms: clock_ms - now.saturating_duration_since(s.started).as_secs_f64() * 1000.0,
            end_ms: clock_ms + s.ends.saturating_duration_since(now).as_secs_f64() * 1000.0,
        }
    };
    let casts = state.casts.iter().map(|(&guid, s)| (guid, convert(s))).collect();
    let channels = state.channels.iter().map(|(&guid, s)| (guid, convert(s))).collect();
    script.set_tbc_casts(casts, channels);

    let clock_secs = script.now();
    let totems = std::array::from_fn(|i| state.totems[i].as_ref().map(|t| {
        let row = spells.and_then(|catalog| catalog.catalog.get(t.spell_id));
        TotemView {
            name: row.map(|r| r.name.clone()).unwrap_or_default(),
            icon: row.and_then(|r| r.icon.clone()),
            start_secs: clock_secs - now.saturating_duration_since(t.started).as_secs_f64(),
            duration_secs: f64::from(t.duration_ms) / 1000.0,
        }
    }));
    script.set_tbc_totems(totems);
    for slot in state.changed_slots.drain(..) {
        script.fire_event("PLAYER_TOTEM_UPDATE", vec![ScriptValue::Int(i64::from(slot))]);
    }
}
