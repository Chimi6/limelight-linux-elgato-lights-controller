# LimeLight / `keylightd` API

`keylightd` exposes a small localhost HTTP API used by the LimeLight UI and intended for third-party integrations (e.g. Open Deck).

- **Base URL**: `http://127.0.0.1:9124`
- **Content-Type**: `application/json`
- **Notes**:
  - The daemon binds to **localhost** only.
  - Requests have a **64KiB** body limit.
  - Basic rate limiting exists (high enough to keep sliders responsive).

## Endpoints

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

