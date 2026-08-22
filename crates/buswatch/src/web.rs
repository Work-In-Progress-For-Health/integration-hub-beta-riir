//! The pages of `spec/14-buswatch.md` §3.
//!
//! # Why the routes are generic over the reader
//!
//! §8.4 asks for every route to render "with a fake reader, including the
//! empty-queue and unreachable-queue cases". An unreachable queue is not
//! something a real emulator can be asked for on demand, so the reader is a
//! parameter rather than a concrete type — see [`QueueReader`].
//!
//! # Nothing here returns an error page for one queue's failure
//!
//! §6.2: "A failure to reach one queue MUST NOT fail the page." Snapshots
//! carry their own errors, so the index renders whatever it has.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::error::quota_hint;
use crate::reader::{MessageDetail, QueueReader};
use crate::render::{Body, render};
use crate::snapshot::{Cache, Snapshot};

/// The bundled stylesheet (§7.1).
const STYLESHEET: &str = include_str!("../static/buswatch.css");

/// The bundled logo (§7.1, copied across unchanged).
const LOGO: &[u8] = include_bytes!("../static/DHCW-logo-RGB-White.png");

/// Everything the routes share.
///
/// The cache is behind a mutex rather than a lock-free structure because
/// BusWatch serves one developer: contention is not a consideration, and a
/// mutex makes §3.2's "serve the cache, refresh what is missing" a single
/// obvious critical section.
pub struct AppState<R> {
    /// Where snapshots come from.
    pub reader: R,
    /// The settings of §2.
    pub config: Config,
    /// §3.2's per-queue cache.
    pub cache: Mutex<Cache>,
    /// §3.4's one-shot feedback message.
    pub flash: Mutex<Option<String>>,
}

impl<R: QueueReader> AppState<R> {
    /// Build state around a reader.
    pub fn new(reader: R, config: Config) -> Self {
        Self {
            reader,
            config,
            cache: Mutex::new(Cache::new()),
            flash: Mutex::new(None),
        }
    }
}

/// The routes of §3.
///
/// # Examples
///
/// ```
/// # use std::sync::Arc;
/// # use buswatch::config::Config;
/// # use buswatch::web::{router, AppState};
/// # use buswatch::fake::FakeReader;
/// let config = Config::from_pairs([("BUSWATCH_QUEUE_NAMES", "one,two")])
///     .expect("the defaults cover the rest");
/// let app = router(Arc::new(AppState::new(FakeReader::new(), config)));
/// # let _ = app;
/// ```
pub fn router<R: QueueReader>(state: Arc<AppState<R>>) -> Router {
    Router::new()
        .route("/", get(index::<R>))
        .route("/messages/{queue}/{sequence}", get(detail::<R>))
        .route("/queues/{queue}/clear", post(clear::<R>))
        // Two files, so they are served by name rather than by walking a
        // directory: the binary carries them, and a path parameter under
        // `/static` would be a traversal question nobody needs to answer.
        .route("/static/buswatch.css", get(stylesheet))
        .route("/static/DHCW-logo-RGB-White.png", get(logo))
        .with_state(state)
}

/// `/`'s query string (§3.1).
#[derive(Debug, Default, Deserialize)]
pub struct Refresh {
    /// Refresh one queue.
    #[serde(default)]
    pub refresh_queue: Option<String>,
    /// Refresh every queue.
    #[serde(default)]
    pub refresh_all: Option<String>,
}

impl Refresh {
    /// Whether §3.1's `refresh_all=true` was asked for.
    ///
    /// Anything other than `true` is not a refresh: `refresh_all=false`
    /// appearing in a bookmarked URL should not refresh everything.
    fn all(&self) -> bool {
        self.refresh_all
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    }
}

/// The queue index (§3, §4).
#[derive(Template)]
#[template(path = "index.html")]
struct IndexPage {
    /// One snapshot per configured queue, in configuration order.
    queues: Vec<Snapshot>,
    /// §3.4's one-shot message, if there is one to show.
    flash: Option<String>,
}

/// One message (§5).
#[derive(Template)]
#[template(path = "detail.html")]
struct DetailPage {
    /// The message.
    detail: MessageDetail,
    /// Its body, rendered per §5.4.
    body: Body,
}

/// A page that could not be produced (§5.2).
#[derive(Template)]
#[template(path = "problem.html")]
struct ProblemPage {
    /// What went wrong, in a few words.
    heading: String,
    /// What to do about it.
    detail: String,
    /// §6.1's hint, when the problem is one it covers.
    hint: Option<&'static str>,
}

