# Live link protocol (version 1)

How a companion plugin (a Bedrock Dedicated Server add-on, an Endstone or LeviLamina plugin, a proxy...) sends live
data to the Bedrock Map app. The app shows it on the **Live link** tab of the control panel and in every open map
window: online players move on the map, and changed chunks are redrawn into the map tiles.

Everything is plain JSON over HTTP, so any plugin platform that can make an HTTP request can use it, including the
Script API's `@minecraft/server-net` module on Bedrock Dedicated Server.

## Connecting

| Setting | Where to find it |
| --- | --- |
| Address | Live link tab, "Plugin address", e.g. `http://127.0.0.1:8130/v1` |
| Key | Live link tab, "Plugin key" (a long random string; "New key" replaces it) |

By default the app only listens on this computer (`127.0.0.1`). Tick **Allow other devices on my network** when the
server runs on another machine; the tab then also lists the LAN address to use.

Every request except `GET /v1/ping` must carry the key:

```
Authorization: Bearer <key>
Content-Type: application/json
```

Bodies are UTF-8 JSON, at most 4 MB per request.

## Endpoints

### `GET /v1/ping`

No key needed. Use it to check the app is running.

```json
{ "ok": true, "app": "bedrock-map", "protocol": 1 }
```

### `POST /v1/hello`

Start a session. Send it once when the plugin starts, and again whenever a request answers `410`.

```json
{
  "protocol": 1,
  "plugin": { "name": "bedrock-map-live", "version": "0.1.0" },
  "world": { "name": "Hayden and boss", "id": "TbdCazG1VTs=" },
  "server": { "name": "My server", "software": "BDS 1.21.50" }
}
```

`world.name` and `world.id` are optional but help the app pick the right map. `server` is optional and only shown.

Answer:

```json
{
  "ok": true,
  "session": "s-8f3a1c0d9e2b4f67",
  "link": { "slug": "Hayden-and-boss-8d1b20", "name": "Hayden and boss" },
  "settings": { "playersEveryMs": 1000, "maxMessages": 2000 }
}
```

`link` is the built map this session's data goes to, or `null` when the app could not match one. The user can link
a map on the Live link tab at any time; the plugin does not need to do anything. Chunk data sent while no map is
linked is dropped (players and events are still shown on the tab).

### `POST /v1/push`

Send any number of messages (at most `settings.maxMessages`) in one request.

```json
{ "session": "s-8f3a1c0d9e2b4f67", "messages": [ { "type": "players", "players": [] } ] }
```

Answer:

```json
{ "ok": true, "accepted": 1, "ignored": 0, "errors": [] }
```

`ignored` counts messages of a type this app version does not know (newer plugins can send them safely). `errors`
lists messages that were rejected, e.g. `"message 3 (chunk): columns must have 256 entries"`.

### `POST /v1/bye`

```json
{ "session": "s-8f3a1c0d9e2b4f67" }
```

Ends the session (players disappear from the map at once). Sessions that send nothing for 3 minutes are ended
automatically; after 30 seconds of silence they show as "quiet".

## Messages

Dimensions are numbers: `0` overworld, `1` nether, `2` the end.

### `players`

The full list of online players. Send it about once a second (`settings.playersEveryMs`); players missing from the
list have left.

```json
{
  "type": "players",
  "players": [
    { "id": "-4294967295", "name": "Steve", "dim": 0, "x": 12.5, "y": 70, "z": -340.2, "yaw": 90, "health": 20 }
  ]
}
```

`id`, `name`, `dim`, `x`, `y`, `z` are required. `yaw` (degrees, 0 = south, 90 = west, like the game) and `health`
are optional.

### `chunk`

The top of every column of one chunk, after it changed (a block placed or broken, an explosion, a newly generated
chunk). The app redraws that part of the map and updates the zoomed-out tiles.

