# Bedrock world map (Rust + Tauri)

Reads a Minecraft **Bedrock** world and builds a pan-and-zoom web map (overworld, nether, end). This is the
Rust + Tauri version of the Node.js tool: the same map files and the same look, as a desktop app instead of
`start.bat` + a browser tab + a terminal window.

## What changed

| Before (Node.js) | Now (Rust + Tauri) |
| --- | --- |
| `node bedrock-map.js ui`, browser tab at 127.0.0.1:8123 | a normal desktop window |
| local HTTP server + `/api/...` calls | Tauri commands (`list_worlds`, `add_world`, `start_build`, `job_status`, `open_map`) |
| map built in a child Node process | map built on a background thread inside the app |
| "Open map" opens a browser tab | "Open map" opens a map window (served by a built-in `maps://` protocol) |
| `maps/` folder next to the tool | `%APPDATA%\Bedrock Map\maps` (macOS: `~/Library/Application Support/Bedrock Map/maps`, Linux: `~/.local/share/Bedrock Map/maps`) |

The control panel (`ui/`) and the map page (`viewer/`) are the original files; only the places that talked to the
local server were changed. Tile format, `meta.json` and the colour table are unchanged, so **maps you built with
the Node version still work**: copy your old `maps` folder into the new data folder.

Dropped on purpose: the command-line interface (`--list`, `--serve-only`, ...), the `--dims` / `--nether-roof`
options (the panel never used them; the nether roof is skipped as before), and the Node test-suite.

## Cave layer

Tick **Cave layer** on a world card before building (on by default, overworld only). On the map page a **Caves**
switch (or the `C` key) dims the surface and paints tunnels and caverns over it: orange is shallow, purple is deep,
and brighter means more open space in that column. Maps built before this version need a **Rebuild map** to get it.

How it decides what is a cave: from the top of each column it finds the surface (the first solid block, ignoring
leaves), then counts open blocks (air and see-through blocks such as torches or rails, not water) that lie at least
8 blocks below it. That keeps rooms in houses and gaps under trees out, but it also means caves shallower than 8
blocks are not shown, and big overhangs or tall hollow buildings can show up as caves. Change `CAVE_COVER` in
`render.rs` to tune it. Building with the cave layer is slower because every sub-chunk of every column is read.

## 3D view

The **3D** button on the map page (or the `V` key) tilts the map into blocky 3D terrain, in the style of BlueMap.
It is built in the page from the colour and height tiles the map already has, so no rebuild is needed (maps from
before height tracking do need one). Left-drag moves, right-drag (or Shift-drag) turns and tilts, the wheel zooms
towards the cursor; WASD or the arrows move, Q/E turn, R/F tilt, and the compass button faces north again. Chunk grid,
caves, pins and the overview map work in 3D too. It needs WebGL2; without it the button stays greyed out.

Only the top block of each column is known, so overhangs, cave mouths and the undersides of trees are filled in
solid, and blocks are flat colours (no textures). `viewer/map3d.js` holds the renderer; `LOD`, `MAX_TILES` and
`DMAX` at its top trade detail and memory for view distance.

## Live link

The **Live link** tab (under "Add a world by hand") receives live data from a companion plugin on your Minecraft
server: online players show as green pins on open maps, changed chunks are drawn into the map tiles (2D and 3D
update in place), and joins, chat and other events appear in the tab's activity list.

**In-game pack (single-player and co-op worlds hosted on this PC).** The tab's "Install in Minecraft" button
builds `Bedrock Map Live.mcpack` and opens it, so Minecraft imports it; "Show the .mcpack file" saves it to share
with friends who also use the app. Then turn on Settings > Creator > Content Log File in Minecraft (the tab checks
this and can turn it on while the game is closed), activate the pack on a world, and play. When the app sees a
world with the pack open, it builds that world's map if there is none yet (the world is found as the one whose
files changed last, and remembered from then on) and opens the map window behind the game; the tab has a switch to
turn this off.
The pack needs no cheats. World scripts cannot use the network, so it writes its messages to Minecraft's content
log, which the app reads (`src-tauri/src/live_game.rs`); the pack's source is in `pack/`, and the app fills in its
list of see-through blocks when it packages it. It sends online players every second, each changed chunk shortly
after a block is placed, broken or blown up, and the chunks within 5 of each player once per session, so newly
explored ground appears on the map. Only the host's PC gets the data; friends who join see nothing in their app.

**Server plugins.** The app runs a small HTTP server for other plugins, on `127.0.0.1:8130` by default (only this
computer). The tab shows the
address and the key a plugin needs, lets you change the port, allow other devices on your network, make a new key,
and pick which built map each connected plugin's world is drawn into. Settings are saved in
`%APPDATA%\Bedrock Map\live-link.json`. Live chunk drawing waits while a map is being built, and a rebuild replaces
whatever was drawn live.

The protocol a plugin speaks (JSON over HTTP, `hello`, `push`, `bye`, and `players`, `chunk`, `event`, `world`
messages) is in [docs/live-protocol.md](docs/live-protocol.md). The code is `src-tauri/src/live.rs` (server and
state), `src-tauri/src/live_tiles.rs` (drawing chunks into tiles) and `ui/live.js`.

## Build and run

You need [Rust](https://rustup.rs) (1.80 or newer) and, on Windows, the WebView2 runtime (already part of Windows 11).
On Linux you also need the usual Tauri system packages (see https://tauri.app/start/prerequisites/).

```
cargo install tauri-cli --version "^2"
cd src-tauri
cargo tauri dev        # run it
cargo tauri build      # make an installer (src-tauri/target/release/bundle)
cargo test             # a few unit tests
```

`cargo tauri icon src-tauri/icons/icon.png` regenerates the icon set (including `.icns` for macOS) if you want a
different icon.

## Layout

```
ui/            control panel (HTML/JS/CSS)       viewer/        map page, embedded into the program
src-tauri/src/
  main.rs      app start, command + protocol registration
  panel.rs     the commands the panel calls, build jobs
  live.rs      live link server for companion plugins; live_tiles.rs draws their chunks into tiles
  live_game.rs reads the in-game pack's content log, builds the .mcpack
pack/          the in-game behavior pack (manifest + scripts/main.js)
  protocol.rs  maps://  (http://maps.localhost on Windows) file server
  worlds.rs    find worlds / .mcworld files, unzip
  builder.rs   build one world into a map folder + meta.json
  world.rs     level.dat, player, chunk index
  leveldb.rs   LevelDB tables + logs (snappy / zlib blocks)
  chunk.rs nbt.rs colors.rs color_tables.rs render.rs pngio.rs
```
