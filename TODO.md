# TODO

## from factory firmware reference

hardware reference: [T5S3-4.7-e-paper-PRO](https://github.com/Xinyuan-LilyGO/T5S3-4.7-e-paper-PRO)

### high priority

- [x] **GPS** — MIA-M10Q (u-blox) or L76K (Quectel), UART TX=GPIO43 RX=GPIO44, power via PCA9535 IO0_0
- [x] **LoRa** — SX1262, SPI CS=GPIO46 IRQ=GPIO10 RST=GPIO1 BUSY=GPIO47, shares SPI bus with SD card
- [x] **front light** — PT4103B23F, PWM on GPIO11
- [ ] **external RTC** — PCF85063 (or PCF8563), I2C 0x51 IRQ=GPIO2 (lib currently uses ESP32 internal RTC only)

### medium priority

- [ ] **SPI bus arbitration** — LoRa CS must be held high during SD card access (shared bus)
- [ ] **BQ25896 full driver** — charge current config, charge status, input power path management (currently shutdown only)
- [ ] **BQ27220 full driver** — current draw, temperature, state of health, remaining/full capacity (currently voltage + SOC only)
- [x] LoRa user message (with keyboard) and broadcast

### low priority / nice to have

- [x] **WiFi** — via `esp-wifi` crate
- [ ] **BLE** — via `esp-wifi` crate
- [ ] **waveform LUT temperature compensation** — use TPS65185 temp sensor to select waveform table at draw time

## file handling and management
- [x] sd card file browser in UI
- [x] open bmp images in file browser
- [ ] epub reader functionality
- [ ] always-on lora receive functionality with unread notification icon

## GPS
- [ ] cached map with location rendering

## LoRa
- [ ] define message types (protos?)
- [ ] auto send coordinates in one click
- [ ] automatically calculate distance from remote peer when coordinate message is received

## other
- [ ] turn ui into its own library; have a UiBuilder with pluggable components
- [ ] page icons for ui homepage
- [ ] customizable fonts/font size
- [ ] customizable timezone
- [ ] pull now playing from music server (see waveshare-epaper repo)
- [ ] pull weather data and display on ui page
- [ ] pull home environment data (temp, humidity, co2)

# bug fixes

- [ ] sleep doesn't properly deep sleep - battery still drains quite a lot
