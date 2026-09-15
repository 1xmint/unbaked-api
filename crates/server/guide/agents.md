# Making and editing Unbaked files: a guide for AI agents

An Unbaked file is an ordinary PNG, M4A or MP4 that also carries its own
recipe: `recipe.json`, the assets it uses, and a fingerprint of the last render.
You never edit pixels or samples. You edit the recipe, render, and look at the
result. This guide is the whole loop, with the commands and the exact shapes of
what comes back. The format itself is in [SPEC.md](../SPEC.md), section 4.

## The loop

1. **Start** from an existing file, or write a recipe folder: `recipe.json` plus
   an `assets/` folder (SPEC section 3.3).
2. **Change** the recipe with a JSON Patch (`unbaked edit`), and bring in new
   pictures or sounds with `unbaked add`.
3. **Check** it: `unbaked check <file> --json`. After an edit it says `stale`;
   that is expected until you render.
4. **Look** before you render the real thing:
   - `unbaked preview <file> -o look.png` for a picture of the image, or of a
     video at `--at-ms`; `--sheet 9` for nine frames of a video in one grid.
   - `unbaked listen <file> --json` for numbers that describe the sound.
5. **Judge** what you saw against what was asked. If it is wrong, go back to 2.
6. **Render**: `unbaked render <file>` (in place) or `-o <new file>`. `check`
   now says `fresh`.

Every command takes `--json` and then prints exactly one JSON object on stdout.

## Commands

| Command | What it does |
|---|---|
| `check <file> --json` | `fresh`, `stale` (with what changed), `render-modified` or `invalid` (with problems) |
| `recipe <file>` | Prints `recipe.json` |
| `edit <file-or-dir> --patch <file\|-> [-o out]` | Applies a JSON Patch to `recipe.json` |
| `add <file-or-dir> <media> --id <id> [-o out]` | Packs a picture, video, sound or font as asset `<id>` |
| `estimate <file-or-dir>` | The size of the render before it runs |
| `preview <file-or-dir> -o out.png [--at-ms N] [--max-edge N] [--sheet N]` | A plain PNG to look at |
| `listen <file-or-dir>` | Length, peak, clipping, loudness per 500 ms, silences |
| `render <file-or-dir> [-o out]` | Writes the finished file |
| `unpack <file> <dir>` / `pack <dir> --into <file>` | Between a file and a folder |

`edit` and `add` change the file or folder in place unless `-o` is given. With
a folder, `-o` names a new folder. `render`, `preview` and `listen` need
`--fonts <dir>` when the recipe refers to fonts it does not pack.

Supported assets: PNG, JPEG and still WebP pictures; MP4 (H.264) video; M4A
(AAC), MP3, WAV and FLAC sound; TrueType and OpenType fonts. The kind comes from
the file's bytes, not its name. A packed font needs a licence:
`add card font.ttf --id title_font --license license.json`, where the JSON is
for example `{"spdx": "OFL-1.1", "file": "assets/OFL.txt"}` (add the licence
text file too, or use a licence that does not require it).

## Reading results

Success always has `"ok": true`:

```json
{ "ok": true, "output": "poster.unbaked.png", "bytes": 48213, "elapsed_ms": 91 }
```

Failure has `"ok": false` and one error:

```json
{
  "ok": false,
  "error": {
    "kind": "invalid",
    "message": "the edited recipe.json is invalid",
    "problems": [
      { "file": "recipe.json", "path": "/layers/2/asset", "message": "there is no asset \"hero\"" }
    ]
  }
}
```

`path` is a JSON Pointer into `file`. When `file` is `"patch"`, it points at
your patch: `/1` is its second operation, and the message names the recipe
location that operation touched. Fix exactly what the problems name.

| `kind` | Meaning | Exit code |
|---|---|---|
| `invalid` | The recipe, or your patch, breaks a rule; see `problems` | 2 |
| `format` | Not an Unbaked file, or its package is damaged | 3 |
| `unsupported` | Something this renderer cannot do, or a file type it does not take | 3 |
| `decode` | An asset file is damaged | 3 |
| `font-not-found` | A referenced font is not in `--fonts` | 3 |
| `over-limit` | Too many pixels, samples or frames for the limits | 3 |
| `encode` | The result could not be written | 3 |
| `timed-out` | The time limit passed | 5 |
| `io` | A file could not be read or written | 4 |
| `usage` | Wrong arguments | 4 |

`check` exits 0 for fresh, 1 for stale or render-modified, 2 for invalid.

## Limits

`render`, `preview` and `listen` stop cleanly instead of running away:

