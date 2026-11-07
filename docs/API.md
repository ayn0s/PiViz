# PiViz WebSocket API

Control PiViz live from a Stream Deck plugin, a script or any WebSocket client: read the current
state, change any setting, switch color presets and profiles, turn the camera on and off.

## Connecting

```
ws://127.0.0.1:7341
```

- The API is on by default. It can be turned off, or moved to another port, in the settings panel
  under **Remote control (API)**. The panel shows the address, or why the port could not be opened.
- It only listens on `127.0.0.1`: only programs on the same computer can connect. There is no
  password.
- Several clients can be connected at once.

## Messages

Every message is a JSON text frame.

**Request** (client → PiViz):

```json
{ "id": 1, "type": "color_preset", "name": "Aurora" }
```

`type` selects the command; its other fields are listed below. `id` is optional and may be any
JSON value; it is echoed in the response so you can match them.

**Response** (PiViz → client, one per request):

```json
{ "id": 1, "ok": true }
{ "id": 2, "ok": false, "error": "unknown name \"Aurore\"" }
{ "id": 3, "ok": true, "state": { … } }
```

Only `get_state` returns a `state`. After any change, from the API or from the panel, PiViz
pushes a **state event** to every client:

```json
{ "event": "state", "state": { … } }
```

A state event is also sent right after connecting. While something changes continuously (a
slider being dragged), events are sent at most every 100 ms, and always once after the last
change. A client that only listens to these events always knows the current state.

## Commands

| `type` | Fields | Effect |
|---|---|---|
| `get_state` | — | Returns the full state. |
| `set` | `settings`: object | Changes any settings; see [Settings](#settings). |
| `color_preset` | `name` **or** `step` | Applies a color preset by name, or the next (`1`) / previous (`-1`) one, wrapping around. |
| `profile` | `name` **or** `step` | Loads a saved profile by name, or the next / previous one. |
| `camera` | `enabled`, `device` (optional) | Turns the camera background on or off. Off returns to the previous background. `device` chooses the camera by name (from `state.camera.devices`). |
| `keyboard` | `visible` | Shows or hides the virtual keyboard. |
| `panel` | `visible` | Shows or hides the settings panel. |

`enabled` and `visible` take `true`, `false` or `"toggle"`. Leaving them out also toggles.

Examples:

```json
{ "type": "camera", "enabled": "toggle" }
{ "type": "camera", "device": "Logitech StreamCam", "enabled": true }
{ "type": "color_preset", "step": 1 }
{ "type": "profile", "name": "Stream" }
{ "type": "set", "settings": { "background": "black" } }
{ "type": "set", "settings": { "bloom": 0.6, "colors": { "accent": "#ff8000" } } }
```

## Settings

`set` takes a **partial** settings object: only the fields given change. Nested objects (such as
`colors`) are merged field by field; arrays (gradients) are replaced whole. Values outside a
range are brought back into it. An unknown field or a wrong type rejects the whole request,
and nothing changes.

Colors are `[r, g, b]` (0–255) or `"#rrggbb"`.

| Field | Type | Range / values |
|---|---|---|
| `fall_speed` | number | 80–1500: rise speed of the notes, in pixels per second |
| `bloom` | number | 0–1 |
| `glow` | number | 0.5–4: note brightness |
| `sparks` | number | 0–3 (0 = none) |
| `relief` | number | 0–2: 3D bevel of the notes |
| `halo` | number | 0–3 |
| `volume` | number | 0–1.5 |
| `camera_brightness` | number | 0.1–1 |
| `show_keyboard` | bool | |
| `fps_limit` | integer | 0 (follow the display) or 10–1000 |
| `background` | string | `"theme"`, `"black"`, `"transparent"`, `"camera"` |
| `language` | string | `"auto"`, `"en"`, `"fr"` |
| `colors.coloring` | string | Notes: `"solid"` or `"gradient"` |
| `colors.solid` | color | Notes, solid |
| `colors.gradient` | colors | Notes, gradient: 2 to 5 colors, low to high keys |
| `colors.accent_coloring` | string | Glow: `"solid"` or `"gradient"` |
| `colors.accent` | color | Glow, solid |
| `colors.accent_gradient` | colors | Glow, gradient: 2 to 5 colors |
| `colors.background` | color | Background color of the `"theme"` background |

## State

```json
{
  "api_version": 1,
  "settings": { "fall_speed": 320, "bloom": 0.28, "colors": { "coloring": "gradient", … }, … },
  "panel_visible": true,
  "camera": {
    "enabled": false,
    "device": "Logitech StreamCam",
    "devices": ["Logitech StreamCam"],
    "status": "off",
    "fps": null
  },
  "color_presets": ["Ember", "Aurora", "Neon", "Glacier", "Gold", "My preset"],
  "color_preset": "Ember",
  "profiles": ["Stream", "Practice"],
  "profile": null
}
```

| Field | Meaning |
|---|---|
| `api_version` | Changes only if a new version could break existing clients. |
| `settings` | Every setting, in the format of [Settings](#settings). |
| `panel_visible` | Whether the settings panel is shown. |
| `camera.enabled` | Whether the camera background is on. |
| `camera.device`, `camera.devices` | The chosen camera and the cameras found. |
| `camera.status` | `"off"`, `"opening"`, `"running"`, `"no_device"`, `"not_found"`, `"failed"`, `"lost"`. |
| `camera.fps` | Frames per second really received, while running; otherwise `null`. |
| `color_presets`, `color_preset` | Built-in then user presets; the one matching the current colors, or `null`. |
| `profiles`, `profile` | Saved profiles; the one matching the current settings, or `null`. |

## Examples

### JavaScript (browser, Node 22+, Stream Deck plugin)

```js
const ws = new WebSocket("ws://127.0.0.1:7341");

ws.onmessage = (message) => {
  const data = JSON.parse(message.data);
  if (data.event === "state") {
    // Keep key images in sync, e.g. a lit "camera" key while the camera is on.
    console.log("camera on:", data.state.camera.enabled, "preset:", data.state.color_preset);
  } else if (!data.ok) {
    console.error(data.error);
  }
};

// Bind these to keys or dials.
const send = (request) => ws.send(JSON.stringify(request));
const toggleCamera = () => send({ type: "camera", enabled: "toggle" });
const nextPreset = () => send({ type: "color_preset", step: 1 });
const setBloom = (value) => send({ type: "set", settings: { bloom: value } });
```

### Python

```python
import asyncio, json, websockets  # pip install websockets

async def main():
    async with websockets.connect("ws://127.0.0.1:7341") as ws:
        print(json.loads(await ws.recv())["state"]["color_preset"])  # the state sent on connect
        await ws.send(json.dumps({"id": 1, "type": "color_preset", "name": "Neon"}))
        print(await ws.recv())

asyncio.run(main())
```

### Command line ([websocat](https://github.com/vi/websocat))

```sh
echo '{"type":"camera","enabled":"toggle"}' | websocat ws://127.0.0.1:7341
```

## Tips for a Stream Deck plugin

- Keep one connection open and reconnect if it drops: PiViz may be started after the Stream Deck
  software, or restarted.
- Show key states from state events (`camera.enabled`, `color_preset`, `profile`), not from what
  you sent: the panel can change them too.
- For dials, send `set` with the new value; there is no need to throttle, PiViz applies the
  latest value each frame.
