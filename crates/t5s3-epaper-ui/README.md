# t5s3-epaper-ui

Touchscreen UI firmware for the [LilyGo T5 E-Paper S3 Pro](https://lilygo.cc/products/t5-e-paper-s3-pro):
a wifi NTP clock, a LoRa keyboard messenger, GPS, SD-card wallpapers, and Music /
Environment pages that pull live data from [`noot-server`](https://github.com/noot/noot-server).
Built on [`t5s3-epaper-core`](../t5s3-epaper-core).

## flashing

The UI bakes wifi credentials in at build time, so configure them first (from the
workspace root):

```sh
cp .env.example .env    # then fill in SSID / PASSWORD / TZ_OFFSET_HOURS
```

The Music and Environment pages fetch JSON from `noot-server` over wifi, so also
set `SERVER_HOST` / `SERVER_PORT` (the server address) and `SENSOR_ID` (the
sensor device the Environment page reads) in `.env`.

Then flash and monitor:

```sh
just ui
# equivalent to:
SSID=… PASSWORD=… TZ_OFFSET_HOURS=… SERVER_HOST=… SERVER_PORT=… SENSOR_ID=… \
  cargo run -p t5s3-epaper-ui --features gps
```

notes:

- GPS support is optional — drop `--features gps` to build the UI without it (the
  GPS page then shows a "compile with --features gps" hint).
- The Music and Environment pages bring wifi up on entry to fetch from
  `noot-server`, then power the radio back down (tap anywhere to refresh). With
  the server unreachable they show an error line rather than blocking.
- The UI loads wallpapers as BMP files from `WALLS/` in the SD card root; use
  `tools/wallpaper` to prepare them.
