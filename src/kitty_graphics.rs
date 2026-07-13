use base64::Engine;
use std::collections::HashMap;

const MAX_KITTY_IMAGES: usize = 100;
const MAX_KITTY_CACHE_MB: u64 = 256;
const MAX_KITTY_TRANSFER_BYTES: usize = 64 * 1024 * 1024;
const MAX_KITTY_DIMENSION: u32 = 8192;
const MAX_KITTY_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_KITTY_PLACEMENTS: usize = 1024;
const KITTY_PENDING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 图像格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Rgb,
    Rgba,
}

impl ImageFormat {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "100" | "png" => Some(ImageFormat::Png),
            "jpeg" | "jpg" => Some(ImageFormat::Jpeg),
            "webp" => Some(ImageFormat::Webp),
            "24" | "rgb" => Some(ImageFormat::Rgb),
            "32" | "rgba" => Some(ImageFormat::Rgba),
            _ => None,
        }
    }
}

/// Kitty 图像
#[derive(Debug, Clone)]
pub struct KittyImage {
    /// Monotonic content version used by the renderer cache. Re-transmitting an
    /// id must invalidate a same-sized GPU texture too.
    pub generation: u64,
    #[allow(dead_code)]
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // 原始或解码后的图像数据
}

/// Kitty 图像放置
#[derive(Debug, Clone)]
pub struct KittyPlacement {
    pub image_id: u32,
    pub placement_id: Option<u32>,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub z_index: i32,
}

/// Kitty 图像协议参数
#[derive(Debug, Default)]
pub struct KittyGraphicsParams {
    pub action: Option<String>,    // a: t=transfer, d=delete, p=place, q=query
    pub image_id: Option<u32>,     // i
    pub image_number: Option<u32>, // I
    pub placement_id: Option<u32>, // p
    pub format: Option<String>,    // f: 24=RGB, 32=RGBA, 100=PNG; text for legacy
    pub width: Option<u32>,        // s
    pub height: Option<u32>,       // v
    pub columns: Option<u32>,      // c: placement width in terminal cells
    pub rows: Option<u32>,         // r: placement height in terminal cells
    pub x: Option<u32>,            // x: column
    pub y: Option<u32>,            // y: row
    pub z: Option<i32>,            // z: z-order
    pub more: bool,                // m: 1=more data, 0=last
    pub data: Option<String>,      // base64 encoded data
}

/// 待传输的图像数据
#[allow(dead_code)]
pub struct PendingTransfer {
    pub image_id: u32,
    pub format: ImageFormat,
    pub chunks: Vec<Vec<u8>>,
    pub bytes: usize,
    pub width: Option<u32>,
    pub height: Option<u32>,
    auto_placement: Option<PlacementRequest>,
    started_at: std::time::Instant,
}

#[derive(Debug, Clone, Copy)]
struct PlacementRequest {
    placement_id: Option<u32>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    z: i32,
}

impl PlacementRequest {
    fn from_params(params: &KittyGraphicsParams) -> Self {
        Self {
            placement_id: params.placement_id,
            x: params.x.unwrap_or(0),
            y: params.y.unwrap_or(0),
            width: params.columns.unwrap_or(1),
            height: params.rows.unwrap_or(1),
            z: params.z.unwrap_or(0),
        }
    }
}

/// Kitty 图像协议状态管理
pub struct KittyGraphicsState {
    images: HashMap<u32, KittyImage>,
    placements: Vec<KittyPlacement>,
    pending_transfer: Option<PendingTransfer>,
    next_placement_id: u32,
    next_generation: u64,
    total_decoded: u32,
    total_bytes_processed: u64,
    total_image_memory: u64,
    access_order: std::collections::VecDeque<u32>,
}

