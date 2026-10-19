//! Bitwise Lua globals required by the TBC 2.4.3 UI.

use mlua::Lua;

pub(super) fn install_bit(lua: &Lua) -> mlua::Result<()> {
    let bit = lua.create_table()?;
    // The client truncates to a 32-bit int and answers signed. `as i64 as u32` is C's wrapping
    // `(unsigned)(int)x`, not a saturating cast, so `bnot(0)` is -1 as there.
    fn u32_of(v: f64) -> u32 {
        v as i64 as u32
    }
    fn out(v: u32) -> i64 {
        v as i32 as i64
    }
    bit.set(
        "bnot",
        lua.create_function(|_, a: f64| Ok(out(!u32_of(a))))?,
    )?;
    bit.set(
        "band",
        lua.create_function(|_, (a, b): (f64, f64)| Ok(out(u32_of(a) & u32_of(b))))?,
    )?;
    bit.set(
        "bor",
        lua.create_function(|_, (a, b): (f64, f64)| Ok(out(u32_of(a) | u32_of(b))))?,
    )?;
    bit.set(
        "bxor",
        lua.create_function(|_, (a, b): (f64, f64)| Ok(out(u32_of(a) ^ u32_of(b))))?,
    )?;
    // Shift counts mask to 5 bits, as the client's x86 shifts do: `lshift(1, 32)` is 1.
    bit.set(
        "lshift",
        lua.create_function(|_, (a, n): (f64, f64)| Ok(out(u32_of(a) << (u32_of(n) & 31))))?,
    )?;
    bit.set(
        "rshift",
        lua.create_function(|_, (a, n): (f64, f64)| Ok(out(u32_of(a) >> (u32_of(n) & 31))))?,
    )?;
    bit.set(
        "arshift",
        lua.create_function(|_, (a, n): (f64, f64)| {
            Ok(out(((u32_of(a) as i32) >> (u32_of(n) & 31)) as u32))
        })?,
    )?;
    // `bit.mod`: an integer remainder, unlike `math.mod`'s float one.
    bit.set(
        "mod",
        lua.create_function(|_, (a, b): (f64, f64)| {
            let b = u32_of(b) as i32;
            if b == 0 {
                return Err(mlua::Error::runtime("bit.mod: division by zero"));
            }
            Ok(((u32_of(a) as i32) % b) as i64)
        })?,
    )?;
    lua.globals().set("bit", bit)
}