/// `GET /` — the queue snapshots of §4.
async fn index<R: QueueReader>(
    State(state): State<Arc<AppState<R>>>,
    Query(refresh): Query<Refresh>,
) -> Response {
    let names = &state.config.queue_names;

    // §3.2 — decide what to refresh while holding the cache, so two
    // concurrent renders cannot both decide to refresh the same queue.
    let to_refresh = {
        let cache = state.cache.lock().await;
        cache.needing_refresh(names, refresh.refresh_queue.as_deref(), refresh.all())
    };

    if !to_refresh.is_empty() {
        let fresh = refresh_all(state.as_ref(), to_refresh).await;
        let mut cache = state.cache.lock().await;
        for snapshot in fresh {
            cache.insert(snapshot);
        }
    }

    // §3.4 — consumed by this render, then discarded.
    let flash = state.flash.lock().await.take();

    let queues = {
        let cache = state.cache.lock().await;
        names
            .iter()
            .filter_map(|name| cache.get(name).cloned())
            .collect()
    };

    page(&IndexPage { queues, flash })
}

/// Peek at several queues at once (§3.3).
///
/// Bounded by `BUSWATCH_MAX_PARALLEL_QUEUE_FETCHES`: the emulator's connection quota (§6.1)
/// is exactly what an unbounded fan-out would exhaust, turning a slow page
/// into a page of quota errors.
async fn refresh_all<R: QueueReader>(state: &AppState<R>, queues: Vec<String>) -> Vec<Snapshot> {
    use futures_util::StreamExt;

    // The queue names arrive owned, and each peek is an `async move` block
    // that keeps its own. Mapping over `&String` instead would need a
    // closure whose return type varies with the reference's lifetime --
    // `snapshot` is an `async fn` in a trait, so its future's type does --
    // and an iterator adaptor needs one fixed item type.
    let count = state.config.peek_count;
    let peeks = queues
        .into_iter()
        .map(|queue| async move { state.reader.snapshot(&queue, count).await });

    futures_util::stream::iter(peeks)
        .buffer_unordered(state.config.parallelism)
        .collect()
        .await
}

/// `GET /messages/{queue}/{sequence}` — one message, per §5.
async fn detail<R: QueueReader>(
    State(state): State<Arc<AppState<R>>>,
    Path((queue, sequence)): Path<(String, i64)>,
) -> Response {
    let limit = state.config.detail_search_limit;

    match state.reader.detail(&queue, sequence, limit).await {
        Some(found) => {
            let body = render(&found.body);
            page(&DetailPage {
                detail: found,
                body,
            })
        }
        // §5.2 — naming the limit, so the reader knows to raise it rather
        // than concluding the message is gone.
        None => problem(
            StatusCode::NOT_FOUND,
            "Message not found",
            &format!(
                "Sequence {sequence} was not among the first {limit} messages on {queue}. \
                 It may be further down the queue: raise BUSWATCH_DETAIL_SEARCH_LIMIT \
                 (currently {limit}) and try again."
            ),
            None,
        ),
    }
}

/// `POST /queues/{queue}/clear` — clear, then redirect to `/`.
async fn clear<R: QueueReader>(
    State(state): State<Arc<AppState<R>>>,
    Path(queue): Path<String>,
) -> Response {
    let message = match state.reader.clear(&queue).await {
        Ok(count) => format!("Cleared {count} message{} from {queue}.", plural(count)),
        // §6.1 — an actionable hint rather than a stack trace.
        Err(reason) => quota_hint(&reason).map_or_else(
            || format!("Could not clear {queue}: {reason}"),
            |hint| format!("Could not clear {queue}: {reason} — {hint}"),
        ),
    };

    // §3.4 — a flash, consumed by the next render.
    *state.flash.lock().await = Some(message);

    // The snapshot is now wrong, so drop it and let the next render take a
    // fresh one rather than showing messages that are gone.
    state.cache.lock().await.forget(&queue);

    // §3 — redirect to `/`, so a reload does not clear the queue again.
    Redirect::to("/").into_response()
}

/// `GET /static/buswatch.css`.
async fn stylesheet() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        STYLESHEET,
    )
        .into_response()
}

/// `GET /static/DHCW-logo-RGB-White.png`.
async fn logo() -> Response {
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static("image/png"))],
        LOGO,
    )
        .into_response()
}

/// Render a template, or say plainly that it could not be rendered.
///
/// §8.5 makes a template a build-time guarantee, so the only way this fails
/// is a formatting error from a value — and even then the answer is a page
/// rather than an empty response, because BusWatch is what a developer
/// reaches for when something else is already broken.
fn page<T: Template>(template: &T) -> Response {
    template.render().map_or_else(
        |error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("BusWatch could not render this page: {error}"),
            )
                .into_response()
        },
        |html| Html(html).into_response(),
    )
}

/// A problem page with a status (§5.2).
fn problem(
    status: StatusCode,
    heading: &str,
    detail: &str,
    hint: Option<&'static str>,
) -> Response {
    let rendered = ProblemPage {
        heading: heading.to_string(),
        detail: detail.to_string(),
        hint,
    };

    rendered.render().map_or_else(
        |_| (status, detail.to_string()).into_response(),
        |html| (status, Html(html)).into_response(),
    )
}

/// `""` or `"s"`.
const fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
