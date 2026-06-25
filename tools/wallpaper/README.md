# wallpaper-tool

Converts an image into a **540×960, 16-level grayscale, Floyd–Steinberg-dithered**
24-bit BMP for the LilyGo T5 S3 Paper Pro. Put the results in a `WALLS/` folder on
the SD card and `examples/ui.rs` (`show_wallpaper`) picks one at random as the
deep-sleep screensaver.

## Usage

```sh
tools/wallpaper/convert.sh <input-image> <output.bmp> [WxH]
# e.g.
tools/wallpaper/convert.sh ~/Pictures/photo.jpg LAIN1.BMP
```

- Input may be JPEG or PNG; it is center-cropped to fill the target size.
- Default size is `540x960` (the panel in portrait). Pass e.g. `960x540` for landscape.
- Paths resolve against your current directory.

## Putting them on the card

Create a folder named `WALLS` in the SD card root and drop the `.bmp` files in it.
Both the folder and the files must use **FAT 8.3 names** (≤8-char base, uppercase,
e.g. `LAIN1.BMP`, `SUNSET.BMP`) — the firmware opens them by short name, so a long
filename would be skipped. Any number of `.bmp` files works; one is chosen at
random on each sleep.
