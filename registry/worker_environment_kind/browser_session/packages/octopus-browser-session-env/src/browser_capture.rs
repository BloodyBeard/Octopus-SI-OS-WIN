use std::path::Path;

use base64::Engine;
use octopus_util_image::{load_for_prompt_bytes, EncodedImage, PromptImageMode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{current_document, page_identity, CdpSession};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct LayoutViewport {
    page_x: f64,
    page_y: f64,
    client_width: f64,
    client_height: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct VisualViewport {
    offset_x: f64,
    offset_y: f64,
    page_x: f64,
    page_y: f64,
    client_width: f64,
    client_height: f64,
    scale: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    zoom: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct CssRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl CssRect {
    fn validate(&self) -> Result<(), String> {
        if ![self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f64::is_finite)
            || self.width <= 0.0
            || self.height <= 0.0
            || !(self.x + self.width).is_finite()
            || !(self.y + self.height).is_finite()
        {
            return Err("browser.screenshot received invalid CSS capture bounds".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct LayoutMetrics {
    css_layout_viewport: LayoutViewport,
    css_visual_viewport: VisualViewport,
    css_content_size: CssRect,
}

#[derive(PartialEq)]
struct CaptureObservation {
    metrics: LayoutMetrics,
    device_pixel_ratio: f64,
    url: Option<String>,
    title: Option<String>,
}

impl CaptureObservation {
    async fn read(session: &mut CdpSession) -> Result<Self, String> {
        let metrics: LayoutMetrics = serde_json::from_value(
            session
                .command("browser.screenshot", "Page.getLayoutMetrics", json!({}))
                .await?,
        )
        .map_err(|error| format!("browser.screenshot received malformed CSS metrics: {error}"))?;
        metrics.css_content_size.validate()?;
        let layout = &metrics.css_layout_viewport;
        CssRect {
            x: layout.page_x,
            y: layout.page_y,
            width: layout.client_width,
            height: layout.client_height,
        }
        .validate()?;
        let visual = &metrics.css_visual_viewport;
        CssRect {
            x: visual.page_x,
            y: visual.page_y,
            width: visual.client_width,
            height: visual.client_height,
        }
        .validate()?;
        if ![visual.offset_x, visual.offset_y, visual.scale]
            .into_iter()
            .all(f64::is_finite)
            || visual.scale <= 0.0
            || visual
                .zoom
                .is_some_and(|zoom| !zoom.is_finite() || zoom <= 0.0)
        {
            return Err("browser.screenshot received invalid CSS visual viewport".into());
        }
        let ratio = session
            .command(
                "browser.screenshot",
                "Runtime.evaluate",
                json!({"expression":"window.devicePixelRatio","returnByValue":true}),
            )
            .await?;
        if ratio.get("exceptionDetails").is_some() {
            return Err("browser.screenshot device pixel ratio evaluation failed".into());
        }
        let device_pixel_ratio = ratio["result"]["value"]
            .as_f64()
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or("browser.screenshot received no valid device pixel ratio")?;
        let (url, title) = page_identity(session).await?;
        Ok(Self {
            metrics,
            device_pixel_ratio,
            url,
            title,
        })
    }
}

pub(super) struct CapturedScreenshot {
    pub image: EncodedImage,
    pub payload_base64: String,
    pub content_hash: String,
    pub geometry: Value,
    pub url: Option<String>,
    pub title: Option<String>,
}

pub(super) async fn screenshot(
    session: &mut CdpSession,
    format: &str,
    quality: Option<u64>,
    full_page: bool,
    requested_clip: Option<Value>,
    css_pixels: bool,
    max_bytes: usize,
) -> Result<CapturedScreenshot, String> {
    let requested_clip: Option<CssRect> = requested_clip.map(serde_json::from_value).transpose()
        .map_err(|error| format!("Invalid screenshot clip: {error}"))?;
    if let Some(clip) = &requested_clip { clip.validate()?; }
    for _ in 0..3 {
        let document = current_document(session).await?;
        let revision = session.observation.document_revision;
        let before = CaptureObservation::read(session).await?;
        let rect = requested_clip.clone().or_else(|| full_page.then(|| before.metrics.css_content_size.clone())).or_else(|| css_pixels.then(|| {
            let viewport = &before.metrics.css_visual_viewport;
            CssRect { x: viewport.page_x, y: viewport.page_y, width: viewport.client_width, height: viewport.client_height }
        }));
        let scale = if css_pixels { 1.0 / before.device_pixel_ratio } else { 1.0 };
        let beyond_viewport = full_page || requested_clip.is_some();
        let clip = rect.as_ref().map(
            |rect| json!({"x":rect.x,"y":rect.y,"width":rect.width,"height":rect.height,"scale":scale}),
        );
        let mut params = json!({
            "format":format,"fromSurface":true,"captureBeyondViewport":beyond_viewport
        });
        if let Some(clip) = &clip {
            params["clip"] = clip.clone();
        }
        if let Some(quality) = quality {
            params["quality"] = json!(quality);
        }
        let result = session
            .command("browser.screenshot", "Page.captureScreenshot", params)
            .await?;
        let encoded = result["data"]
            .as_str()
            .ok_or("browser.screenshot CDP reply has no data")?;
        if encoded.len() > max_bytes.div_ceil(3).saturating_mul(4) {
            return Err(format!(
                "browser.screenshot result exceeds {max_bytes} bytes"
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| format!("browser.screenshot data is not base64: {error}"))?;
        if bytes.is_empty() || bytes.len() > max_bytes {
            return Err(format!(
                "browser.screenshot result exceeds {max_bytes} bytes"
            ));
        }
        let content_hash = blake3::hash(&bytes).to_hex().to_string();
        let image = tokio::task::spawn_blocking(move || {
            load_for_prompt_bytes(
                Path::new("browser.screenshot"),
                bytes,
                PromptImageMode::Original,
            )
        })
        .await
        .map_err(|error| format!("browser.screenshot image decoder failed: {error}"))?
        .map_err(|error| format!("browser.screenshot returned an invalid image: {error}"))?;
        if image.width == 0
            || image.height == 0
            || image.mime != format!("image/{format}")
            || blake3::hash(&image.bytes).to_hex().as_str() != content_hash
        {
            return Err(
                "browser.screenshot image format or original bytes did not match the capture"
                    .into(),
            );
        }
        let after = CaptureObservation::read(session).await?;
        if current_document(session).await? != document
            || revision != session.observation.document_revision
            || before != after
        {
            continue;
        }
        let coordinates = match &rect {
            Some(rect) => json!({
                "image_space":"encoded_image_pixels",
                "mapping_status":"explicit_cdp_clip",
                "mapped_space":"document_css",
                "document_css_origin":{"x":rect.x,"y":rect.y},
                "document_css_per_image_pixel":{"x":rect.width / f64::from(image.width),"y":rect.height / f64::from(image.height)}
            }),
            None => json!({
                "image_space":"encoded_image_pixels",
                "mapping_status":"unavailable_unclipped_surface"
            }),
        };
        let geometry = json!({
            "schema_version":1,
            "document":{"frame_id":document.frame_id,"loader_id":document.loader_id,"document_revision":revision},
            "image":{"width":image.width,"height":image.height,"media_type":image.mime,"content_hash":content_hash},
            "capture":{
                "scope":if requested_clip.is_some() { "clip" } else if full_page { "full_page" } else { "viewport" },
                "pixel_mode":if css_pixels { "css" } else { "device" },
                "document_css_rect":rect,
                "clip":clip,
                "from_surface":true,
                "capture_beyond_viewport":beyond_viewport
            },
            "observed":{
                "css_layout_viewport":before.metrics.css_layout_viewport,
                "css_visual_viewport":before.metrics.css_visual_viewport,
                "css_content_size":before.metrics.css_content_size,
                "device_pixel_ratio":before.device_pixel_ratio
            },
            "coordinates":coordinates
        });
        return Ok(CapturedScreenshot {
            image,
            payload_base64: encoded.to_string(),
            content_hash,
            geometry,
            url: before.url,
            title: before.title,
        });
    }
    Err(
        "browser.screenshot document or metrics changed during capture; no image was published"
            .into(),
    )
}
