set dotenv-load

# flash the wifi clock example (SSID/PASSWORD/TZ_OFFSET_HOURS from .env)
clock:
    # re-touch so the env-baked credentials pick up any .env changes
    touch crates/t5s3-epaper-core/examples/clock.rs
    SSID="$SSID" PASSWORD="$PASSWORD" TZ_OFFSET_HOURS="$TZ_OFFSET_HOURS" cargo run -p t5s3-epaper-core --example clock --features wifi

# flash the touchscreen ui (gps + lora keyboard + wifi status-bar clock; creds/tz from .env)
ui:
    # re-touch so the env-baked credentials pick up any .env changes
    touch crates/t5s3-epaper-ui/src/main.rs
    SSID="$SSID" PASSWORD="$PASSWORD" TZ_OFFSET_HOURS="$TZ_OFFSET_HOURS" SERVER_HOST="$SERVER_HOST" SERVER_PORT="$SERVER_PORT" SENSOR_ID="$SENSOR_ID" FUNNEL_HOST="${FUNNEL_HOST:-}" SERVER_TOKEN="${SERVER_TOKEN:-}" ESP_LOG="${ESP_LOG:-}" cargo run -p t5s3-epaper-ui --features gps

# flash the ble ⇄ lora bridge example (faster scheduler tick so the radio thread can service advertising)
ble:
    ESP_RTOS_CONFIG_TICK_RATE_HZ=1000 cargo run -p t3s3-epaper --example ble

# check that everything compiles
check:
    cargo c -p t5s3-epaper-core --examples --all-features
    cargo c -p t5s3-epaper-ui --all-features
    cargo c -p t3s3-epaper --examples

# format and lint
lint:
    cargo fmt
    cargo clippy --workspace --all-features

# host unit tests for the standalone epub-reader crate (RUSTFLAGS cleared so the
# root's -nostartfiles link arg doesn't leak into the host test binary)
test-epub:
    cd crates/epub-reader && RUSTFLAGS= cargo +stable test
