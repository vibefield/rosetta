//! The library needs catalog access; captured content is scoped to its source
//! session. HTML is delivered as inert bytes, never active daemon-origin HTML.

use super::*;
use rosetta_protocol::{ArtifactListRequest, ArtifactPage};
use rosetta_store::html_artifacts::{self, ListError};

pub(super) async fn list(
    State(state): State<AppState>,
    Query(request): Query<ArtifactListRequest>,
) -> Result<Json<ArtifactPage>, Failure> {
    let hide_pending = !state.projection.is_published();
    let page = with_read(state.projection.clone(), move |conn| {
        html_artifacts::list(conn, &request, hide_pending).map_err(|error| match error {
            ListError::Invalid => Failure::bad_request("invalid artifact filter or cursor"),
            ListError::Expired => Failure::cursor_expired(),
            ListError::Store(error) => store_err(error),
        })
    })
    .await?;
    Ok(Json(page))
}

pub(super) async fn content(
    State(state): State<AppState>,
    UrlPath((session_id, publication_id)): UrlPath<(String, String)>,
) -> Result<Response, Failure> {
    let hide_pending = !state.projection.is_published();
    let asset = with_read(state.projection.clone(), move |conn| {
        if behind_admission(conn, &session_id, hide_pending)? {
            return Err(Failure::not_found("artifact is not available"));
        }
        html_artifacts::get(conn, &session_id, &publication_id)
            .map_err(store_err)?
            .and_then(|entry| entry.asset)
            .ok_or_else(|| Failure::not_found("this publication has no captured HTML"))
    })
    .await?;
    asset
        .validate()
        .map_err(|e| Failure::internal(format!("invalid artifact content: {e}")))?;
    if asset.media_type != rosetta_core::html_artifacts::HTML_MEDIA_TYPE
        || asset.byte_len > rosetta_core::html_artifacts::HTML_MAX_BYTES as u64
    {
        return Err(Failure::internal("invalid artifact media type or size"));
    }
    let root = assets::asset_root(&state.db);
    let bytes = tokio::task::spawn_blocking(move || {
        let path = root.join(format!("{}.html", asset.digest));
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| Failure::not_found("captured HTML is unavailable"))?;
        if !metadata.is_file() || metadata.len() != asset.byte_len {
            return Err(Failure::internal("captured HTML has the wrong size"));
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| Failure::not_found("captured HTML is unavailable"))?
            .take(rosetta_core::html_artifacts::HTML_MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Failure::internal(format!("reading captured HTML: {e}")))?;
        if bytes.len() as u64 != asset.byte_len
            || blake3::hash(&bytes).to_hex().as_str() != asset.digest
        {
            return Err(Failure::internal("captured HTML failed its content hash"));
        }
        Ok(bytes)
    })
    .await
    .map_err(|e| Failure::internal(format!("artifact read failed: {e}")))??;
    let mut response = Response::new(Body::from(bytes));
    for (name, value) in [
        (header::CONTENT_TYPE, "application/octet-stream"),
        (
            header::CONTENT_DISPOSITION,
            "attachment; filename=artifact.html",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (
            header::CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'",
        ),
        (header::CACHE_CONTROL, "private, no-store"),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    Ok(response)
}