- `--time-limit-ms N`: stop once the work has run this long (`timed-out`).
- `--max-pixels N`: most pixels in any one picture buffer.
- `--max-samples N`: most samples per channel in any one sound buffer.
- `--max-frames N`: most frames in a video.

Cost grows with more than size. A blur's cost grows with its `sigma`, so a large
blur on a large layer is slow. Run `estimate` first when cost matters: its
`work_units` is one number for the whole job.

## Editing with JSON Patch

A patch is a list of operations (RFC 6902): `add`, `remove`, `replace`, `move`,
`copy` and `test`. Paths are JSON Pointers; array items are numbered from 0, and
`/layers/-` means "after the last layer". The whole patch applies or nothing
changes, and the result must be a valid recipe.

Layers are found by position, and positions shift when layers are added or
removed. **Start each patch with a `test` of the layer's `id`**, so a stale index
fails loudly instead of changing the wrong layer. Read the current recipe with
`unbaked recipe <file>` before writing a patch.

### Change a headline

```json
[
  { "op": "test", "path": "/layers/3/id", "value": "headline" },
  { "op": "replace", "path": "/layers/3/text", "value": "Summer sale ends Friday" }
]
```

### Move a layer

`x` and `y` place the layer's anchor on the canvas, in pixels from the top left.

```json
[
  { "op": "test", "path": "/layers/1/id", "value": "logo" },
  { "op": "replace", "path": "/layers/1/transform/x", "value": 1600 },
  { "op": "replace", "path": "/layers/1/transform/y", "value": 80 }
]
```

If the layer has no `transform` yet, `add` the whole object instead:
`{ "op": "add", "path": "/layers/1/transform", "value": { "x": 1600, "y": 80 } }`.

### Swap a background image

Replace the asset's file, keeping its id, so every layer using it changes:

```sh
unbaked add poster.unbaked.png new-beach.webp --id background
unbaked render poster.unbaked.png
```

Or add the new picture under a new id and point one layer at it:

```sh
unbaked add poster.unbaked.png new-beach.webp --id beach2
```

```json
[
  { "op": "test", "path": "/layers/0/id", "value": "background" },
  { "op": "replace", "path": "/layers/0/asset", "value": "beach2" }
]
```

### Add a voice-over and lower the music under it

```sh
unbaked add promo.unbaked.mp4 voice.mp3 --id voice
```

The voice starts at 2 s and lasts about 6 s. The music (`/audio/0`) dips 12 dB
from just before the voice to just after it. `gain_db` keyframe times count from
the clip's own `start_ms`.

```json
[
  { "op": "test", "path": "/audio/0/id", "value": "music" },
  { "op": "add", "path": "/audio/-", "value": {
      "id": "voiceover", "asset": "voice", "start_ms": 2000, "fade_in_ms": 100, "fade_out_ms": 200 } },
  { "op": "add", "path": "/audio/0/gain_db", "value": { "keys": [
      { "t_ms": 1500, "v": 0, "ease": "ease-in-out" },
      { "t_ms": 2000, "v": -12 },
      { "t_ms": 8000, "v": -12, "ease": "ease-in-out" },
      { "t_ms": 8500, "v": 0 } ] } }
]
```

Then `unbaked listen promo.unbaked.mp4 --json`: `clipped_samples` should be 0,
and `loudness_dbfs` should not jump where the voice comes in. If it clips, lower
the voice clip's `gain_db`.

### Turn a video into a still

The scene stays; only the export changes. This takes the 4-second mark:

```json
[
  { "op": "replace", "path": "/output/kind", "value": "image" },
  { "op": "add", "path": "/output/at_ms", "value": 4000 }
]
```

A still is a PNG, so render it to a new name:
`unbaked render promo.unbaked.mp4 -o still.unbaked.png`. Changing `kind` back to
`"video"` makes the video again.

## Sizing solids

Give a solid its size with `width` and `height`, not by scaling a small one:
a 1×1 solid scaled up 500 times gets soft edges, while a 500×500 solid is sharp.

```json
{ "id": "banner", "type": "solid", "color": "#e03020ff", "width": 1920, "height": 160,
  "transform": { "x": 0, "y": 920 } }
```

## Looking well

- Preview at the moment that matters: `--at-ms` for when a title is fully in,
  not the first frame of its fade.
- For motion, a `--sheet 9` or `--sheet 16` grid shows the whole video at once.
- Previews are at most 1024 pixels on the longest edge by default; small text
  may need `--max-edge 2048` to read.
- `listen` reports levels in dBFS: 0 is the loudest possible, -6 is half the
  amplitude, `null` is silence. `silences` lists quiet stretches of 250 ms or
  more, which helps find a clip that starts too late.
