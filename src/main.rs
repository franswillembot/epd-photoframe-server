mod album;
mod background;
mod config;
mod degraded;
mod device;
mod dither;
mod draw;
mod mqtt;
mod overlays;
mod screen_state;
#[cfg(test)]
mod test_snapshot;
mod weather;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::Context;
use axum::{
    Router,
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use tiny_skia::Pixmap;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use album::AlbumClient;
use config::{Config, ScreenConfig};
use device::{PowerState, SensorState};
use dither::PreparedDitherMethod;
use mqtt::Publisher;
use overlays::{BatteryIndicator, Infobox, Overlay, OverlayContext};
use screen_state::{
    ScreenState, calculate_error_refresh_time, calculate_refresh_time, seconds_until,
};

struct Screen {
    config: ScreenConfig,
    album: AlbumClient,
    state: Mutex<ScreenState>,
    dither_method: PreparedDitherMethod,
    /// Per-screen overlay list, built once at startup. Render order =
    /// list order (later overlays draw on top of earlier ones).
    overlays: Vec<Box<dyn Overlay>>,
}

impl Screen {
    fn from_config(config: ScreenConfig) -> anyhow::Result<(String, Arc<Self>)> {
        let album = AlbumClient::new(config.share_url.clone())?;
        let state = Mutex::new(ScreenState::new(&config));
        let dither_method = PreparedDitherMethod::prepare(&config.dither)
            .with_context(|| format!("preparing dither for screen `{}`", config.name))?;
        let overlays = build_overlays(&config);
        let name = config.name.clone();
        Ok((
            name,
            Arc::new(Self {
                config,
                album,
                state,
                dither_method,
                overlays,
            }),
        ))
    }
}

fn build_overlays(config: &ScreenConfig) -> Vec<Box<dyn Overlay>> {
    // Order here = render order; later overlays draw on top.
    let mut overlays: Vec<Box<dyn Overlay>> = Vec::new();
    if let Some(cfg) = &config.infobox {
        overlays.push(Box::new(Infobox::new(cfg.clone(), config.locale.clone())));
    }
    if let Some(cfg) = &config.battery_indicator {
        overlays.push(Box::new(BatteryIndicator::new(cfg.clone())));
    }
    overlays
}

/// Shared application state. Wrapped in an `Arc` once at construction
/// (`Arc::new(AppState { … })`) and that `Arc<AppState>` is what's
/// passed to `Router::with_state` — Axum's per-request state extractor
/// then clones the outer `Arc` (one refcount bump) instead of every
/// inner field needing its own `Clone`. Each `Screen` is itself
/// `Arc<Screen>` so a request handler can hand a single owned
/// reference to a `spawn_blocking` task without cloning the screen's
/// fields.
struct AppState {
    screens: HashMap<String, Arc<Screen>>,
    http: Client,
    mqtt: Option<Publisher>,
}

impl AppState {
    fn from_config(config: Config) -> anyhow::Result<(Self, std::net::SocketAddr)> {
        let listen = config.listen;
        let mqtt = config.mqtt.as_ref().map(|m| {
            tracing::info!(broker = %m.broker, port = m.port, "connecting to mqtt broker");
            Publisher::connect(m, &config.screens)
        });
        let screens = build_screens(config.screens)?;
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok((
            Self {
                screens,
                http,
                mqtt,
            },
            listen,
        ))
    }
}

fn build_screens(screens: Vec<ScreenConfig>) -> anyhow::Result<HashMap<String, Arc<Screen>>> {
    screens
        .into_iter()
        .map(Screen::from_config)
        .collect::<anyhow::Result<_>>()
}

struct CanvasResult {
    image: Pixmap,
    degraded: bool,
}

#[derive(Debug, Deserialize)]
struct ScreenQuery {
    #[serde(default)]
    action: Option<Action>,
    #[serde(default)]
    battery_mv: Option<u32>,
    #[serde(default)]
    battery_pct: Option<u8>,
    #[serde(default, rename = "temp_c")]
    temperature_c: Option<f32>,
    #[serde(default)]
    humidity_pct: Option<f32>,
    #[serde(default)]
    power: Option<PowerState>,
}

impl ScreenQuery {
    fn into_request(self) -> ScreenRequest {
        let refresh_album = matches!(self.action, Some(Action::Refresh));
        let cursor_advance = match self.action {
            Some(Action::Next) => 1,
            Some(Action::Previous) => -1,
            _ => 0,
        };
        let sensors = SensorState {
            battery_mv: self.battery_mv,
            battery_pct: self.battery_pct,
            temperature_c: self.temperature_c,
            humidity_pct: self.humidity_pct,
            power: self.power,
        };
        ScreenRequest {
            action: self.action,
            refresh_album,
            cursor_advance,
            sensors,
        }
    }
}

struct ScreenRequest {
    action: Option<Action>,
    refresh_album: bool,
    cursor_advance: i64,
    sensors: SensorState,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Action {
    Next,
    Previous,
    Refresh,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "epd_photoframe_server=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_string());
    let config = Config::from_file(&config_path)?;
    tracing::info!(path = %config_path, screens = config.screens.len(), "loaded config");
    let (state, listen) = AppState::from_config(config)?;
    let state = Arc::new(state);

    let app = Router::new()
        .route("/screen/{name}", get(screen_handler))
        .route("/health", get(|| async { "ok" }))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn screen_handler(
    Path(name): Path<String>,
    Query(q): Query<ScreenQuery>,
    OriginalUri(uri): OriginalUri,
    State(state): State<Arc<AppState>>,
) -> Response {
    let Some(screen) = state.screens.get(&name) else {
        return (StatusCode::NOT_FOUND, format!("screen `{name}` not found")).into_response();
    };

    let request = q.into_request();
    let now = Utc::now();

    tracing::info!(screen = %name, ?request.action, "fetching image");
    let cfg = &screen.config;
    let mut next_rotation: Option<DateTime<Utc>> = None;

    let ctx = OverlayContext {
        now: now.with_timezone(&screen.config.timezone),
        sensors: &request.sensors,
        http: &state.http,
        canvas_size: (cfg.width, cfg.height),
    };

    // Photo retrieval and overlay preprocesses are independent — fire all
    // concurrently. Each overlay's preprocess does its own external work
    // (e.g. weather fetch); soft failures surface via `ReadyOverlay::degraded`
    // rather than aborting the request.
    let canvas_future = load_photo_or_placeholder(
        screen,
        &name,
        cfg,
        now,
        request.refresh_album,
        request.cursor_advance,
        &mut next_rotation,
    );
    let overlays_future =
        futures::future::join_all(screen.overlays.iter().map(|o| o.preprocess(&ctx)));
    let (canvas_result, ready_overlays) = tokio::join!(canvas_future, overlays_future,);

    let CanvasResult {
        image: mut img,
        mut degraded,
    } = match canvas_result {
        Ok(result) => result,
        Err(e) => {
            tracing::error!(screen = %name, error = %format!("{e:#}"), "placeholder allocation failed");
            return error_response_with_refresh(
                format!("placeholder failed: {e:#}"),
                cfg,
                next_rotation,
                now,
                uri.path(),
            );
        }
    };

    for overlay in &ready_overlays {
        if overlay.degraded() {
            degraded = true;
        }
        overlay.render(&mut img);
    }

    if let Some(publisher) = &state.mqtt {
        publisher.publish_screen_state(&name, &cfg.publish, &request.sensors, now);
    }

    let png = match encode_png_blocking(Arc::clone(screen), name.clone(), img).await {
        Ok(png) => png,
        Err(e) => {
            tracing::error!(screen = %name, error = %format!("{e:#}"), "dither failed");
            return error_response_with_refresh(
                format!("dither failed: {e:#}"),
                cfg,
                next_rotation,
                now,
                uri.path(),
            );
        }
    };

    let mut response = png.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));

    let refresh_at = calculate_refresh_time(
        degraded,
        cfg.error_refresh,
        cfg.wake_delay,
        next_rotation,
        now,
    );
    if let Some(refresh_at) = refresh_at {
        set_refresh_header(&mut response, refresh_at, now, uri.path());
    }

    response
}

async fn load_photo_or_placeholder(
    screen: &Screen,
    name: &str,
    cfg: &ScreenConfig,
    now: DateTime<Utc>,
    fresh: bool,
    advance: i64,
    next_rotation: &mut Option<DateTime<Utc>>,
) -> anyhow::Result<CanvasResult> {
    match load_photo(screen, cfg, now, fresh, advance, next_rotation).await {
        Ok(image) => Ok(CanvasResult {
            image,
            degraded: false,
        }),
        Err(e) => {
            tracing::warn!(screen = %name, error = %format!("{e:#}"), "image fetch failed; rendering placeholder");
            let image =
                degraded::placeholder(cfg.width, cfg.height, &cfg.background, &format!("{e:#}"))?;
            Ok(CanvasResult {
                image,
                degraded: true,
            })
        }
    }
}

async fn load_photo(
    screen: &Screen,
    cfg: &ScreenConfig,
    now: DateTime<Utc>,
    fresh: bool,
    advance: i64,
    next_rotation: &mut Option<DateTime<Utc>>,
) -> anyhow::Result<Pixmap> {
    let img = screen
        .album
        .pick(cfg.width, cfg.height, &cfg.fit, fresh, |n, new| {
            let mut st = screen.state.lock().expect("screen state poisoned");
            let idx = st.pick_index(now, advance, fresh, new, n);
            *next_rotation = st.next_scheduled_rotation(now);
            tracing::info!(
                seed = st.seed(),
                cursor = st.cursor(),
                idx,
                "selected photo"
            );
            idx
        })
        .await?;
    background::apply(img, cfg.width, cfg.height, &cfg.background)
}

async fn encode_png_blocking(
    screen: Arc<Screen>,
    name: String,
    img: Pixmap,
) -> anyhow::Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || {
        let dither_start = Instant::now();
        let palette_image = screen.dither_method.run(img)?;
        let dither_ms = dither_start.elapsed().as_secs_f64() * 1000.0;
        let encode_start = Instant::now();
        let png = palette_image.to_png().map_err(anyhow::Error::from)?;
        let encode_ms = encode_start.elapsed().as_secs_f64() * 1000.0;
        tracing::debug!(
            screen = %name,
            bytes = png.len(),
            dither_ms = format_args!("{dither_ms:.1}"),
            encode_ms = format_args!("{encode_ms:.1}"),
            "png ready"
        );
        Ok::<_, anyhow::Error>(png)
    })
    .await
    .context("dither task panicked")?
}

fn set_refresh_header(
    response: &mut Response,
    target: DateTime<Utc>,
    now: DateTime<Utc>,
    path: &str,
) {
    if let Ok(hv) = HeaderValue::from_str(&format!("{}; url={}", seconds_until(target, now), path))
    {
        response
            .headers_mut()
            .insert(HeaderName::from_static("refresh"), hv);
    }
}

fn error_response_with_refresh(
    body: String,
    cfg: &ScreenConfig,
    next_rotation: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    path: &str,
) -> Response {
    let mut response = (StatusCode::INTERNAL_SERVER_ERROR, body).into_response();
    let refresh_at =
        calculate_error_refresh_time(cfg.error_refresh, cfg.wake_delay, next_rotation, now);
    set_refresh_header(&mut response, refresh_at, now, path);
    response
}
