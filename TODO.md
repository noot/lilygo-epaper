# TODO

## from factory firmware reference

hardware reference: [T5S3-4.7-e-paper-PRO](https://github.com/Xinyuan-LilyGO/T5S3-4.7-e-paper-PRO)

### high priority

- [x] **GPS** — MIA-M10Q (u-blox) or L76K (Quectel), UART TX=GPIO43 RX=GPIO44, power via PCA9535 IO0_0
- [x] **LoRa** — SX1262, SPI CS=GPIO46 IRQ=GPIO10 RST=GPIO1 BUSY=GPIO47, shares SPI bus with SD card
- [x] **front light** — PT4103B23F, PWM on GPIO11
- [ ] **external RTC** — PCF85063 (or PCF8563), I2C 0x51 IRQ=GPIO2 (lib currently uses ESP32 internal RTC only)

### medium priority

- [x] **SPI bus arbitration** — LoRa CS must be held high during SD card access (shared bus)
- [ ] **BQ25896 full driver** — charge current config, charge status, input power path management (currently shutdown only)
- [ ] **BQ27220 full driver** — current draw, temperature, state of health, remaining/full capacity (currently voltage + SOC only)
- [x] LoRa user message (with keyboard) and broadcast

### low priority / nice to have

- [x] **WiFi** — via `esp-wifi` crate
- [ ] **BLE** — via `esp-wifi` crate
- [ ] **waveform LUT temperature compensation** — use TPS65185 temp sensor to select waveform table at draw time
- [x] join wifi network page/setting, displays current connection if any

## file handling and management
- [x] sd card file browser in UI
- [x] open bmp images in file browser
- [x] epub/txt/markdown reader (lazy per-chapter epub parsing, JPEG/PNG image rendering with dithering, content-hashed resume)

## GPS
- [x] cached map with location rendering

## LoRa
- [ ] define message types (protos?)
- [ ] auto send coordinates in one click
- [ ] automatically calculate distance from remote peer when coordinate message is received
- [ ] always-on lora receive functionality with unread notification icon

## reader
- [x] map auxiliary button to "next page" when in reader
- [x] make dedicated reader page that shows currently reading, progress, covers, etc
- [x] update chapter numbers to match actual book chapters
- [ ] support more scripts — Greek, CJK, Hebrew, etc. currently render as `?` (need additional u8g2 fonts added to `pick_font` fallback chain)
- [ ] stream epub chapters from SD instead of holding the whole compressed file in PSRAM (for large/image-heavy books)

## other
- [ ] turn ui into its own library; have a UiBuilder with pluggable components
- [x] page icons for ui homepage
- [x] user settings persisted
- [x] customizable fonts/font size
- [x] customizable timezone
- [x] pull now playing from music server (see waveshare-epaper repo)
- [x] pull weather data and display on ui page
- [x] pull home environment data (temp, humidity, co2)
- [x] notes app - stores notes on sd card

# bug fixes

- [x] sleep doesn't properly deep sleep - battery still drains quite a lot
- [x] battery percentage only refreshes when a new page is loaded
- [ ] keyboard "send" button is unresponsive at times (lower right corner issue?)
- [x] if clock fails to sync it doesn't resync - have button in info to try to sync (Sync clock button on the wifi settings page forces a re-sync / internet check)
- [x] saved wifi networks don't store password on disconnect - last 5 joined networks are now remembered, tap to reconnect
