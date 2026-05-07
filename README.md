# epd-photoframe-server

A small HTTP server that turns a public Google Photos shared album into a
slow slideshow on a battery-powered e-ink photo frame — typically one
photo per day, or twice a day, so a single charge lasts months.

Plug an album link into the config, point your e-ink frame at this
server, and it serves a fresh, dithered, correctly-sized PNG every time
the frame wakes up.

## What the output looks like

These are unmodified server responses — the same PNGs the frames
display, dithered to the target panel and complete with info overlay
and battery indicator.

![Living room — 1200×1600 portrait, Spectra 6](examples/living-room.png)

*1200×1600 portrait, Spectra 6 (E1004) — `naive-dominant` decomposition,
Floyd–Steinberg diffusion, blue noise. Info overlay bottom-left,
battery indicator top-right.*

![E1002 landscape — 800×480, Spectra 6](examples/e1002-landscape.png)

*800×480 landscape, Spectra 6 (E1002) — `octahedron-furthest`
decomposition, Floyd–Steinberg diffusion, blue noise.*

![E1001 landscape — 800×480, grayscale](examples/e1001-landscape.png)

*800×480 landscape, 4-level grayscale (E1001) — `gray-offset-blend:0.33`
decomposition, Floyd–Steinberg diffusion, blue noise.*

## How it fits together

Three pieces, all on GitHub:

