<div align="center">
  <h1>Benilla-TBC</h1>
  <p><b>World of Warcraft: The Burning Crusade 2.4.3 client in Rust and <a href="https://bevy.org">Bevy</a></b></p>
  <p><a href="LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="License"></a></p>
</div>

**Benilla-TBC (The Benilla Crusade)** is a fork of
[Benilla](https://github.com/samwhosung/benilla) targeting
**World of Warcraft: The Burning Crusade 2.4.3 (build 8606)**.
It reads game data from your own 2.4.3 installation and connects to compatible
TBC servers using the original client protocol.

## What's inside

- **Formats:** readers for MPQ archives, BLP textures, DBC tables, ADT/WDT/WDL maps,
  M2 models and WMO buildings.
- **World:** streamed terrain, buildings, doodads, water, lighting, fog and weather.
- **Models:** animated M2s, characters, equipment, particles, ribbons and spell visuals.
- **Movement:** player and creature movement, transports, mounts and camera controls.
- **Networking:** SRP6 authentication, world-session encryption, object updates,
  and gameplay packets.
- **Interface:** a FrameXML and Lua engine using the TBC interface files from your installation.
- **Audio:** music, ambience and sound effects.

The asset readers and UI engine are separate from Bevy, and the world renderer can run
without connecting to a game server. See [the crate map](docs/MAP.md) for the project layout.

## Running it

Requirements:

- **World of Warcraft: The Burning Crusade 2.4.3 (build 8606)** game data.
- A compatible **2.4.3 server** with Warden disabled.
- Stable Rust and a C compiler. Linux builds also require native audio and input
  development libraries (including ALSA and udev) and `pkg-config`.

From the repository root:

```sh
WOW_DATA=/path/to/WoW/Data cargo run -p benilla
```

On Windows (PowerShell):

```powershell
$env:WOW_DATA="C:\path\to\WoW\Data"; cargo run -p benilla
```

`WOW_DATA` points to the installation's `Data` directory. The default authentication
server is `localhost:3724`. Set `WOW_HOST` to another host or `host:port`, or use
the Realmlist button on the login screen. Set `WOW_USER` and `WOW_PASS` to provide
credentials through environment variables.

Settings and addons are stored in `benilla-config/` at the repository root. Addons
belong in `benilla-config/AddOns/`.

## Credits and license

Based on [Benilla](https://github.com/samwhosung/benilla) by samwhosung.
Additional format references come from the
[wowemulation-dev](https://github.com/wowemulation-dev) community and
[warcraft-rs](https://github.com/wowemulation-dev/warcraft-rs).

Benilla-TBC ships **no Blizzard game content**. Supply your own legally obtained
2.4.3 client files. This is an independent fan project, not affiliated with or
endorsed by Blizzard Entertainment. World of Warcraft is a trademark of
Blizzard Entertainment, Inc.

The source code is licensed under [MIT](LICENSE-MIT) or
[Apache 2.0](LICENSE-APACHE), at your option. Vendored components in
`third_party/` retain their respective licenses.
