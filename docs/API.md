# LimeLight / `keylightd` API

`keylightd` exposes a small **localhost** HTTP JSON API. The LimeLight GUI uses it, and it’s also intended for third‑party integrations (scripts, Open Deck, etc).

## Quick start

- **Base URL (default)**: `http://127.0.0.1:9124`
- **Content-Type**: `application/json`
- **Local only**: the daemon binds to `127.0.0.1` only (no LAN access)
- **Limits**:
  - Request body: **64 KiB**
  - Basic rate limiting (high enough for “live” sliders)

If you’re building an integration, the usual flow is:

1. Trigger discovery: `POST /v1/lights/refresh`
2. Read persisted lights: `GET /v1/lights`
3. Read current states (optional, for UI): `GET /v1/lights/states`
4. Send updates: `PUT /v1/lights/{id}`, `PUT /v1/groups/{name}`, or `PUT /v1/all`

## Common `curl` examples

Health check:

```bash
curl -s http://127.0.0.1:9124/v1/health
```

Discover lights (mDNS scan, ~3 seconds):

```bash
curl -s -X POST http://127.0.0.1:9124/v1/lights/refresh
```

List persisted lights:

```bash
curl -s http://127.0.0.1:9124/v1/lights
```

Fetch current states (on/brightness/temperature) for enabled + reachable lights:

```bash
curl -s http://127.0.0.1:9124/v1/lights/states
```

Turn on a specific light:

```bash
curl -s -X PUT http://127.0.0.1:9124/v1/lights/<light-id> \
  -H 'content-type: application/json' \
  -d '{"on":1}'
```

Set brightness (0..100) and temperature (kelvin):

```bash
curl -s -X PUT http://127.0.0.1:9124/v1/lights/<light-id> \
  -H 'content-type: application/json' \
  -d '{"brightness":40,"kelvin":5500}'
```

Apply to all enabled lights:

```bash
curl -s -X PUT http://127.0.0.1:9124/v1/all \
  -H 'content-type: application/json' \
  -d '{"on":0}'
```

## Endpoint reference

### Health

**GET** `/v1/health`

Response:

```json
{ "status": "ok" }
```

### Lights (persisted)

**GET** `/v1/lights`

Returns the persisted lights list (including disabled lights).

**POST** `/v1/lights`

Add a light by IP (LAN addresses only).

Request:

```json
{ "ip": "192.168.1.106" }
```

Notes:
- This is mainly a fallback when mDNS discovery doesn’t work.
- Only private/LAN ranges are accepted (to avoid SSRF).

### Discovery / Refresh

**POST** `/v1/lights/refresh`

Triggers an mDNS scan and updates the persisted lights list.

Request (optional):

```json
{ "timeout": 3 }
```

### Current light states

**GET** `/v1/lights/states`

Returns current state for each enabled, reachable light.

### Enable/disable persisted light

**PUT** `/v1/lights/{id}/enabled`

Request:

```json
{ "enabled": true }
```

### Set alias (friendly name)

**PUT** `/v1/lights/{id}/alias`

Request:

```json
{ "alias": "left" }
```

Set `null` (or empty/whitespace) to clear.

### Update a single light

**PUT** `/v1/lights/{id}`

Request:

```json
{ "on": 1, "brightness": 50, "kelvin": 4500 }
```

Fields are optional:
- `on`: `0` or `1`
- `brightness`: `0..100`
- `kelvin`: `2900..7000`
- `mired`: `143..344` (alternative to `kelvin`)

Notes:
- Updates are sent to the physical light on your LAN (Elgato’s local API).
- If you send both `kelvin` and `mired`, `kelvin` is preferred.

### Groups

**GET** `/v1/groups`

**POST** `/v1/groups`

Request:

```json
{ "name": "office", "members": ["<light-id>", "<light-id>"] }
```

**PUT** `/v1/groups/{name}`

Same update request as a light (applies to members).

**DELETE** `/v1/groups/{name}`

Deletes a group.

### Update all lights

**PUT** `/v1/all`

Same update request as a light (applies to all enabled lights).

## Errors

Errors are JSON:

```json
{ "error": "message" }
```

Status codes:
- `400`: invalid request
- `404`: not found
- `413`: request body too large
- `429`: too many requests
- `500`: internal server error

## Practical notes for Open Deck / scripts

- **Light IDs**: Use the `id` returned by `GET /v1/lights` (it’s stable across IP changes).
- **Aliases**: You can show a friendly name using `alias` (set via `PUT /v1/lights/{id}/alias`).
- **Discovery vs control**:
  - Discovery persists lights (and updates IPs when they change).
  - Control endpoints operate on enabled lights and apply immediately.
