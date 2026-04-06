# Add non-blocking `connect` variant

## Problem

`connect(None, timeout)` in `src/connect.rs` has a three-step fallback:

1. `DONGLORA_MUX_TCP` env var -> TCP mux
2. Unix socket mux (if socket file exists)
3. Direct USB serial via `find_port().unwrap_or_else(wait_for_device)`

Step 3 calls `wait_for_device()` which polls `find_port()` every 500ms
**indefinitely** until a USB device appears. This makes `connect(None, ...)`
unsuitable for callers that need to retry with backoff or report disconnected
status (e.g. donglora-bridge's reconnect loop).

Today, donglora-bridge works around this by manually reimplementing the
fallback chain:

```rust
if let Ok(c) = connect_mux_auto(timeout) {
    (c, "mux".to_string())
} else {
    let port_path = find_port()
        .ok_or_else(|| anyhow!("no mux or USB dongle found"))?;
    let c = connect(Some(&port_path), timeout)?;
    (c, shorten_path(&port_path))
}
```

This duplicates logic from `connect()` (steps 1-2 via `connect_mux_auto`, step
3 minus the blocking wait) and won't pick up any future changes to the
`connect()` fallback order.

## Proposed solution

Add a `try_connect` (or similar) that runs the same mux -> serial fallback as
`connect(None, timeout)` but replaces `wait_for_device()` with a single
`find_port()` call, returning an error immediately if no device is found:

```rust
/// Like [`connect`] but returns an error instead of blocking when no USB
/// device is present. Suitable for callers with their own retry logic.
pub fn try_connect(timeout: Duration) -> Result<Client<AnyTransport>> {
    // Steps 1-2: identical to connect() — try TCP mux, then Unix socket mux

    // Step 3: non-blocking USB scan
    let port_path = discovery::find_port()
        .ok_or_else(|| anyhow!("no DongLoRa device found (no mux, no USB device)"))?;
    let transport = SerialTransport::open(&port_path, timeout)?;
    Ok(Client::new(AnyTransport::Serial(transport)))
}
```

This would let donglora-bridge collapse its connection code to:

```rust
let (mut client, device) = if let Some(port) = port {
    let c = connect(Some(port), timeout)?;
    (c, shorten_path(port))
} else {
    let c = try_connect(timeout)?;
    let device = match c.transport() {
        AnyTransport::Mux(_) => "mux".to_string(),
        AnyTransport::Serial(_) => find_port()
            .map(|p| shorten_path(&p))
            .unwrap_or_else(|| "serial".to_string()),
    };
    (c, device)
};
```

### Device name from transport

The bridge also needs to know *what* it connected to for display purposes. Two
options:

1. Match on `AnyTransport` variant after connecting (shown above) -- works but
   requires a redundant `find_port()` call for the display name since
   `SerialTransport` has private fields.
2. Have `try_connect` return the port path alongside the client, e.g.
   `-> Result<(Client<AnyTransport>, ConnectionInfo)>` where `ConnectionInfo`
   is an enum like `Mux { socket_path }` | `Serial { port_path }`. Cleaner
   but a larger API change.

Either works. Option 2 would also benefit `connect()` callers who want to log
what they connected to.