```json
{
  "type": "chunk",
  "dim": 0,
  "cx": -12,
  "cz": 40,
  "columns": [ { "y": 71, "block": "minecraft:grass_block" }, { "y": 62, "block": "minecraft:sand", "water": 4 } ]
}
```

- `cx`, `cz` are chunk coordinates (block X and Z divided by 16, rounded down).
- `columns` has exactly 256 entries, `columns[z * 16 + x]` for local `x`, `z` from 0 to 15.
- Each entry is the highest block that is not see-through (skip air, torches, flowers, glass, rails...), as
  `{ "y": <block Y>, "block": "<block id>" }`.
- When there is water on top, `y` is the height of the water surface, `block` is the first block under the water
  and `water` is how many water blocks deep it is. A column of only water down to the bottom of the world can send
  `{ "y": 62, "block": "minecraft:water", "water": 30 }`.
- `null` means "unknown, leave this column as it is". Use it for columns that are not loaded.

Compact form (about a fifth of the size): list each block name once in `blocks`, and send each column as an array
`[y, index into blocks]`, or `[y, index into blocks, water]` when there is water on top.

```json
{ "type": "chunk", "dim": 0, "cx": -12, "cz": 40, "blocks": ["minecraft:grass_block", "minecraft:sand"], "columns": [[71, 0], [62, 1, 4], null] }
```

Send each changed chunk at most every few seconds; the app redraws at most once a second anyway.

### `event`

Something worth showing in the activity list on the Live link tab.

```json
{ "type": "event", "kind": "chat", "player": "Steve", "text": "hello", "dim": 0, "x": 12, "y": 70, "z": -340 }
```

`kind` is free text; the app knows `join`, `leave`, `chat`, `death` and shows anything else as is. Everything except
`kind` is optional.

### `world`

World state shown on the Live link tab. Any of the fields may be left out.

```json
{ "type": "world", "time": 6000, "day": 12, "weather": "rain" }
```

## Errors

| Status | Meaning | What the plugin should do |
| --- | --- | --- |
| 400 | Body is not valid JSON, or a required field is missing | Fix the request |
| 401 | Missing or wrong key | Stop and tell the server owner to copy the key from the app |
| 404 | Unknown endpoint | Check the address ends in `/v1` |
| 410 | Unknown session (the app restarted or the session timed out) | Send `hello` again, then retry |
| 413 | Body over 4 MB | Send fewer messages per request |

Error bodies look like `{ "ok": false, "error": "Unknown session. Send hello again." }`.

## Without a network: the content log

Scripts in an ordinary or co-op world cannot make web requests (`@minecraft/server-net` only exists on Bedrock
Dedicated Server). The app's own in-game pack (`pack/` in the source) therefore writes the same messages to
Minecraft's content log, which the app reads as it grows (`%APPDATA%\Minecraft Bedrock\logs`, and the Preview and
older Microsoft Store folders). The player must turn on Settings > Creator > Content Log File.

Each message is one `console.warn` line: `BMLIVE1 ` followed by JSON. The app looks for that marker anywhere in the
line, so the log's own prefix does not matter.

```
BMLIVE1 {"s":"k3j9x0a1qz7f","t":"hello","d":{ <hello body> }}
BMLIVE1 {"s":"k3j9x0a1qz7f","t":"push","m":[ <messages> ]}
BMLIVE1 {"s":"k3j9x0a1qz7f","t":"bye"}
```

`s` is a session id the pack picks when the world starts. No key is needed (only programs on this PC can write
the log). The pack says hello again every 30 seconds, so an app started mid-game still picks the world up, and it
keeps a random `world.id` in the world itself (dynamic property `bmlive:world_id`), so the app remembers which map
the world belongs to. The first time, the app links the map of the world whose files changed most recently.

## Versioning

`protocol` is `1`. New optional fields and new message types may be added without changing it, so plugins must
ignore fields they do not know, and the app ignores message types it does not know. A change that breaks old
plugins will raise the number.
