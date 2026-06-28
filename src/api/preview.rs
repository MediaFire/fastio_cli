#![allow(clippy::missing_errors_doc)]

/// Preview API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for preview URL generation and transform requests.
use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::client::ApiClient;
use crate::error::CliError;

/// The only valid transform name (the server validates against
/// `SyncRenderer::names()`, whose sole value is `image`).
pub const IMAGE_TRANSFORM_NAME: &str = "image";

/// Output formats accepted by the image transform (`output-format`, hyphenated).
/// `webp` is NOT supported.
const VALID_OUTPUT_FORMATS: &[&str] = &["png", "jpg", "jpeg"];

/// Named size presets accepted by the image transform (`size`, case-insensitive).
const VALID_SIZES: &[&str] = &["icontiny", "iconsmall", "iconmedium", "preview"];

/// Rotations accepted by the image transform (`rotate`).
const VALID_ROTATIONS: &[u32] = &[0, 90, 180, 270];

/// Get a preauthorized preview URL for a file.
///
/// `GET /{context_type}/{context_id}/storage/{node_id}/preview/{preview_type}/preauthorize/`
pub async fn get_preview_url(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    preview_type: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/storage/{}/preview/{}/preauthorize/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
        urlencoding::encode(preview_type),
    );
    client.get(&path).await
}

/// Parameters for requesting an image transformation.
///
/// The transform name is fixed to `image` (the only valid value); the struct
/// carries the node coordinates plus the image parameters that are applied when
/// the resulting `/read/` URL is fetched.
pub struct TransformUrlParams<'a> {
    /// Context type: workspace or share.
    pub context_type: &'a str,
    /// Context ID (workspace or share ID).
    pub context_id: &'a str,
    /// Storage node ID.
    pub node_id: &'a str,
    /// Transform name — must be `image`.
    pub transform_name: &'a str,
    /// Target width in pixels (positive int).
    pub width: Option<u32>,
    /// Target height in pixels (positive int).
    pub height: Option<u32>,
    /// Output format: `png`, `jpg`, or `jpeg` (the wire key is `output-format`).
    pub output_format: Option<&'a str>,
    /// Size preset: `IconTiny`, `IconSmall`, `IconMedium`, or `Preview`
    /// (case-insensitive).
    pub size: Option<&'a str>,
    /// Crop rectangle width (all four crop fields required together).
    pub crop_width: Option<u32>,
    /// Crop rectangle height.
    pub crop_height: Option<u32>,
    /// Crop rectangle x offset.
    pub crop_x: Option<u32>,
    /// Crop rectangle y offset.
    pub crop_y: Option<u32>,
    /// Rotation in degrees: one of 0, 90, 180, 270.
    pub rotate: Option<u32>,
}