impl KittyGraphicsState {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            placements: Vec::new(),
            pending_transfer: None,
            next_placement_id: 1,
            next_generation: 1,
            total_decoded: 0,
            total_bytes_processed: 0,
            total_image_memory: 0,
            access_order: std::collections::VecDeque::new(),
        }
    }

    fn enforce_image_limits(&mut self) {
        while self.images.len() > MAX_KITTY_IMAGES
            || self.total_image_memory > MAX_KITTY_CACHE_MB * 1024 * 1024
        {
            if let Some(oldest_id) = self.access_order.pop_front() {
                if let Some(img) = self.images.remove(&oldest_id) {
                    self.total_image_memory -= img.data.len() as u64;
                    self.placements.retain(|p| p.image_id != oldest_id);
                }
            } else {
                break;
            }
        }
    }

    /// 解析 Kitty 图像协议的 APC/DCS 数据
    pub fn parse_graphics_payload(&mut self, payload: &str) -> Result<(), String> {
        self.expire_pending_transfer();
        let params = Self::parse_params(payload)?;

        match params.action.as_deref() {
            Some("t") | Some("T") => self.handle_transfer(params),
            Some("p") => self.handle_placement(params),
            Some("d") => self.handle_delete(params),
            Some("q") => self.handle_query(params),
            // Continuation chunks of a multi-chunk transfer carry only `m=`/data
            // with no `a=` key, so route them to the transfer handler when a
            // transfer is already in progress.
            None if self.pending_transfer.is_some() => self.handle_transfer(params),
            _ => Err("Unknown action".to_string()),
        }
    }

    /// 解析参数字符串
    fn parse_params(payload: &str) -> Result<KittyGraphicsParams, String> {
        let mut params = KittyGraphicsParams::default();

        // The standard wire form is `G<comma-separated controls>;<base64>`.
        // Older jterm3 tests and callers used semicolons between every field,
        // so retain that grammar as a compatibility path.
        let (payload, has_apc_marker) = payload
            .strip_prefix('G')
            .map_or((payload, false), |rest| (rest, true));
        let control_end = payload.find(';').unwrap_or(payload.len());
        let standard = has_apc_marker || payload[..control_end].contains(',');

        if standard {
            let (controls, data) = payload
                .split_once(';')
                .map_or((payload, None), |(controls, data)| (controls, Some(data)));
            for pair in controls.split(',').filter(|pair| !pair.is_empty()) {
                let (key, value) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("Invalid Kitty control field: {pair}"))?;
                Self::parse_control(&mut params, key, value)?;
            }
            if let Some(data) = data.filter(|data| !data.is_empty()) {
                params.data = Some(data.to_string());
            }
        } else {
            for pair in payload.split(';').filter(|pair| !pair.is_empty()) {
                let Some((key, value)) = pair.split_once('=') else {
                    params.data = Some(pair.to_string());
                    continue;
                };
                if Self::is_known_control(key) {
                    Self::parse_control(&mut params, key, value)?;
                } else {
                    // Base64 padding includes `=`; preserve the complete legacy
                    // data segment instead of interpreting it as an unknown key.
                    params.data = Some(pair.to_string());
                }
            }
        }

        Ok(params)
    }

    fn is_known_control(key: &str) -> bool {
        matches!(
            key,
            "a" | "i" | "I" | "p" | "f" | "s" | "v" | "c" | "r" | "x" | "y" | "z" | "m"
        )
    }

    fn parse_control(
        params: &mut KittyGraphicsParams,
        key: &str,
        value: &str,
    ) -> Result<(), String> {
        fn unsigned(key: &str, value: &str) -> Result<u32, String> {
            value
                .parse()
                .map_err(|_| format!("Invalid numeric value for {key}: {value}"))
        }

        match key {
            "a" => params.action = Some(value.to_string()),
            "i" => params.image_id = Some(unsigned(key, value)?),
            "I" => params.image_number = Some(unsigned(key, value)?),
            "p" => params.placement_id = Some(unsigned(key, value)?),
            "f" => params.format = Some(value.to_string()),
            "s" => params.width = Some(unsigned(key, value)?),
            "v" => params.height = Some(unsigned(key, value)?),
            "c" => params.columns = Some(unsigned(key, value)?),
            "r" => params.rows = Some(unsigned(key, value)?),
            "x" => params.x = Some(unsigned(key, value)?),
            "y" => params.y = Some(unsigned(key, value)?),
            "z" => {
                params.z = Some(
                    value
                        .parse()
                        .map_err(|_| format!("Invalid numeric value for {key}: {value}"))?,
                );
            }
            "m" => match value {
                "0" => params.more = false,
                "1" => params.more = true,
                _ => return Err(format!("Invalid continuation flag: {value}")),
            },
            // Unknown standard controls are intentionally ignored so newer
            // protocol extensions remain forward compatible.
            _ => {}
        }
        Ok(())
    }

    /// 处理传输操作 (a=t)
    fn handle_transfer(&mut self, params: KittyGraphicsParams) -> Result<(), String> {
        // Continuations omit `a=`. An explicit transfer starts a new transaction
        // and therefore aborts any stale/incomplete one, allowing clean retries.
        if params.action.is_some() && self.pending_transfer.is_some() {
            self.pending_transfer = None;
        }
        let requested_placement =
            (params.action.as_deref() == Some("T")).then(|| PlacementRequest::from_params(&params));
        // Continuation chunks omit `i=`; fall back to the in-progress transfer's id.
        let image_id = params
            .image_id
            .or_else(|| self.pending_transfer.as_ref().map(|p| p.image_id))
            .ok_or("Missing image ID")?;
        let format = match params.format.as_deref() {
            Some(value) => {
                ImageFormat::from_str(value).ok_or_else(|| format!("Unknown format: {value}"))?
            }
            None => self
                .pending_transfer
                .as_ref()
                .map(|pending| pending.format)
                .unwrap_or(ImageFormat::Png),
        };

        // 解码 base64 数据
        let data = if let Some(encoded) = params.data {
            let max_encoded = MAX_KITTY_TRANSFER_BYTES.saturating_mul(4) / 3 + 4;
            if encoded.len() > max_encoded {
                self.pending_transfer = None;
                return Err("Kitty image transfer exceeds per-image limit".to_string());
            }
            let engine = base64::engine::general_purpose::STANDARD;
            match engine.decode(&encoded) {
                Ok(data) => data,
                Err(error) => {
                    self.pending_transfer = None;
                    return Err(format!("Base64 decode error: {error}"));
                }
            }
        } else {
            self.pending_transfer = None;
            return Err("No image data provided".to_string());
        };

        if params.more {
            // 分块传输，需要缓存
            if let Some(pending) = self.pending_transfer.as_ref() {
                if pending.image_id != image_id || pending.format != format {
                    self.pending_transfer = None;
                    return Err("Kitty continuation does not match pending transfer".to_string());
                }
                let within_limit = pending
                    .bytes
                    .checked_add(data.len())
                    .is_some_and(|bytes| bytes <= MAX_KITTY_TRANSFER_BYTES);
                if !within_limit {
                    self.pending_transfer = None;
                    return Err("Kitty image transfer exceeds per-image limit".to_string());
                }
            }
            let pending = self.pending_transfer.get_or_insert(PendingTransfer {
                image_id,
                format,
                chunks: Vec::new(),
                bytes: 0,
                width: params.width,
                height: params.height,
                auto_placement: requested_placement,
                started_at: std::time::Instant::now(),
            });
            if pending.auto_placement.is_none() {
                pending.auto_placement = requested_placement;
            }
            pending.bytes += data.len();
            pending.chunks.push(data);
        } else {
            // 最后一块或单块传输
            let pending = self.pending_transfer.take();

            // 合并所有块
            let (mut final_data, format, width_hint, height_hint, auto_placement) =
                if let Some(pending) = pending {
                    if pending.image_id != image_id || pending.format != format {
                        return Err(
                            "Kitty continuation does not match pending transfer".to_string()
                        );
                    }
                    let total = pending
                        .bytes
                        .checked_add(data.len())
                        .filter(|&bytes| bytes <= MAX_KITTY_TRANSFER_BYTES)
                        .ok_or("Kitty image transfer exceeds per-image limit")?;
                    let mut combined = Vec::with_capacity(total);
                    for chunk in pending.chunks {
                        combined.extend_from_slice(&chunk);
                    }
                    combined.extend_from_slice(&data);
                    (
                        combined,
                        pending.format,
                        params.width.or(pending.width),
                        params.height.or(pending.height),
                        requested_placement.or(pending.auto_placement),
                    )
                } else {
                    (
                        data,
                        format,
                        params.width,
                        params.height,
                        requested_placement,
                    )
                };

            // 获取或计算图像尺寸
            let (width, height) = match format {
                ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Webp => {
                    // 对于压缩格式，先解码以获取尺寸
                    let (decoded_data, w, h) = self.decode_compressed_image(final_data, format)?;
                    final_data = decoded_data;
                    (w, h)
                }
                ImageFormat::Rgb | ImageFormat::Rgba => {
                    // 对于原始格式，必须从参数获取尺寸
                    let w = width_hint.ok_or("Missing width for raw image format")?;
                    let h = height_hint.ok_or("Missing height for raw image format")?;
                    Self::validate_dimensions(w, h)?;
                    let pixels = (w as usize)
                        .checked_mul(h as usize)
                        .ok_or("Raw image dimensions overflow")?;
                    let channels = if format == ImageFormat::Rgb { 3 } else { 4 };
                    let expected = pixels
                        .checked_mul(channels)
                        .ok_or("Raw image byte length overflow")?;
                    if final_data.len() != expected {
                        return Err(format!(
                            "Raw image length mismatch: expected {expected}, got {}",
                            final_data.len()
                        ));
                    }
                    if format == ImageFormat::Rgb {
                        let mut rgba = Vec::with_capacity(pixels * 4);
                        for rgb in final_data.chunks_exact(3) {
                            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                        }
                        final_data = rgba;
                    }
                    (w, h)
                }
            };

            let data_size = final_data.len() as u64;
            self.total_decoded += 1;
            self.total_bytes_processed += data_size;
            // Re-transmitting an existing id replaces the old image: drop its
            // memory and its stale access-order entry so the counter doesn't
            // drift (and later underflow in enforce_image_limits).
            if let Some(old) = self.images.get(&image_id) {
                self.total_image_memory = self
                    .total_image_memory
                    .saturating_sub(old.data.len() as u64);
                self.access_order.retain(|&id| id != image_id);
            }
            self.total_image_memory += data_size;
            self.access_order.push_back(image_id);

            let generation = self.next_generation;
            self.next_generation = self.next_generation.wrapping_add(1).max(1);
            self.images.insert(
                image_id,
                KittyImage {
                    generation,
                    format,
                    width,
                    height,
                    data: final_data,
                },
            );

            self.enforce_image_limits();

            if let Some(placement) = auto_placement {
                self.add_placement(image_id, placement);
            }

            log::info!("[KITTY_GRAPHICS] Stored image {} ({}x{}) format: {:?} | Stats: {} images, {}MB total",
                image_id, width, height, format, self.images.len(), self.total_bytes_processed / 1_000_000);
        }

        Ok(())
    }

    /// 解码压缩图像格式（PNG/JPEG），返回 (RGBA数据, 宽度, 高度)
    fn decode_compressed_image(
        &self,
        data: Vec<u8>,
        format: ImageFormat,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        if data.len() > MAX_KITTY_TRANSFER_BYTES {
            return Err("Compressed image exceeds per-image limit".to_string());
        }
        let cursor = std::io::Cursor::new(data);
        let mut reader = image::ImageReader::new(cursor)
            .with_guessed_format()
            .map_err(|e| format!("Failed to detect image format: {e}"))?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_KITTY_DIMENSION);
        limits.max_image_height = Some(MAX_KITTY_DIMENSION);
        limits.max_alloc = Some(MAX_KITTY_TRANSFER_BYTES as u64);
        reader.limits(limits);
        let img = reader
            .decode()
            .map_err(|e| format!("Failed to load image: {e}"))?;

        let width = img.width();
        let height = img.height();
        Self::validate_dimensions(width, height)?;
        let rgba_image = img.to_rgba8();

        log::debug!(
            "[KITTY_GRAPHICS] Decoded {:?} image {}x{} -> RGBA {}B",
            format,
            width,
            height,
            rgba_image.len()
        );

        Ok((rgba_image.into_raw(), width, height))
    }

    fn validate_dimensions(width: u32, height: u32) -> Result<(), String> {
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or("Image dimensions overflow")?;
        if width == 0
            || height == 0
            || width > MAX_KITTY_DIMENSION
            || height > MAX_KITTY_DIMENSION
            || pixels > MAX_KITTY_PIXELS
        {
            return Err(format!("Image dimensions exceed limit: {width}x{height}"));
        }
        Ok(())
    }

    /// 处理放置操作 (a=p)
    fn handle_placement(&mut self, params: KittyGraphicsParams) -> Result<(), String> {
        let image_id = params.image_id.ok_or("Missing image ID")?;
        let placement = PlacementRequest {
            placement_id: params.placement_id,
            x: params.x.unwrap_or(0),
            y: params.y.unwrap_or(0),
            width: params.columns.or(params.width).unwrap_or(1),
            height: params.rows.or(params.height).unwrap_or(1),
            z: params.z.unwrap_or(0),
        };
        self.add_placement(image_id, placement);
        Ok(())
    }

    fn add_placement(&mut self, image_id: u32, placement: PlacementRequest) {
        let placement_id = placement.placement_id.or_else(|| {
            let id = self.next_placement_id;
            self.next_placement_id += 1;
            Some(id)
        });

        self.placements.push(KittyPlacement {
            image_id,
            placement_id,
            x: placement.x,
            y: placement.y,
            width: placement.width,
            height: placement.height,
            z_index: placement.z,
        });

        if self.placements.len() > MAX_KITTY_PLACEMENTS {
            let excess = self.placements.len() - MAX_KITTY_PLACEMENTS;
            self.placements.drain(0..excess);
        }

        // 按 z-order 排序
        self.placements.sort_by_key(|p| p.z_index);

        log::info!(
            "[KITTY_GRAPHICS] Placed image {} at ({},{}) size {}x{} z={}",
            image_id,
            placement.x,
            placement.y,
            placement.width,
            placement.height,
            placement.z
        );
    }

    /// 处理删除操作 (a=d)
    fn handle_delete(&mut self, params: KittyGraphicsParams) -> Result<(), String> {
        if let Some(image_id) = params.image_id {
            if let Some(img) = self.images.remove(&image_id) {
                self.total_image_memory -= img.data.len() as u64;
            }
            self.placements.retain(|p| p.image_id != image_id);
            self.access_order.retain(|&id| id != image_id);
            log::info!("[KITTY_GRAPHICS] Deleted image {}", image_id);
        } else if let Some(placement_id) = params.placement_id {
            self.placements
                .retain(|p| p.placement_id != Some(placement_id));
            log::info!("[KITTY_GRAPHICS] Deleted placement {}", placement_id);
        } else {
            return Err("Missing image_id or placement_id for delete".to_string());
        }

        Ok(())
    }

    /// 处理查询操作 (a=q)
    fn handle_query(&mut self, _params: KittyGraphicsParams) -> Result<(), String> {
        // 返回支持的格式
        // ESC_DCS ? kitty 0 ; png ; jpeg ; rgb ; rgba ESC_ST
        let response = "\x1bP?kitty 0;png;jpeg;rgb;rgba\x1b\\";
        log::info!("[KITTY_GRAPHICS] Query response: {}", response);
        // 实际应用中需要将此回复发送给应用程序
        Ok(())
    }

    /// 获取性能统计
    #[allow(dead_code)]
    pub fn get_stats(&self) -> (u32, u64, usize) {
        (
            self.total_decoded,
            self.total_bytes_processed,
            self.images.len(),
        )
    }

    /// 获取所有放置
    pub fn get_placements(&self) -> &[KittyPlacement] {
        &self.placements
    }

    /// 获取图像
    pub fn get_image(&self, id: u32) -> Option<&KittyImage> {
        self.images.get(&id)
    }

    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    pub fn image_memory_mb(&self) -> u64 {
        self.total_image_memory / 1_000_000
    }

    pub fn expire_pending_transfer(&mut self) {
        if self
            .pending_transfer
            .as_ref()
            .is_some_and(|pending| pending.started_at.elapsed() >= KITTY_PENDING_TIMEOUT)
        {
            self.pending_transfer = None;
        }
    }

    /// 清除所有数据
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.images.clear();
        self.placements.clear();
        self.pending_transfer = None;
        self.total_decoded = 0;
        self.total_bytes_processed = 0;
        self.total_image_memory = 0;
        self.access_order.clear();
    }
}

