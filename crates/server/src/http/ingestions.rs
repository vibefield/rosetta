use axum::Json;
use axum::extract::{Path, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rosetta_protocol::{ErrorCode, Ingestion, IngestionAction, IngestionControlRequest};

use super::{AppState, Failure};

fn response(run: Ingestion, accepted: bool) -> Response {
    let location = format!("/api/ingestions/{}", run.id);
    (
        if accepted {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        [
            (header::CACHE_CONTROL, "no-store".to_owned()),
            (header::LOCATION, location),
            (header::RETRY_AFTER, "1".to_owned()),
        ],
        Json(run),
    )
        .into_response()
}

pub(super) async fn get(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let result = (|| {
        let control = state
            .health
            .as_ref()
            .and_then(|health| health.ingestion())
            .ok_or_else(|| Failure::not_found("this daemon has no cold run"))?;
        let run = control.snapshot();
        if run.id != id {
            return Err(Failure::not_found("unknown ingestion"));
        }
        Ok(response(run, false))
    })();
    no_store(result.unwrap_or_else(IntoResponse::into_response))
}

pub(super) async fn pause(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<IngestionControlRequest>, JsonRejection>,
) -> Response {
    command(state, id, headers, request, IngestionAction::Pause).await
}

pub(super) async fn resume(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<IngestionControlRequest>, JsonRejection>,
) -> Response {
    command(state, id, headers, request, IngestionAction::Resume).await
}

async fn command(
    state: AppState,
    id: String,
    headers: HeaderMap,
    request: Result<Json<IngestionControlRequest>, JsonRejection>,
    action: IngestionAction,
) -> Response {
    let result = async {
        let Json(request) =
            request.map_err(|_| Failure::bad_request("expected a JSON expected_revision"))?;
        let field = |name| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| Failure::bad_request(format!("missing {name}")))
        };
        let index = field("x-rosetta-index-id")?;
        let key = field("idempotency-key")?;
        if key.len() != 36
            || !key.bytes().enumerate().all(|(i, byte)| {
                if [8, 13, 18, 23].contains(&i) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
        {
            return Err(Failure::bad_request("Idempotency-Key must be a UUID"));
        }
        let control = state
            .health
            .as_ref()
            .and_then(|health| health.ingestion())
            .cloned()
            .ok_or_else(|| Failure::not_found("this daemon has no cold run"))?;
        let reply = tokio::task::spawn_blocking(move || {
            control.command(&id, &index, &key, action, request)
        })
        .await
        .map_err(|error| Failure::internal(error.to_string()))?
        .map_err(|error| {
            let status = match error.code {
                ErrorCode::NotFound => StatusCode::NOT_FOUND,
                ErrorCode::ControlCapacity => StatusCode::TOO_MANY_REQUESTS,
                ErrorCode::ShuttingDown => StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::CONFLICT,
            };
            Failure(status, error)
        })?;
        Ok(response(reply.run, reply.accepted))
    }
    .await;
    no_store(result.unwrap_or_else(IntoResponse::into_response))
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    if response.status() == StatusCode::TOO_MANY_REQUESTS {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            axum::http::HeaderValue::from_static("60"),
        );
    }
    response
}
