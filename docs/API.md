# `keylightd` API

`keylightd` exposes a small **localhost-only** HTTP JSON API. The LimeLight window uses it; it is also meant for scripts and integrations (Open Deck etc.).

- **Base URL**: `http://127.0.0.1:9124` (`LIMELIGHT_PORT` overrides)
- **Content-Type**: `application/json`
- Requests must carry a loopback `Host` header (`127.0.0.1`, `localhost`, `[::1]`); anything else is rejected with 403.
- Body limit 64 KiB. Rate limit: 400 control requests/s, 5 refreshes per 10 s.
- Light **ids** look like `serial:CW25K1A01802`. They are stable across renames and IP changes. Percent-encode them in paths. Aliases and device names are accepted wherever an id is.

Typical flow:

1. `GET /v1/lights/states` — everything the UI needs, including offline lights.
2. `PUT /v1/lights/{id}` / `PUT /v1/groups/{name}` / `PUT /v1/all` — send changes.
3. `POST /v1/lights/refresh` only when the user asks for a scan; discovery runs continuously anyway.

## Endpoints

### `GET /v1/health`

```json
{ "status": "ok", "version": "0.2.0", "reachable": 2, "enabled": 2 }
```

### `POST /v1/shutdown`

Stops the daemon (used by the GUI to replace an outdated daemon after an upgrade).

### `GET /v1/lights`

Persisted light records (including disabled ones). Fields: `id`, `alias`, `name`, `hostname`, `port`, `addresses`, `last_seen_unix`, `enabled`, `accessory_info`, `mdns_fullname`, `serial`, `product`.

### `POST /v1/lights`

Add a light by LAN IP (fallback when mDNS is blocked). `{ "ip": "192.168.1.106" }` → the record. Non-LAN addresses are rejected.

### `POST /v1/lights/refresh`

One-shot mDNS scan. Optional body `{ "timeout": 3 }` (1–15 s). Response: `{ "refreshed": true, "found": 2, "lights": [...] }`.

### `GET /v1/lights/states`

One entry per **enabled** light, always, fetched from the lights in parallel:

```json
[
  {
    "id": "serial:CW25K1A01802",
    "name": "Elgato Key Light Air 805B",
    "alias": "Left Light",
    "enabled": true,
    "reachable": true,
    "on": false,
    "brightness": 39,
    "kelvin": 4975,
    "hue": null,
    "saturation": null,
    "color_capable": false,
    "product": "Elgato Key Light Air"
  }
]
```

When `reachable` is false the values are the last known ones. Colour lights (Light Strip) report `hue` (0–360) and `saturation` (0–100) while in colour mode.

### `PUT /v1/lights/{id}`, `PUT /v1/groups/{name}`, `PUT /v1/all`

Body, all fields optional (at least one required):

```json
{ "on": 1, "brightness": 50, "kelvin": 4500, "mired": 222, "hue": 120.0, "saturation": 80.0 }
```

- `on`: 0 or 1 · `brightness`: 0–100 · `kelvin`: 2900–7000 (or `mired` 143–344; `mired` wins if both are sent)
- `hue` / `saturation`: colour lights only.

Every target is contacted at the same time. Response has a per-light result; HTTP 200 if at least one light accepted, 502 if none did, 404 if the target does not exist.

```json
{
  "ok": true,
  "results": [
    { "id": "serial:…", "ok": true, "error": null, "state": { "...LightStateResponse" } },
    { "id": "serial:…", "ok": false, "error": "unreachable: …", "state": null }
  ]
}
```

### `GET /v1/lights/{id}`

One persisted record (same shape as the list).

### `GET /v1/lights/{id}/settings` / `PUT /v1/lights/{id}/settings`

The light's own settings (what Control Center calls device settings). Read straight from the light; the PUT body may be partial, the daemon merges it into the current settings before writing (the lights reject partial bodies themselves).

```json
{
  "powerOnBehavior": 1,
  "powerOnBrightness": 20,
  "powerOnTemperature": 213,
  "switchOnDurationMs": 100,
  "switchOffDurationMs": 300,
  "colorChangeDurationMs": 100
}
```

- `powerOnBehavior`: 1 = restore the last state, 2 = use `powerOn*` defaults.
- `powerOnTemperature` is in **mired** (143–344).
- Durations are milliseconds; 0 = instant.
- Colour lights add `powerOnHue` / `powerOnSaturation`; Key Light Mini adds a `battery` object.

### `DELETE /v1/lights/{id}`

Forgets the light and removes it from every group.

### `PUT /v1/lights/{id}/enabled` — `{ "enabled": true }`

Hidden lights are kept but not shown or controlled.

### `PUT /v1/lights/{id}/alias` — `{ "alias": "Left" }`

Local nickname. `null` or empty clears it.

### `PUT /v1/lights/{id}/name` — `{ "name": "Desk Left" }`

Renames the **device itself** (same as Control Center). The mDNS name changes; identity does not.

### `POST /v1/lights/{id}/identify`

Makes the light blink.

### Groups

- `GET /v1/groups` → `[{ "name": "office", "members": ["serial:…"] }]`
- `POST /v1/groups` — `{ "name": "office", "members": ["<id or alias>", …] }` creates or replaces. Unknown members are rejected.
- `DELETE /v1/groups/{name}`

### Presets

Named, target-independent looks shared by the window and the OpenDeck plugin.

- `GET /v1/presets` → `[{ "name": "Night", "on": true, "brightness": 15, "kelvin": 3200 }]` (colour lights may add `hue`/`saturation`)
- `POST /v1/presets` — body is one preset; inserts or replaces by case-insensitive name, keeps position. 400 on empty/too-long name.
- `PUT /v1/presets` — body is the whole ordered list; use for rename and reorder. 400 on duplicate names.
- `DELETE /v1/presets/{name}`

Applying a preset is client-side: turn it into an update (`{on:1, brightness, kelvin}` or hue/saturation for colour presets) and send it to `/v1/lights/{id}`, `/v1/groups/{name}` or `/v1/all`. A light "is on" a preset when it is on and within 3 % brightness and 150 K.

### Settings

- `GET /v1/settings` / `PUT /v1/settings`

```json
{ "auto_enable_discovered": true }
```

## Errors

```json
{ "error": "message" }
```

`400` bad request · `403` bad Host · `404` not found · `413` body too large · `429` rate limited · `500` internal · `502` light(s) did not respond.