impl Default for KittyGraphicsState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_graphics_params() {
        let payload = "a=t;i=1;s=100;v=100;f=png";
        let params = KittyGraphicsState::parse_params(payload).unwrap();
        assert_eq!(params.action.as_deref(), Some("t"));
        assert_eq!(params.image_id, Some(1));
        assert_eq!(params.width, Some(100));
        assert_eq!(params.height, Some(100));
        assert_eq!(params.format.as_deref(), Some("png"));
    }

    #[test]
    fn parses_standard_apc_controls_and_data() {
        let params = KittyGraphicsState::parse_params("Gf=32,s=1,v=1,a=T,i=7;AQIDBA==").unwrap();

        assert_eq!(params.action.as_deref(), Some("T"));
        assert_eq!(params.image_id, Some(7));
        assert_eq!(params.width, Some(1));
        assert_eq!(params.height, Some(1));
        assert_eq!(params.format.as_deref(), Some("32"));
        assert_eq!(params.data.as_deref(), Some("AQIDBA=="));
    }

    #[test]
    fn numeric_formats_follow_the_kitty_protocol() {
        assert_eq!(ImageFormat::from_str("24"), Some(ImageFormat::Rgb));
        assert_eq!(ImageFormat::from_str("32"), Some(ImageFormat::Rgba));
        assert_eq!(ImageFormat::from_str("100"), Some(ImageFormat::Png));
    }

    #[test]
    fn test_placement_ordering() {
        let mut state = KittyGraphicsState::new();
        state.placements.push(KittyPlacement {
            image_id: 1,
            placement_id: None,
            x: 0,
            y: 0,
            width: 10,
            height: 10,
            z_index: 5,
        });
        state.placements.push(KittyPlacement {
            image_id: 2,
            placement_id: None,
            x: 10,
            y: 10,
            width: 10,
            height: 10,
            z_index: -1,
        });

        // Sort by z_index
        state.placements.sort_by_key(|p| p.z_index);

        assert_eq!(state.placements[0].z_index, -1);
        assert_eq!(state.placements[1].z_index, 5);
    }

    #[test]
    fn test_complete_kitty_workflow() {
        let mut state = KittyGraphicsState::new();

        // Create a simple 2x2 RGBA image (red square)
        // 4 pixels * 4 bytes (RGBA) = 16 bytes
        let mut image_data = Vec::new();
        for _ in 0..4 {
            image_data.extend_from_slice(&[255, 0, 0, 255]); // Red pixel RGBA
        }

        // Encode to base64
        let base64_data = base64::engine::general_purpose::STANDARD.encode(&image_data);

        // The legacy semicolon-delimited syntax remains supported.
        let payload = format!("a=t;i=1;s=2;v=2;f=rgba;m=0;{}", base64_data);
        state.parse_graphics_payload(&payload).unwrap();

        let image = state.get_image(1).unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.data, image_data);
    }

    #[test]
    fn standard_rgba_transmit_and_display_creates_placement() {
        let mut state = KittyGraphicsState::new();
        let data = [1, 2, 3, 4];
        let encoded = base64::engine::general_purpose::STANDARD.encode(data);

        state
            .parse_graphics_payload(&format!("Gf=32,s=1,v=1,a=T,i=1;{encoded}"))
            .unwrap();

        let image = state.get_image(1).unwrap();
        assert_eq!(image.format, ImageFormat::Rgba);
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.data, data);
        assert_eq!(state.get_placements().len(), 1);
        let placement = &state.get_placements()[0];
        assert_eq!(placement.image_id, 1);
        assert_eq!((placement.width, placement.height), (1, 1));
    }

    #[test]
    fn raw_rgb_is_expanded_to_rgba() {
        let mut state = KittyGraphicsState::new();
        let rgb = [10, 20, 30, 40, 50, 60];
        let encoded = base64::engine::general_purpose::STANDARD.encode(rgb);

        state
            .parse_graphics_payload(&format!("Gf=24,s=2,v=1,a=t,i=2;{encoded}"))
            .unwrap();

        assert_eq!(
            state.get_image(2).unwrap().data,
            [10, 20, 30, 255, 40, 50, 60, 255]
        );
    }

    #[test]
    fn raw_transfer_rejects_invalid_dimensions_and_lengths() {
        let mut state = KittyGraphicsState::new();
        let rgba = base64::engine::general_purpose::STANDARD.encode([1, 2, 3, 4]);
        let error = state
            .parse_graphics_payload(&format!("Gf=32,s=0,v=1,a=t,i=3;{rgba}"))
            .unwrap_err();
        assert!(error.contains("dimensions exceed limit"));
        assert!(state.get_image(3).is_none());

        let short = base64::engine::general_purpose::STANDARD.encode([1, 2, 3]);
        let error = state
            .parse_graphics_payload(&format!("Gf=32,s=1,v=1,a=t,i=4;{short}"))
            .unwrap_err();
        assert!(error.contains("length mismatch"));
        assert!(state.get_image(4).is_none());
    }

    #[test]
    fn standard_continuation_inherits_identity_and_auto_placement() {
        let mut state = KittyGraphicsState::new();
        let first = base64::engine::general_purpose::STANDARD.encode([1, 2]);
        let last = base64::engine::general_purpose::STANDARD.encode([3, 4]);

        state
            .parse_graphics_payload(&format!("Gf=32,s=1,v=1,a=T,i=9,m=1;{first}"))
            .unwrap();
        assert!(state.get_image(9).is_none());
        state
            .parse_graphics_payload(&format!("Gm=0;{last}"))
            .unwrap();

        assert_eq!(state.get_image(9).unwrap().data, [1, 2, 3, 4]);
        assert_eq!(state.get_placements().len(), 1);
        assert_eq!(state.get_placements()[0].image_id, 9);
        assert!(state.pending_transfer.is_none());
    }

    #[test]
    fn mismatched_continuation_is_rejected_and_discarded() {
        let mut state = KittyGraphicsState::new();
        let first = base64::engine::general_purpose::STANDARD.encode([1, 2]);
        let last = base64::engine::general_purpose::STANDARD.encode([3, 4]);

        state
            .parse_graphics_payload(&format!("Gf=32,s=1,v=1,a=t,i=10,m=1;{first}"))
            .unwrap();
        let error = state
            .parse_graphics_payload(&format!("Gi=11,m=0;{last}"))
            .unwrap_err();

        assert!(error.contains("does not match"));
        assert!(state.pending_transfer.is_none());
        assert!(state.get_image(10).is_none());
        assert!(state.get_image(11).is_none());
    }

    #[test]
    fn failed_continuation_is_cleared_and_can_be_retried() {
        let mut state = KittyGraphicsState::new();
        let first = base64::engine::general_purpose::STANDARD.encode([1, 2]);
        state
            .parse_graphics_payload(&format!("Gf=32,s=1,v=1,a=t,i=12,m=1;{first}"))
            .unwrap();

        assert!(state.parse_graphics_payload("Gm=0;%%%bad%%%").is_err());
        assert!(state.pending_transfer.is_none());

        let complete = base64::engine::general_purpose::STANDARD.encode([1, 2, 3, 4]);
        state
            .parse_graphics_payload(&format!("Gf=32,s=1,v=1,a=t,i=12;{complete}"))
            .unwrap();
        assert_eq!(state.get_image(12).unwrap().data, [1, 2, 3, 4]);
    }

    #[test]
    fn stale_pending_transfer_expires() {
        let mut state = KittyGraphicsState::new();
        let first = base64::engine::general_purpose::STANDARD.encode([1, 2]);
        state
            .parse_graphics_payload(&format!("Gf=32,s=1,v=1,a=t,i=13,m=1;{first}"))
            .unwrap();
        state.pending_transfer.as_mut().unwrap().started_at =
            std::time::Instant::now() - KITTY_PENDING_TIMEOUT;

        state.expire_pending_transfer();

        assert!(state.pending_transfer.is_none());
    }
}