- **[epd-photoframe](https://github.com/Frans-Willem/epd-photoframe)** —
  the firmware running on the frame itself (a Seeed reTerminal E1001,
  E1002, or E1004). It wakes up on a timer, fetches one image, displays
  it, and goes back to sleep.
- **epd-photoframe-server** *(this repo)* — the server the frame talks
  to. It does all the work: download a photo, resize it, draw an
  optional info overlay, dither it down to the panel's palette
  (six-colour Spectra 6 on the E1002 / E1004, grayscale on the E1001),
  hand back a PNG.
- **[epd-dither](https://github.com/Frans-Willem/epd-dither)** — the
  library that does the dithering. The server pulls it in as a
  dependency; you don't need to install it separately.

The frame is intentionally dumb — it just displays whatever PNG the
server hands it. The main reason is memory: the E1004 (13.3″, 1200×1600)
simply can't hold a full-resolution source photo and dither it on-device
with the RAM it has. Doing the heavy lifting on the server side also
keeps the frame asleep for longer per refresh, which saves battery.

## What it does on each request

When the frame asks for `/screen/<name>`, the server:

1. Picks a photo from the configured Google Photos album.
2. Downloads it at roughly the right size and crops or pads it to fit
   the panel exactly.
3. Optionally draws an info overlay (date, time, weather) and a battery
   indicator.
4. Dithers the result to the panel's palette.
5. Returns it as a PNG, along with a `Refresh` header telling the frame
   when to wake up for the next photo.

Optionally, it can also forward sensor readings the frame includes in
its request (battery level, temperature, humidity) over MQTT — to Home
Assistant, for example, or any other consumer of plain MQTT.

## Setup

You need a public Google Photos shared album and somewhere to run the
server.

1. Create the album and grab a share link (see below).
2. Copy `config.minimal.toml` to `config.toml` and edit it — at minimum,
   set the screen's `name`, `width`, `height`, and `share_url`.
3. Run the server (see below).
4. Point your frame's firmware at `http://<server>:3000/screen/<name>`,
   or just open that URL in a browser to check it works.
5. Once you have something showing up, expand the config to taste —
   `config.example.toml` walks through every available option with
   inline comments (rotation schedule, info overlay, battery indicator,
   dithering, MQTT, …).

### Creating the Google Photos album

In the Google Photos web app or mobile app:

1. Create a new album and add the photos you want in rotation.
2. Open the album, hit **Share**, and turn on link sharing (**Create
   link** / **Share via link**). The album must be reachable without a
   Google sign-in — the server scrapes the public share page anonymously.
3. Copy the link. Both `https://photos.app.goo.gl/…` short links and
   `https://photos.google.com/share/…` long links work; paste whichever
   you got into `share_url`.

You can keep adding or removing photos later. The server scrapes the
share page lazily — on whichever request first finds the cached copy
older than an hour — so changes show up after the next frame fetch once
the cache has expired. Hitting the endpoint with `?action=refresh`,
`?action=next`, or `?action=previous` drops the cached copy and
re-scrapes immediately if you don't want to wait.

### Running with Docker (recommended for deployment)

The repo ships with a `Dockerfile`. The easy path is to check the repo
out next to a `docker-compose.yml` that builds it in place:

```yaml
services:
  epd-photoframe-server:
    build: ./epd-photoframe-server
    restart: unless-stopped
    ports:
      - "3000:3000"
    volumes:
      - ./config.toml:/config.toml:ro
```

Then `docker compose up -d --build`. To pick up server updates, `git
pull` inside the checkout and re-run the same command.

### Running with cargo (for development)

If you have a Rust toolchain handy and want to iterate on the code:

```bash
cargo run --release -- config.toml
```

## Trying it from a browser

Every response carries a standard HTTP `Refresh` header, so a browser
will happily auto-reload at the configured cadence — useful for
sanity-checking a config without involving any hardware. A few query
parameters are worth knowing about:

- `?action=next` / `?action=previous` — step through the album manually.
- `?action=refresh` — drop the cached album scrape and re-fetch (useful
  after adding or removing photos in the album).
- `?battery_pct=42&battery_mv=3700` — pretend the request came from a
  device reporting these values, so the battery indicator and MQTT
  publishing exercise their full code paths.

For example: <http://localhost:3000/screen/living-room?action=next&battery_pct=42>.

## Configuring screens

Each `[[screens]]` section in `config.toml` is one frame. The bare
minimum is name, dimensions, and album URL; everything else has sensible
defaults. The example config covers:

- **Fit and background** — how to handle photos whose aspect ratio
  doesn't match the panel.
- **Rotation schedule** — when to reshuffle to a new photo. Cron
  expressions or natural-language phrases like "at 6 AM and 6 PM".
- **Info overlay** — date, day of week, and current weather, drawn in a
  corner of the image. The header (day / date) and weather panel are
  configured independently — show one, both, or neither — and on a tall
  display (E1004) the weather can expand into a multi-day forecast row
  alongside or instead of today's reading.
- **Battery indicator** — a small Android-style battery icon showing
  the level the frame reported.
- **Dithering** — noise pattern, error-diffusion algorithm, and which
  palette to target (Spectra 6 variants for colour panels, grayscale
  for the E1001). Defaults are fine for most setups; the knobs are
  there if you want to tune for your panel and lighting.

`config.example.toml` documents every option in full — what it does,
what values it accepts, and what the default is. Treat it as the
reference; this section is just the highlights.

## Home Assistant / MQTT

If you add a top-level `[mqtt]` section, the server publishes whatever
sensor values the frame includes on its requests (battery voltage,
temperature, humidity, charging state) to your MQTT broker, plus a
"last seen" timestamp on every fetch.

On startup it also publishes a Home Assistant–style **discovery config**
for each frame on the standard `homeassistant/...` topic. Home Assistant
picks these up automatically, and so does anything else that follows the
same convention (openHAB, Domoticz, IoBroker, …) — each frame just
appears as a device with the right sensor entities, no manual YAML. The
state topics themselves are plain MQTT, so anything else on the bus can
read them too.

## Hardware

Tested with the **Seeed reTerminal E1001**, **E1002** (7.3″ Spectra 6),
and **E1004** (13.3″ Spectra 6) e-ink frames, paired with the
[epd-photoframe](https://github.com/Frans-Willem/epd-photoframe)
firmware. The server doesn't actually care what's on the other end —
anything that can fetch a paletted PNG over HTTP and respect a
`Refresh` header will work, including a plain web browser.

## Status

Hobby project, but I've taken some care to polish it: it should be in
reasonable shape for someone other than me to pick up and use. Feedback,
bug reports, and pull requests are welcome on the GitHub issue tracker.

The Google Photos share-page scraping in particular is at the mercy of
Google's HTML layout — if it breaks, please file an issue.

## License

Source code is licensed under version 3 of the GNU Affero General
Public License (`AGPL-3.0-only`). Full text in [LICENSE](./LICENSE).

Bundled third-party assets in `assets/` carry their own licences (SIL
OFL 1.1 for the fonts) — see [NOTICE](./NOTICE) and the per-asset
files in [LICENSES/](./LICENSES) for details.

## A note on LLM use

I made heavy use of Claude while building this. I was closely involved
throughout — every change Claude proposed was reviewed before it
landed, and I personally stand by the quality of the code in this repo.
What an LLM gave me was time: enough of it to take this project from
"works on my desk" to something polished enough for other people to
use, which I wouldn't otherwise have done as a side project.

That said, plenty of suggestions Claude made along the way were
nonsense, and I would not trust an LLM to write code unsupervised after
this experience. Use accordingly.
