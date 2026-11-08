**A fast, native subtitle editor written in Rust, with a full command-line toolkit included.**

SubForge is a desktop app for creating, timing, typesetting, translating and checking subtitles (SRT, ASS/SSA and WebVTT). It started as a replacement for the usual mix of subtitle apps, and it puts editing, video preview with real libass rendering, a waveform/spectrogram timeline, quality checks, OCR, speech-to-text and MKV muxing into one window.

Everything the GUI can do to a file is also available from `subforge-cli`. You can script it, run it in batch jobs, or connect it to other tools (a translation pipeline, for example) through simple JSON in and out.

It has particularly good support for **Thai** (and other languages written without spaces between words): line breaking that never splits a word, character counts by grapheme so vowels and tone marks are counted correctly, a Thai spell checker, and fixes for common Thai typing and OCR errors.

---

## Table of contents

- [Highlights](#highlights)
- [Project layout](#project-layout)
- [Building from source](#building-from-source)
- [Installing](#installing)
- [Optional external tools](#optional-external-tools)
- [Using the editor](#using-the-editor)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Using the CLI](#using-the-cli)
- [Integrating with other programs](#integrating-with-other-programs)
- [Using `subforge-core` as a library](#using-subforge-core-as-a-library)
- [Where SubForge stores things](#where-subforge-stores-things)
- [Known limitations](#known-limitations)
- [License](#license)

---

## Highlights

**Editing**
- Reads and writes **SRT, ASS, SSA and WebVTT**, and exports plain text. HTML-style tags (`<i>`, `<b>`, `<font color>`) and ASS override tags are converted automatically when you switch formats.
- **Lossless ASS round-trip.** Unknown sections (`[Fonts]`, `[Graphics]`, `[Aegisub Project Garbage]`), custom `Format` orders, unknown Script Info keys, layers, effects and comments are all kept. Tests check that lines with drawings, vector clips, nested `\t` and karaoke come back byte-for-byte identical.
- **Detects text encoding** automatically (BOM, UTF-8, Windows-874/TIS-620, Shift_JIS, and more) and can save as UTF-8, UTF-8 with BOM, UTF-16 or Windows-874.
- A virtualised subtitle grid that stays smooth with thousands of lines, multi-select, insert/duplicate/split/join/comment, find and replace (regex, whole word, per field), and filtering.
- **Diff-based undo/redo.** Only the lines that changed are stored, so 200 edits on a 5,000-line file take about 90 KB instead of hundreds of MB.
- A style manager. Renaming a style also updates every line that uses it.

**Video and timeline**
- Video and audio preview through ffmpeg, with audio as the master clock and exact frame stepping. A long-lived decoder and an LRU frame cache (~256 MB) make stepping back and forth almost instant.
- **Real libass rendering** both while paused and while playing, so `\t`, `\frz`, karaoke, drawings and fonts embedded in the MKV look the way they will in a player.
- A timeline showing the **waveform and/or a mel spectrogram**, scene-change **keyframes**, snapping (to keyframes, other lines and the playhead), and drag-to-retime.
- Timing tools: shift (all lines, the selection, or from a given line), frame-rate conversion, two-point linear sync, snap to frames or keyframes, and set start/end from the video position.

**Typesetting**
- On-video tools: **Move** (`\pos`/`\move`), **Rotate** (`\frz`), **Scale** (`\fscx`/`\fscy`) and **Clip** (rectangle or vector `\clip`/`\iclip`). They work on every selected line, and one drag is one undo step.
- **Tween/motion panel:** animate position, scale, rotation, font size, border, blur, opacity and colour with 9 easing curves (each with In/Out/In-Out variants). It can output either compact `\move`+`\t` tags or frame-by-frame lines, and comes with 10 presets (slide in, pop, zoom out, spin, drop and bounce, …). Settings are stored inside the line, so you can reopen an animation and edit it later.
- **Motion tracking:** click a feature in the video and SubForge generates a per-frame `\pos` so the subtitle follows it. Track a second point to get scale and rotation as well.
- **Font checker:** finds every font the script really uses, resolving names the same way libass does, and tells you whether each one is found, faked (synthetic bold/italic) or missing. It can also collect all the font files into one folder.
- **Resample resolution:** change PlayRes and rescale styles, positions, clips, `\t` values and drawings so the result looks the same.

**Quality and cleanup**
- **Live QC:** characters per second, line length, number of lines, minimum and maximum duration, overlaps, gaps, empty lines, unbalanced tags, missing styles. Presets are included for Netflix-style, Thai, Japanese and children's content.
- **Auto-fix:** trims spaces, removes empty lines, fixes overlaps, enforces gaps, extends lines that are too short or too fast to read, and breaks long lines.
- **Fix common errors:** Thai sara am (`ํา` → `ำ`), `เเ` → `แ`, vowel/tone-mark order, invisible characters, duplicate words, punctuation spacing and more. You review a before/after preview for each line.
- **Spell check** with built-in English and Thai dictionaries (~50k Thai words) and a personal dictionary.
- More tools: adjust durations (by ms, %, a fixed length or a target CPS), bridge gaps, remove hearing-impaired text, change case, and merge lines with the same text.

**Getting subtitles in and out**
- **Extract subtitle tracks** from MKV/MP4/TS. ASS tracks are stream-copied without changes, and attached fonts are picked up automatically.
- **OCR image subtitles** (Blu-ray PGS and DVD VobSub) with Tesseract, then review the results side by side with the images. Thai words the OCR got wrong are repaired using the dictionary.
- **Transcribe** speech to subtitles locally with whisper.cpp, with no API costs. Models can be downloaded from inside the app.
- **Sync to speech:** fixes the offset *and* the frame-rate drift of a subtitle in *any* language (for example a Thai subtitle against Japanese audio) by matching it to detected speech.
- **Mux into MKV:** put the subtitle and its fonts back into the video. Nothing is re-encoded, so it takes seconds.
- **Translation mode:** load a reference subtitle to see the original text above the line you are typing.
- **Batch convert** whole folders, optionally with shift, fps change, auto-fix, tag stripping and an encoding choice.

**Workspace**
- Dockable panels: drag tabs to split or merge them, close and reopen them from the *View* menu. The layout is remembered between runs.
- Dark and light themes, UI scaling, recent files, autosave backups, and a warning before closing with unsaved changes.
- Background loading with a progress screen (reading the file, detecting the encoding, extracting tracks, keyframes, waveform), plus a *Continue now* button if you don't want to wait.

---

## Project layout

SubForge is a Cargo workspace with three crates:

```
subforge/
├─ Cargo.toml                 workspace manifest
└─ crates/
   ├─ subforge-core/          the library: parsing, writing, timing, QC, fixes, OCR, speech, fonts...
   │  └─ dicts/               bundled Hunspell dictionaries (en_US, th_TH)
   ├─ subforge-gui/           the desktop editor (egui/eframe) → binary: subforge
   └─ subforge-cli/           the command-line tool (clap)    → binary: subforge-cli
```

All the actual subtitle logic lives in `subforge-core`. The GUI and the CLI are thin front ends over it, so a file processed with the CLI comes out the same as one edited in the GUI.

---

## Building from source

### 1. Install Rust

You need a recent stable Rust toolchain (the workspace uses edition 2021, and dependencies such as egui 0.37 and ICU 2 need a modern compiler, so just use the latest stable).

Install it from <https://rustup.rs>, or update an existing install:

```bash
rustup update stable
```

### 2. Platform prerequisites

**Windows.** Install the *Visual Studio Build Tools* with the "Desktop development with C++" workload (rustup offers to do this for you). Nothing else is needed.

**macOS.** Install the Xcode command line tools:

```bash
xcode-select --install
```

**Linux.** The GUI needs development headers for ALSA (audio), X11/Wayland and OpenGL. On Debian/Ubuntu:

```bash
sudo apt install build-essential pkg-config libasound2-dev \
    libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
    libxkbcommon-dev libwayland-dev libgl1-mesa-dev
```

On Fedora:

```bash
sudo dnf install gcc pkgconf-pkg-config alsa-lib-devel \
    libX11-devel libXcursor-devel libXrandr-devel libXi-devel \
    libxkbcommon-devel wayland-devel mesa-libGL-devel
```

The CLI alone has no system dependencies.

### 3. Build

```bash
git clone https://github.com/<your-username>/subforge.git
cd subforge
cargo build --release
```

The release profile uses thin LTO and a single codegen unit, so the first build takes a few minutes. When it finishes you will find:

| File | What it is |
|---|---|
| `target/release/subforge` (`subforge.exe` on Windows) | The editor (GUI) |
| `target/release/subforge-cli` (`subforge-cli.exe`) | The command-line tool |

To build only one of them:

```bash
cargo build --release -p subforge-cli   # CLI only, no GUI dependencies
cargo build --release -p subforge-gui   # GUI only
```

To run without installing:

```bash
cargo run --release -p subforge-gui -- episode01.ass --video episode01.mkv
cargo run --release -p subforge-cli -- info episode01.srt
```

### Cargo features

`subforge-core` has one feature, `thai-segment` (on by default). It enables dictionary-based word breaking for Thai, Lao, Khmer and Burmese through ICU, which line balancing and auto-fix use to break lines without cutting a word in half. You can turn it off for a smaller build if you never work with those languages:

```toml
subforge-core = { path = "crates/subforge-core", default-features = false }
```

### Running the tests

```bash
cargo test --workspace
```

---

## Installing

There is no installer. SubForge is two self-contained executables.

**Windows.** Copy `subforge.exe` and `subforge-cli.exe` to a folder of your choice, for example `C:\Tools\SubForge\`. The easiest setup is to put the ffmpeg binaries in the same folder (see below). If you want to call `subforge-cli` from any terminal, add that folder to your `PATH`.

**Linux / macOS.** Copy the binaries somewhere on your `PATH`:

```bash
install -Dm755 target/release/subforge     ~/.local/bin/subforge
install -Dm755 target/release/subforge-cli ~/.local/bin/subforge-cli
```

Or let Cargo install them into `~/.cargo/bin`:

```bash
cargo install --path crates/subforge-gui
cargo install --path crates/subforge-cli
```

---

## Optional external tools

The subtitle editor itself works with no extra software. Some features call well-known external programs. SubForge looks for each one **next to the SubForge executable, in an `ffmpeg/` or `ffmpeg/bin/` folder next to it, or on your `PATH`**, and you can also point to them in *File ▸ Settings*. CLI commands take `--ffmpeg`, `--ffprobe`, `--tesseract` and `--whisper` flags for the same purpose.

| Tool | Needed for | Where to get it |
|---|---|---|
| **ffmpeg + ffprobe** | Video preview, waveform and spectrogram, keyframes, extracting tracks from video, muxing, speech features | Windows: builds from [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) or [BtbN](https://github.com/BtbN/FFmpeg-Builds/releases). Linux: your package manager. macOS: `brew install ffmpeg` |
| ffmpeg **with libass** | Exact subtitle rendering in the preview | The builds above already include it. Without libass, a simpler overlay is drawn instead (see limitations) |
| **Tesseract** | OCR of PGS/VobSub image subtitles | Windows: [UB-Mannheim installer](https://github.com/UB-Mannheim/tesseract/wiki) (tick *Thai* and any other languages you need during setup). Linux: e.g. `apt install tesseract-ocr tesseract-ocr-tha` |
| **whisper.cpp** (`whisper-cli`) | Transcribing speech, re-timing text against audio, and whisper-based sync | [whisper.cpp releases](https://github.com/ggml-org/whisper.cpp/releases) (CPU, CUDA and Vulkan builds). Put `whisper-cli` next to SubForge or ffmpeg. Models (`ggml-*.bin`, tiny → large-v3-turbo) can be downloaded from *Tools ▸ Speech* |

*Sync to speech* with the built-in voice detector does **not** need whisper. Only ffmpeg is required to decode the audio.

---

## Using the editor

Start SubForge by double-clicking it, or from a terminal:

```bash
subforge                                  # empty editor
subforge episode01.ass                    # open a subtitle
subforge episode01.ass --video episode01.mkv
subforge episode01.mkv                    # open a video (you'll be offered its subtitle tracks)
```

If you open a subtitle and a video with the same name sits next to it, the video is opened automatically. You can also drag and drop subtitle and video files onto the window.

### A typical workflow

1. **Open** a subtitle with *File ▸ Open*, or pull one out of a video with *File ▸ Import subtitles from video…*. SubForge loads it in the background and shows each step. Encoding is detected for you.
2. **Watch and time it.** Press `Space` to play, click the timeline to seek, and drag the edges of a block to retime it. `F7`/`F8` set the current line's start/end to the video position. `F6` snaps the selected lines to nearby keyframes.
3. **Edit text** in the line editor. `Ctrl+Enter` moves to the next line (and creates one at the end), which makes it fast to type a translation line by line. For translation, *Translate ▸ Load reference* shows the original text above the box you are typing in.
4. **Typeset** with the on-video tools (`M` move, `R` rotate, `S` scale, `C` clip, `T` track) and the *Position & Motion* panel.
5. **Check** with the *Quality check* panel (`F9`). Problems are also coloured live in the grid, the timeline and the editor. Run auto-fix, *Fix common errors* or spell check (`Ctrl+F7`) to clean up.
6. **Check fonts** (*Typeset ▸ Fonts…*) before you publish, then **save**, or **mux** the subtitle and its fonts straight into an MKV.

### Transcribing from scratch

*Tools ▸ Speech* → choose a whisper model (download it there if needed) → choose the language → **Transcribe**. Lines are split at pauses and sentence ends. Thai and Japanese are split at real word boundaries. You can add character names to the prompt to help recognition.

### OCR of Blu-ray/DVD subtitles

Open the MKV/M2TS/MP4, `.sup` or `.idx` file, choose the image track, choose the OCR languages (e.g. `tha+eng`) and run it. A review window shows each image next to the recognised text, highlights lines the OCR was unsure about, and lets you correct them before importing.

---

## Keyboard shortcuts

| Key | Action |
|---|---|
| `Space` / `Alt+Space` | Play / pause |
| `←` `→` (with `Shift` = ×5) | Seek |
| `Ctrl+←` / `Ctrl+→` | Step one frame back / forward |
| `F5` / `Ctrl+P` | Play the current line |
| `F7` / `Ctrl+[` | Set start = video time |
| `F8` / `Ctrl+]` | Set end = video time |
| `↑` `↓` (`Ctrl+↑↓` while typing) | Previous / next line |
| `Enter` | Focus the text box |
| `Ctrl+Enter` | Next line (creates a new one at the end) |
| `Ctrl+I` / `Ctrl+D` / `Ctrl+K` / `Ctrl+J` | Insert / duplicate / split / join |
| `Alt+←` / `Alt+→` | Previous / next keyframe |
| `F6` | Snap selected lines to keyframes |
| `M` `R` `S` `C` `T` · `Esc` | Video tools: Move, Rotate, Scale, Clip, Track · back to Pointer |
| `Ctrl+F` / `Ctrl+H` | Find / replace |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| `Ctrl+S` | Save |
| `Ctrl+F7` | Spell check |
| `F9` | Quality check panel |

On the timeline: `Ctrl+wheel` zooms, holding `Alt` while dragging turns snapping off, and right-clicking gives you insert/split/set-time commands at that point.

---

## Using the CLI

`subforge-cli` exposes the same engine as the editor. Run `subforge-cli --help` for the full list of commands, or `subforge-cli <command> --help` for one command's options.

A few rules apply to every command:

- Any input file can be `-` to read from **stdin**.
- Without `-o`, the result is written to **stdout**, so commands can be piped together. Use `--in-place` to overwrite the input instead.
- Times can be written as `1.5s`, `-250ms`, `0:01:02.500` or `-00:00:01,200`.
- Line selections are 1-based ranges, such as `--lines 5,7,10-20`.
- Commands that check things (`check`, `fonts`, `spell`) exit with **code 2** when they find problems, which is handy in scripts and CI. Most commands accept `--json` for output that programs can read.

### Commands

| Group | Command | What it does |
|---|---|---|
| Inspect | `info` | Format, encoding, line count and statistics |
| | `check` | Quality check (CPS, line length, durations, overlaps, gaps…) |
| | `dump` | The whole file as JSON |
| | `qc-config` | Print the default QC settings as JSON (edit them and pass `--qc-config`) |
| Convert | `convert` | Change format and/or encoding |
| | `batch` | Convert/process many files or whole folders |
| Timing | `shift` | Move times by an offset |
| | `fps` | Frame-rate conversion |
| | `sync` | Sync using reference points (1 point = shift, 2 = linear, more = piecewise) |
| | `keyframes` | Export keyframes from a video (Aegisub format or ms) |
| | `snap-kf` | Snap line starts/ends to nearby keyframes |
| | `durations` | Lengthen/shorten lines by ms, %, a fixed length or a target CPS |
| | `bridge-gaps` | Close small gaps between lines |
| | `sync-speech` | Fix offset and drift automatically by matching to detected speech |
| Text | `fix` | Auto-fix timing and text issues (`--rebalance` breaks long lines) |
| | `fix-errors` | Fix common errors (Thai typing mistakes, spaces, punctuation…) |
| | `find` | Find and optionally replace text |
| | `spell` | Spell check (English + Thai built in) |
| | `remove-hi` | Remove hearing-impaired text |
| | `case` | Change case: upper, lower, sentence, title |
| Typeset | `pos` | Set position/alignment, nudge, or clear positioning |
| | `tween` | Animate lines with easing (or apply a preset) |
| | `track` | Motion-track points in the video and attach lines to them |
| | `resample` | Change script resolution and rescale everything |
| | `fonts` | Check the fonts a script uses, or collect them into a folder |
| Video | `tracks` | List subtitle tracks in a video |
| | `extract` | Extract a subtitle track (ASS is copied without changes) |
| | `mux` | Put subtitles and fonts into an MKV without re-encoding |
| | `ocr` | OCR image subtitles (PGS/VobSub) with Tesseract |
| | `transcribe` | Speech → subtitles with whisper.cpp |
| | `align` | Re-time lines against the audio |
| Integration | `apply` | Apply JSON edits back onto a file |

### Examples

```bash
# Inspect and convert
subforge-cli info ep01.srt --json
subforge-cli convert ep01.srt -o ep01.ass
subforge-cli convert old.srt --in-encoding windows-874 -o new.srt --encoding utf-8-bom

# Convert a whole folder (recursively) to SRT with a ".th" suffix, auto-fixing on the way
subforge-cli batch subs/ --to srt --out-dir out/ -r --suffix .th --fix

# Timing
subforge-cli shift ep01.ass -1.2s --in-place
subforge-cli shift ep01.ass 500ms --after 0:10:00 --in-place
subforge-cli fps ep01.srt --from-fps 23.976 --to-fps 25 -o ep01.pal.srt
subforge-cli sync ep01.srt --point '#1=0:00:05.200' --point '#350=0:23:41.000' --in-place
subforge-cli snap-kf ep01.ass --video ep01.mkv --in-place
subforge-cli sync-speech ep01.th.ass --video ep01.mkv --snap 250ms -o ep01.fixed.ass

# Quality
subforge-cli check ep01.ass --max-cps 20 --max-line-len 42
subforge-cli fix ep01.ass --enforce-gap --rebalance --in-place
subforge-cli fix-errors ep01.th.ass --dry-run
subforge-cli spell ep01.th.ass --lang en,th --user-dict words.txt

# Typesetting
subforge-cli pos ep01.ass --lines 12 --x 960 --y 120 --an 8 --in-place
subforge-cli tween ep01.ass --lines 12 --preset drop --in-place
subforge-cli tween ep01.ass --lines 5-9 --from-offset -200,0 --fade-from \
    --ease expo-out --duration 500ms --fbf --fps 23.976 --in-place
subforge-cli resample ep01.ass 1920x1080 --in-place

# Video
subforge-cli tracks ep01.mkv
subforge-cli extract ep01.mkv --track 1 -o ep01.ass
subforge-cli fonts ep01.ass --video ep01.mkv
subforge-cli mux ep01.mkv ep01.th.ass -o ep01.th.mkv --lang tha --title "Thai" --default

# Speech and OCR
subforge-cli transcribe ep01.mkv --model ggml-large-v3-turbo.bin --lang ja -o ep01.ja.srt
subforge-cli ocr ep01.mkv --track 3 --lang tha+eng -o ep01.th.srt
```

Tween presets: `slide-left`, `slide-right`, `slide-top`, `slide-bottom`, `pop`, `zoom-out`, `fade-rise`, `spin`, `drop`, `pulse`.

---

## Integrating with other programs

`dump` and `apply` let another program (a translator, a script, another app) work on the text without having to parse subtitle formats itself:

```bash
# 1. Export the text. "plain" holds each line as plain text with tags removed
subforge-cli dump ep01.ass --stats > ep01.json

# 2. ...translate or process ep01.json in your own program...

# 3. Write the changes back. Styles, tags and timing that you didn't touch are kept
echo '[{"index":0,"plain":"Hello\nsecond line"},{"index":5,"delete":true}]' \
  | subforge-cli apply ep01.ass --edits - -o ep01.en.ass
```

`apply` accepts either:

- a **list of edits**, each with an `index` and any of `text`, `plain`, `start`, `end`, `style`, `actor`, `comment`, `delete`; or
- a **full document** as produced by `dump`, with its `events` modified.

---

## Using `subforge-core` as a library

You can also link the core crate directly from another Rust project:

```toml
[dependencies]
subforge-core = { path = "../subforge/crates/subforge-core" }
```

```rust
use subforge_core::{qc, timing};

fn main() -> anyhow::Result<()> {
    let mut loaded = subforge_core::load("ep01.srt", None)?;           // auto-detects encoding
    timing::shift(&mut loaded.sub, 1500, &Default::default(), timing::Field::Both);
    for issue in qc::check(&loaded.sub, &Default::default()) {
        println!("{issue:?}");
    }
    subforge_core::save("ep01.ass", &loaded.sub, None, &Default::default())?;
    Ok(())
}
```

---

## Where SubForge stores things

| What | Windows | Linux | macOS |
|---|---|---|---|
| Cache (waveforms, keyframes, extracted fonts) | `%LOCALAPPDATA%\SubForge\cache` | `$XDG_CACHE_HOME/subforge` (usually `~/.cache/subforge`) | `~/Library/Caches/SubForge` |
| Downloaded whisper models | `models/` folder next to the cache folder | same | same |
| Autosave backups | `%TEMP%\subforge-autosave` | `$TMPDIR/subforge-autosave` (usually `/tmp`) | same |

Cache entries are keyed by path + size + modification time, so re-opening a video is instant and a replaced file is recomputed automatically. Entries unused for 45 days are deleted, and you can clear the cache from *Settings*. The autosave interval (2 minutes by default, 0 = off) is also set there.

---

## Known limitations

- **Exact preview rendering needs ffmpeg with libass.** Without it, SubForge draws a faster fallback overlay that handles `\pos`, `\move`, `\an`, `\fs` and `\1c`, but not `\t`, karaoke, drawings or rotation.
- **Frame-by-frame animations** use the frame rate of the currently open video (23.976 if no video is open). If you switch to a video with a different frame rate, apply the animation again.
- **Thai text on screen:** egui does not do full complex text shaping, so a few stacked tone marks may be drawn slightly out of place in the editor. The saved file contains exactly what you typed.
- **Motion tracking** uses template matching. It struggles with objects that have no texture, objects that are hidden behind something, heavy motion blur, or big changes in perspective. Unreliable frames are marked in red so you can fix them. Perspective (`\frx`/`\fry`) is not tracked yet.
- **OCR repair** can't fix a misread Thai word if the mistake happens to spell another real word. Check those in the review window.
- **Thai spell check** flags names and transliterated words that are not in the dictionary. Use *Add to dictionary* once and they won't be flagged again.
- **Overlap checks and auto-fix** only compare lines with the same style and layer, so signs that overlap dialogue on purpose are not reported.
- Without an audio output device, video plays without sound, timed by the system clock.

---

## License

MIT (as declared in `Cargo.toml`). Add a `LICENSE` file with the MIT text before publishing.

The bundled Hunspell dictionaries in `crates/subforge-core/dicts/` are third-party works under their own licenses: the English dictionary is from [SCOWL/wordlist.sourceforge.net](http://wordlist.sourceforge.net) (see `README_en_US.txt`), and the Thai dictionary is from LibreOffice/LEXiTRON.