/// Validate the image-transform parameters against the server contract,
/// returning a `CliError::Parse` describing the first violation.
fn validate_transform_params(params: &TransformUrlParams<'_>) -> Result<(), CliError> {
    if params.transform_name != IMAGE_TRANSFORM_NAME {
        return Err(CliError::Parse(format!(
            "the only valid transform name is '{IMAGE_TRANSFORM_NAME}' (got '{}')",
            params.transform_name
        )));
    }
    if let Some(fmt) = params.output_format {
        let lower = fmt.to_ascii_lowercase();
        if !VALID_OUTPUT_FORMATS.contains(&lower.as_str()) {
            return Err(CliError::Parse(format!(
                "invalid output-format '{fmt}' (valid: png, jpg, jpeg)"
            )));
        }
    }
    if let Some(size) = params.size
        && !VALID_SIZES.contains(&size.to_ascii_lowercase().as_str())
    {
        return Err(CliError::Parse(format!(
            "invalid size '{size}' (valid: IconTiny, IconSmall, IconMedium, Preview)"
        )));
    }
    if let Some(rotate) = params.rotate
        && !VALID_ROTATIONS.contains(&rotate)
    {
        return Err(CliError::Parse(format!(
            "invalid rotate '{rotate}' (valid: 0, 90, 180, 270)"
        )));
    }
    // The backend marks every integer dimension `Assert\Positive` (strictly
    // > 0), so a supplied 0 is rejected server-side — reject it client-side with
    // a clear message instead of a 400. Rotate is excluded: it is an
    // `Assert\Choice` where 0 is a valid value. (See
    // `~/vividengine/php/lib/sync_rendering/enum/ImageRenderKeys.php`.)
    for (label, value) in [
        ("--width", params.width),
        ("--height", params.height),
        ("--crop-width", params.crop_width),
        ("--crop-height", params.crop_height),
        ("--crop-x", params.crop_x),
        ("--crop-y", params.crop_y),
    ] {
        if value == Some(0) {
            return Err(CliError::Parse(format!(
                "{label} must be a positive integer (got 0)"
            )));
        }
    }
    // A crop requires all four of cropwidth/cropheight/cropx/cropy together.
    let crop_set = [
        params.crop_width,
        params.crop_height,
        params.crop_x,
        params.crop_y,
    ];
    let crop_count = crop_set.iter().filter(|v| v.is_some()).count();
    if crop_count != 0 && crop_count != 4 {
        return Err(CliError::Parse(
            "a crop requires all four of --crop-width/--crop-height/--crop-x/--crop-y together"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Build the absolute `/read/` URL that applies the image params, given the
/// download `token` minted by `requestread`.
///
/// Extracted as a pure free function so the URL assembly (param keys, the
/// hyphenated lowercased `output-format`, the all-or-nothing crop block) can be
/// unit-tested without an HTTP client. `base_url` is the API base (no trailing
/// slash expected, but tolerated). Only params the caller supplied are appended;
/// the four `crop*` keys are emitted ONLY when all four are present (a
/// present-but-empty key 400s the backend's `ExpectsInteger`).
fn build_transform_read_url(
    base_url: &str,
    params: &TransformUrlParams<'_>,
    token: &str,
) -> String {
    let mut url = format!(
        "{}/{}/{}/storage/{}/transform/{}/read/?token={}",
        base_url.trim_end_matches('/'),
        urlencoding::encode(params.context_type),
        urlencoding::encode(params.context_id),
        urlencoding::encode(params.node_id),
        urlencoding::encode(IMAGE_TRANSFORM_NAME),
        urlencoding::encode(token),
    );
    if let Some(fmt) = params.output_format {
        // The server's `output-format` Choice constraint is case-SENSITIVE
        // (png|jpg|jpeg lowercase); validation accepts mixed case, so normalize
        // here or `JPG` would reach the server as `output-format=JPG` and 400.
        let _ = write!(
            url,
            "&output-format={}",
            urlencoding::encode(&fmt.to_ascii_lowercase())
        );
    }
    if let Some(w) = params.width {
        let _ = write!(url, "&width={w}");
    }
    if let Some(h) = params.height {
        let _ = write!(url, "&height={h}");
    }
    if let Some(s) = params.size {
        let _ = write!(url, "&size={}", urlencoding::encode(s));
    }
    // Crop is all-or-nothing (validated above). Only emit the four `crop*` keys
    // when ALL four are present — a present-but-EMPTY key is read by the backend
    // as input and run through `ExpectsInteger` on `""`, which throws and 400s
    // every no-crop transform (the common path). So omit them entirely when unset.
    if let (Some(cw), Some(ch), Some(cx), Some(cy)) = (
        params.crop_width,
        params.crop_height,
        params.crop_x,
        params.crop_y,
    ) {
        let _ = write!(url, "&cropwidth={cw}&cropheight={ch}&cropx={cx}&cropy={cy}");
    }
    if let Some(r) = params.rotate {
        let _ = write!(url, "&rotate={r}");
    }
    url
}

/// Request an image transformation (resize, crop, format conversion).
///
/// Two-step model: a download token is minted via `requestread` (which ignores
/// the image params), then the returned URL is the **`/read/`** URL carrying the
/// token AND the image params, so they actually apply when the URL is fetched.
///
/// - `GET /{ctx}/{ctx_id}/storage/{node}/transform/image/requestread/` → `{token}`
/// - returned URL: `…/transform/image/read/?token=<t>&output-format=…&width=…&…`
///
/// The transform name is fixed to `image` (validated); `output-format` is
/// `png`/`jpg`/`jpeg` (no `webp`); `size` is one of the named presets; `rotate`
/// is one of 0/90/180/270; a crop needs all four crop fields. Returns a JSON
/// object `{ "transform_name", "token", "read_url" }`.
pub async fn get_transform_url(
    client: &ApiClient,
    params: &TransformUrlParams<'_>,
) -> Result<Value, CliError> {
    validate_transform_params(params)?;

    // Step 1: mint the download token. `requestread` reads ONLY the node +
    // transform name (it ignores width/height/output-format/etc.).
    let request_path = format!(
        "/{}/{}/storage/{}/transform/{}/requestread/",
        urlencoding::encode(params.context_type),
        urlencoding::encode(params.context_id),
        urlencoding::encode(params.node_id),
        urlencoding::encode(IMAGE_TRANSFORM_NAME),
    );
    let response: Value = client.get(&request_path).await?;
    let token = response
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::Parse("transform requestread response is missing a `token`".to_owned())
        })?;

    // Step 2: build the `/read/` URL that actually applies the image params.
    let read_url = build_transform_read_url(client.base_url(), params, token);
    Ok(json!({
        "transform_name": IMAGE_TRANSFORM_NAME,
        "token": token,
        "read_url": read_url,
    }))
}

#[cfg(test)]
mod tests {
    use super::{TransformUrlParams, build_transform_read_url, validate_transform_params};
    use crate::error::CliError;

    fn base_params() -> TransformUrlParams<'static> {
        TransformUrlParams {
            context_type: "workspace",
            context_id: "ws1",
            node_id: "node1",
            transform_name: "image",
            width: None,
            height: None,
            output_format: None,
            size: None,
            crop_width: None,
            crop_height: None,
            crop_x: None,
            crop_y: None,
            rotate: None,
        }
    }

    #[test]
    fn rejects_non_image_transform_name() {
        let mut p = base_params();
        p.transform_name = "video";
        let err = validate_transform_params(&p).expect_err("non-image must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn rejects_webp_output_format() {
        let mut p = base_params();
        p.output_format = Some("webp");
        assert!(validate_transform_params(&p).is_err());
    }

    #[test]
    fn accepts_valid_output_formats_case_insensitive() {
        for fmt in ["png", "jpg", "jpeg", "JPG", "PNG"] {
            let mut p = base_params();
            p.output_format = Some(fmt);
            assert!(validate_transform_params(&p).is_ok(), "{fmt}");
        }
    }

    #[test]
    fn rejects_invalid_size_and_rotate() {
        let mut p = base_params();
        p.size = Some("Huge");
        assert!(validate_transform_params(&p).is_err());
        let mut p = base_params();
        p.rotate = Some(45);
        assert!(validate_transform_params(&p).is_err());
    }

    #[test]
    fn accepts_valid_size_case_insensitive_and_rotations() {
        for s in ["IconTiny", "iconsmall", "ICONMEDIUM", "Preview"] {
            let mut p = base_params();
            p.size = Some(s);
            assert!(validate_transform_params(&p).is_ok(), "{s}");
        }
        for r in [0, 90, 180, 270] {
            let mut p = base_params();
            p.rotate = Some(r);
            assert!(validate_transform_params(&p).is_ok(), "{r}");
        }
    }

    #[test]
    fn partial_crop_is_rejected_full_crop_accepted() {
        let mut p = base_params();
        p.crop_width = Some(10);
        assert!(validate_transform_params(&p).is_err(), "partial crop");
        // All four set with positive offsets is accepted. (crop_x/crop_y of 0 is
        // NOT valid — the backend marks every crop field `Assert\Positive`.)
        let mut p = base_params();
        p.crop_width = Some(10);
        p.crop_height = Some(20);
        p.crop_x = Some(1);
        p.crop_y = Some(2);
        assert!(validate_transform_params(&p).is_ok(), "full crop");
    }

    #[test]
    fn zero_dimensions_are_rejected() {
        // The backend marks width/height/crop_* `Assert\Positive` (strictly > 0),
        // so a supplied 0 must be rejected client-side. Rotate is excluded (0 is
        // a valid Choice).
        for set in [
            |p: &mut TransformUrlParams<'_>| p.width = Some(0),
            |p: &mut TransformUrlParams<'_>| p.height = Some(0),
            |p: &mut TransformUrlParams<'_>| {
                p.crop_width = Some(0);
                p.crop_height = Some(10);
                p.crop_x = Some(1);
                p.crop_y = Some(1);
            },
            |p: &mut TransformUrlParams<'_>| {
                p.crop_width = Some(10);
                p.crop_height = Some(10);
                p.crop_x = Some(0);
                p.crop_y = Some(1);
            },
        ] {
            let mut p = base_params();
            set(&mut p);
            assert!(validate_transform_params(&p).is_err(), "zero must reject");
        }
        // rotate=0 stays valid.
        let mut p = base_params();
        p.rotate = Some(0);
        assert!(validate_transform_params(&p).is_ok(), "rotate 0 valid");
    }

    #[test]
    fn read_url_uses_hyphenated_output_format_and_only_supplied_params() {
        let mut p = base_params();
        p.output_format = Some("png");
        p.width = Some(100);
        let url = build_transform_read_url("https://api.fast.io/current", &p, "TOK");
        assert!(url.starts_with(
            "https://api.fast.io/current/workspace/ws1/storage/node1/transform/image/read/?token=TOK"
        ));
        assert!(url.contains("&output-format=png"), "hyphenated key: {url}");
        assert!(!url.contains("output_format"), "no underscore key: {url}");
        assert!(url.contains("&width=100"), "{url}");
        // Height/size/rotate were unset → absent.
        assert!(!url.contains("&height="), "{url}");
        assert!(!url.contains("&size="), "{url}");
        assert!(!url.contains("&rotate="), "{url}");
        // With no crop set, NONE of the four crop keys may appear — a
        // present-but-empty key would 400 the backend's ExpectsInteger.
        assert!(!url.contains("cropwidth"), "no crop keys when unset: {url}");
        assert!(
            !url.contains("cropheight"),
            "no crop keys when unset: {url}"
        );
        assert!(!url.contains("cropx"), "no crop keys when unset: {url}");
        assert!(!url.contains("cropy"), "no crop keys when unset: {url}");
    }

    #[test]
    fn read_url_lowercases_output_format() {
        // Validation accepts mixed case, but the server's Choice constraint is
        // case-sensitive (lowercase), so the emitted key must be normalized.
        let mut p = base_params();
        p.output_format = Some("JPG");
        let url = build_transform_read_url("https://api.fast.io/current", &p, "T");
        assert!(url.contains("&output-format=jpg"), "lowercased: {url}");
        assert!(!url.contains("output-format=JPG"), "not raw case: {url}");
    }

    #[test]
    fn read_url_emits_full_crop_and_rotate_when_set() {
        let mut p = base_params();
        p.crop_width = Some(10);
        p.crop_height = Some(20);
        p.crop_x = Some(1);
        p.crop_y = Some(2);
        p.rotate = Some(90);
        let url = build_transform_read_url("https://api.fast.io/current/", &p, "T");
        assert!(
            url.contains("&cropwidth=10&cropheight=20&cropx=1&cropy=2"),
            "{url}"
        );
        assert!(url.contains("&rotate=90"), "{url}");
        // Trailing slash on the base is trimmed (no double slash before context).
        assert!(url.contains("/current/workspace/ws1/"), "{url}");
    }
}
